//! 前端可调用的 Tauri 命令（IPC 契约：命令名与参数名固定，勿改签名）。

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use tauri::{AppHandle, State};

use crate::clipboard::Clipboard;
use crate::paste;
use crate::settings::{self, Settings, SettingsState};
use crate::store::{Item, Store};

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

    settings::save_settings(&config_dir, &cur)?;
    Ok(())
}

/// 隐藏主面板，并把焦点归还给面板出现前的窗口
#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    crate::hide_panel_with_focus(&app);
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
}

/// 把条目内容写入系统剪贴板（文本或图片），写成功即标记自身写入
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
        other => Err(format!("未知条目类型: {other}")),
    }
}
