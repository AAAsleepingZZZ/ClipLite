//! 前端可调用的 Tauri 命令（IPC 契约：命令名与参数名固定，勿改签名）。

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{self, Deserialize};
use tauri::{AppHandle, Manager, State};

use crate::clipboard::Clipboard;
use crate::paste;
use crate::settings::{self, Settings, SettingsState, MAX_IMAGE_MB_MAX, MAX_IMAGE_MB_MIN, MAX_ITEMS_MAX, MAX_ITEMS_MIN};
use crate::store::{Item, Snippet, Store};

/// 查询历史条目，query 为空时返回全部
#[tauri::command]
pub fn get_items(
    state: State<'_, Arc<Mutex<Store>>>,
    query: Option<String>,
) -> Result<Vec<Item>, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.list(query.as_deref()).map_err(|e| e.to_string())
}

/// 复制指定条目到系统剪贴板
#[tauri::command]
pub fn copy_item(
    state: State<'_, Arc<Mutex<Store>>>,
    clipboard: State<'_, Arc<Clipboard>>,
    id: i64,
) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    copy_to_clipboard(&store, &clipboard, id)
}

/// 复制指定条目并模拟 Ctrl+V 粘贴到用户原来的窗口
#[tauri::command]
pub fn paste_item(
    app: AppHandle,
    state: State<'_, Arc<Mutex<Store>>>,
    clipboard: State<'_, Arc<Clipboard>>,
    id: i64,
) -> Result<(), String> {
    {
        let store = state.lock().map_err(|e| e.to_string())?;
        copy_to_clipboard(&store, &clipboard, id)?;
    }
    // 先收起面板并归还焦点，让 Ctrl+V 落到用户原来的窗口
    crate::hide_panel_with_focus(&app);
    std::thread::sleep(std::time::Duration::from_millis(120));
    // 复制完成后先释放存储锁，再执行按键模拟
    paste::paste()?;
    // 整个粘贴流程成功后再次标记自身写入，确保监听线程不回环
    clipboard.mark_self_write();
    Ok(())
}

/// 切换条目的置顶状态，返回更新后的条目
#[tauri::command]
pub fn toggle_pin(
    state: State<'_, Arc<Mutex<Store>>>,
    id: i64,
) -> Result<Item, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.toggle_pin(id).map_err(|e| e.to_string())
}

/// 删除指定条目
#[tauri::command]
pub fn delete_item(state: State<'_, Arc<Mutex<Store>>>, id: i64) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.delete(id).map_err(|e| e.to_string())
}

/// 清空全部历史
#[tauri::command]
pub fn clear_history(state: State<'_, Arc<Mutex<Store>>>) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.clear().map_err(|e| e.to_string())
}

/// 读取图片条目对应的 PNG 文件，返回 base64 data URL
#[tauri::command]
pub fn get_image(state: State<'_, Arc<Mutex<Store>>>, id: i64) -> Result<String, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    let item = store.get_item(id).map_err(|e| e.to_string())?;
    let path = item.image_path.ok_or_else(|| "该条目不是图片".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败: {e}"))?;
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(&bytes)))
}

/// 文件条目是图片文件时，生成缩略图（128x128 方形裁剪）返回 base64 data URL。
/// 非图片文件 / 解码失败返回错误，由前端保持文件图标。
#[tauri::command]
pub fn get_file_thumb(state: State<'_, Arc<Mutex<Store>>>, id: i64) -> Result<String, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    let item = store.get_item(id).map_err(|e| e.to_string())?;
    if item.kind != "file" {
        return Err("该条目不是文件".to_string());
    }
    let path = item
        .content
        .as_deref()
        .and_then(|c| c.lines().find(|l| !l.trim().is_empty()))
        .ok_or_else(|| "文件条目为空".to_string())?;
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str()) {
        return Err("不是图片文件".to_string());
    }
    // 解码并等比缩放为 128x128 方形缩略图（cover 裁剪）
    let img = image::open(path).map_err(|e| format!("解码图片失败: {e}"))?;
    let thumb = img.resize_to_fill(128, 128, image::imageops::FilterType::Triangle);
    let thumb = thumb.to_rgba8();
    let mut buf = Vec::new();
    {
        use image::codecs::png::PngEncoder;
        use image::{ExtendedColorType, ImageEncoder};
        let mut cursor = std::io::Cursor::new(&mut buf);
        PngEncoder::new(&mut cursor)
            .write_image(&thumb, 128, 128, ExtendedColorType::Rgba8)
            .map_err(|e| format!("缩略图编码失败: {e}"))?;
    }
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(&buf)))
}

/// 读取当前设置
#[tauri::command]
pub fn get_settings(state: State<'_, SettingsState>) -> Result<Settings, String> {
    let s = state.settings.lock().map_err(|e| e.to_string())?;
    Ok(s.clone())
}

/// 更新设置（仅更新传入的字段）。
/// 热键变更：先注册新键、成功后再注销旧键，注册失败返回错误由前端提示。
#[tauri::command]
pub fn update_settings(
    app: AppHandle,
    state: State<'_, SettingsState>,
    settings: UpdateSettings,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let config_dir = state.config_dir.clone();
    let mut cur = state.settings.lock().map_err(|e| e.to_string())?;

    // 热键变更
    if let Some(new_hotkey) = settings.hotkey {
        if new_hotkey != cur.hotkey {
            let shortcut = settings::parse_hotkey(&new_hotkey)?;
            if let Err(e) = app.global_shortcut().register(shortcut) {
                return Err(format!("热键注册失败：{e}"));
            }
            if let Ok(old) = settings::parse_hotkey(&cur.hotkey) {
                let _ = app.global_shortcut().unregister(old);
            }
            cur.hotkey = new_hotkey;
        }
    }

    // 退出时清空
    if let Some(v) = settings.clear_on_exit {
        cur.clear_on_exit = v;
    }

    // 开机自启
    if let Some(v) = settings.autostart {
        if v != cur.autostart {
            let result = if v {
                app.autolaunch().enable()
            } else {
                app.autolaunch().disable()
            };
            if let Err(e) = result {
                return Err(format!("设置开机自启失败：{e}"));
            }
            cur.autostart = v;
        }
    }

    // 毛玻璃透出强度（染色层不透明度）
    if let Some(v) = settings.glass_alpha {
        cur.glass_alpha = v.clamp(0.30, 0.62);
    }

    // 历史条数上限（同步到 Store，立即生效）
    if let Some(v) = settings.max_items {
        cur.max_items = v.clamp(MAX_ITEMS_MIN, MAX_ITEMS_MAX);
        if let Some(store_state) = app.try_state::<Arc<Mutex<Store>>>() {
            if let Ok(mut store) = store_state.lock() {
                store.set_max_items(cur.max_items);
            }
        }
    }

    // 图片大小上限（MB）
    if let Some(v) = settings.max_image_mb {
        cur.max_image_mb = v.clamp(MAX_IMAGE_MB_MIN, MAX_IMAGE_MB_MAX);
    }

    settings::save_settings(&config_dir, &cur)?;
    Ok(())
}

/// 隐藏主面板，并把焦点归还给面板出现前的窗口
#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    crate::hide_panel_with_focus(&app);
}

/// 查询全部片段（分组由前端聚合）
#[tauri::command]
pub fn get_snippets(state: State<'_, Arc<Mutex<Store>>>) -> Result<Vec<Snippet>, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.list_snippets().map_err(|e| e.to_string())
}

/// 新增片段
#[tauri::command]
pub fn add_snippet(
    state: State<'_, Arc<Mutex<Store>>>,
    content: String,
    title: Option<String>,
    group_name: Option<String>,
) -> Result<Snippet, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store
        .add_snippet(
            &content,
            title.as_deref(),
            group_name.as_deref().unwrap_or(crate::store::DEFAULT_GROUP),
        )
        .map_err(|e| e.to_string())
}

/// 更新片段（仅更新传入的字段）
#[tauri::command]
pub fn update_snippet(
    state: State<'_, Arc<Mutex<Store>>>,
    id: i64,
    content: Option<String>,
    title: Option<String>,
    group_name: Option<String>,
) -> Result<Snippet, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store
        .update_snippet(id, content.as_deref(), title.as_deref(), group_name.as_deref())
        .map_err(|e| e.to_string())
}

/// 删除片段
#[tauri::command]
pub fn delete_snippet(state: State<'_, Arc<Mutex<Store>>>, id: i64) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.delete_snippet(id).map_err(|e| e.to_string())
}

/// 分组重命名（目标重名时报错）
#[tauri::command]
pub fn rename_group(
    state: State<'_, Arc<Mutex<Store>>>,
    old_name: String,
    new_name: String,
) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.rename_group(&old_name, &new_name).map_err(|e| e.to_string())
}

/// 删除分组：组内片段移入「默认」组，返回迁移条数
#[tauri::command]
pub fn delete_group(state: State<'_, Arc<Mutex<Store>>>, name: String) -> Result<usize, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.delete_group(&name).map_err(|e| e.to_string())
}

/// 新建分组（空分组独立持久化，重名报错）
#[tauri::command]
pub fn add_group(state: State<'_, Arc<Mutex<Store>>>, name: String) -> Result<(), String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.add_group(&name).map_err(|e| e.to_string())
}

/// 查询全部分组名（含空分组）
#[tauri::command]
pub fn get_groups(state: State<'_, Arc<Mutex<Store>>>) -> Result<Vec<String>, String> {
    let store = state.lock().map_err(|e| e.to_string())?;
    store.list_groups().map_err(|e| e.to_string())
}

/// 复制任意文本到系统剪贴板（片段库单击复制用）
#[tauri::command]
pub fn copy_text(clipboard: State<'_, Arc<Clipboard>>, content: String) -> Result<(), String> {
    clipboard.write_text(&content)
}

/// 把任意文本写入剪贴板并模拟 Ctrl+V 粘贴到用户原来的窗口（片段库双击/回车粘贴用）
#[tauri::command]
pub fn paste_text(
    app: AppHandle,
    clipboard: State<'_, Arc<Clipboard>>,
    content: String,
) -> Result<(), String> {
    clipboard.write_text(&content)?;
    // 先收起面板并归还焦点，让 Ctrl+V 落到用户原来的窗口
    crate::hide_panel_with_focus(&app);
    std::thread::sleep(std::time::Duration::from_millis(120));
    paste::paste()?;
    // 整个粘贴流程成功后再次标记自身写入，确保监听线程不回环
    clipboard.mark_self_write();
    Ok(())
}

/// 更新设置的请求体（字段均可选：仅更新传入的字段）
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSettings {
    pub hotkey: Option<String>,
    #[serde(alias = "clearOnExit")]
    pub clear_on_exit: Option<bool>,
    pub autostart: Option<bool>,
    #[serde(alias = "glassAlpha")]
    pub glass_alpha: Option<f64>,
    #[serde(alias = "maxItems")]
    pub max_items: Option<i64>,
    #[serde(alias = "maxImageMb")]
    pub max_image_mb: Option<i64>,
}

/// 把条目内容写入系统剪贴板（文本、图片或文件列表），写成功即标记自身写入
fn copy_to_clipboard(store: &Store, clipboard: &Clipboard, id: i64) -> Result<(), String> {
    let item = store.get_item(id).map_err(|e| e.to_string())?;
    match item.kind.as_str() {
        "text" => {
            let text = item.content.ok_or_else(|| "条目内容为空".to_string())?;
            clipboard.write_text(&text)
        }
        "image" => {
            let path = item.image_path.ok_or_else(|| "该条目不是图片".to_string())?;
            let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败: {e}"))?;
            // PNG 文件解码回 RGBA 像素，再写入剪贴板
            let rgba = image::load_from_memory(&bytes)
                .map_err(|e| format!("解码图片失败: {e}"))?
                .to_rgba8();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let data = arboard::ImageData {
                width: w,
                height: h,
                bytes: Cow::Owned(rgba.into_raw()),
            };
            clipboard.write_image(&data)
        }
        "file" => {
            let content = item.content.ok_or_else(|| "条目内容为空".to_string())?;
            let paths: Vec<std::path::PathBuf> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(std::path::PathBuf::from)
                .collect();
            if paths.is_empty() {
                return Err("文件条目为空".to_string());
            }
            clipboard.write_files(paths)
        }
        other => Err(format!("未知条目类型: {other}")),
    }
}
