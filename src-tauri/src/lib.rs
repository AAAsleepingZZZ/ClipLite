//! ClipLite 剪贴板管理器 —— 后端入口：插件组装、命令注册、监听线程与窗口行为。

mod capture;
mod clipboard;
mod commands;
mod listener;
mod paste;
mod settings;
mod store;
mod tray;

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, SetForegroundWindow,
};

use crate::clipboard::Clipboard;
use crate::settings::SettingsState;
use crate::store::Store;

/// 面板出现前的"前台窗口"句柄，用于面板收起时把焦点归还给用户原来的窗口
pub type FocusState = Arc<Mutex<Option<isize>>>;

/// 应用入口：组装插件、注册命令、初始化各模块。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // 开机自启能力
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        // 单实例：二次启动时呼出面板
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_panel(app);
        }))
        // 全局热键：按下时呼出面板
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        show_panel(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::get_items,
            commands::copy_item,
            commands::paste_item,
            commands::toggle_pin,
            commands::delete_item,
            commands::clear_history,
            commands::get_image,
            commands::get_settings,
            commands::update_settings,
            commands::hide_panel,
            commands::get_snippets,
            commands::add_snippet,
            commands::update_snippet,
            commands::delete_snippet,
            commands::rename_group,
            commands::delete_group,
            commands::add_group,
            commands::get_groups,
            commands::copy_text,
            commands::paste_text,
            commands::get_file_thumb,
        ])
        .setup(|app| {
            setup_app(app)?;
            Ok(())
        })
        // 失焦自动隐藏面板
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(false) = event {
                if window.label() == "main" {
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // 退出钩子：若开启"退出时清空"，则清空全部历史
            if let tauri::RunEvent::Exit = event {
                let should_clear = app_handle
                    .try_state::<SettingsState>()
                    .map(|s| s.settings.lock().map(|s| s.clear_on_exit).unwrap_or(false))
                    .unwrap_or(false);
                if should_clear {
                    if let Some(store_state) = app_handle.try_state::<Arc<Mutex<Store>>>() {
                        if let Ok(store) = store_state.lock() {
                            let _ = store.clear();
                        }
                    }
                }
            }
        });
}

/// setup 阶段：初始化存储 / 设置 / 剪贴板 / 窗口效果 / 托盘 / 热键 / 监听线程
fn setup_app(app: &tauri::App) -> Result<(), String> {
    // 数据目录（数据库 + 图片）
    let data_dir: PathBuf = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let store = Store::new(&data_dir).map_err(|e| e.to_string())?;
    let store = Arc::new(Mutex::new(store));
    app.manage(store.clone());

    // 设置（含配置文件目录）；Arc 共享给监听线程，容量参数实时生效
    let config_dir: PathBuf = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let settings = Arc::new(Mutex::new(settings::load_settings(&config_dir)));
    {
        let s = settings.lock().map_err(|e| e.to_string())?;
        store.lock().map_err(|e| e.to_string())?.set_max_items(s.max_items);
    }
    app.manage(SettingsState {
        config_dir,
        settings: settings.clone(),
    });

    // 剪贴板封装（共享"自身写入"标记）
    let marker = Arc::new(AtomicU64::new(0));
    let clipboard = Arc::new(Clipboard::new(marker));
    app.manage(clipboard.clone());

    // 面板打开前的前台窗口记录（收起时归还焦点）
    let focus_state: FocusState = Arc::new(Mutex::new(None));
    app.manage(focus_state);

    // 窗口毛玻璃效果：Mica → Acrylic → Blur 依次降级，全部失败则忽略
    apply_window_effects(app);

    // 系统托盘
    tray::create_tray(app.handle(), store.clone()).map_err(|e| e.to_string())?;

    // 注册全局热键；失败（如键位被其他程序占用）仅记录日志
    let hotkey_str = {
        let s = settings.lock().map_err(|e| e.to_string())?;
        s.hotkey.clone()
    };
    match settings::parse_hotkey(&hotkey_str) {
        Ok(shortcut) => {
            if let Err(e) = app.global_shortcut().register(shortcut) {
                eprintln!("[ClipLite] 全局热键注册失败: {e}");
            }
        }
        Err(e) => eprintln!("[ClipLite] 热键格式无效: {e}"),
    }

    // 按设置同步开机自启状态
    {
        use tauri_plugin_autostart::ManagerExt;
        let autostart = settings.lock().map_err(|e| e.to_string())?.autostart;
        let result = if autostart {
            app.autolaunch().enable()
        } else {
            app.autolaunch().disable()
        };
        if let Err(e) = result {
            eprintln!("[ClipLite] 同步开机自启状态失败: {e}");
        }
    }

    // 启动剪贴板监听线程
    listener::spawn(store, app.handle().clone(), clipboard, settings);

    // 命名事件触发通道：任何进程 SetEvent("ClipLite_ShowPanel") 即可呼出面板
    // （与热键同一路径，便于命令行/外部工具触发与自动化测试）
    #[cfg(target_os = "windows")]
    {
        use windows::core::w;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
        let app = app.handle().clone();
        std::thread::spawn(move || unsafe {
            eprintln!("[ClipLite] 命名事件监听线程启动");
            let evt = CreateEventW(
                None,
                false,
                false,
                w!("ClipLite_ShowPanel"),
            );
            let handle: HANDLE = match evt {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[ClipLite] 创建命名事件失败: {e}");
                    return;
                }
            };
            loop {
                // 无限等待：面板呼出事件
                if WaitForSingleObject(handle, u32::MAX)
                    == windows::Win32::Foundation::WAIT_EVENT(0)
                {
                    eprintln!("[ClipLite] 收到呼出事件");
                    show_panel(&app);
                }
            }
        });
    }

    Ok(())
}

/// 应用窗口毛玻璃/亚克力/模糊效果（依次降级，全部失败则忽略）
fn apply_window_effects(app: &tauri::App) {
    #[cfg(target_os = "windows")]
    {
        use window_vibrancy::{apply_acrylic, apply_blur, apply_mica};
        if let Some(window) = app.get_webview_window("main") {
            // Mica → Acrylic → Blur 依次降级；Mica 仅在 Windows 11 上可用
            if apply_mica(&window, None).is_err()
                && apply_acrylic(&window, Some((18, 18, 18, 125))).is_err()
            {
                let _ = apply_blur(&window, Some((18, 18, 18, 125)));
            }
        }
    }
}

/// 呼出面板：定位到光标右下方（+8px 偏移），超出工作区则向左/向上翻转，
/// 截取面板区域的桌面作为毛玻璃背景，随后显示并聚焦。
/// 面板已可见时则收起（热键切换）并归还焦点。
/// 热键、托盘"显示面板"与二次启动都调用本函数。
pub fn show_panel(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else { return };

    // 已可见 → 收起并归还焦点（热键/托盘再次触发为切换行为）
    if window.is_visible().unwrap_or(false) {
        hide_panel_with_focus(app);
        return;
    }

    let Ok(size) = window.inner_size() else { return };
    let (w, h) = (size.width as i32, size.height as i32);

    // 光标位置（物理像素）
    let mut pos = POINT::default();
    let cursor_ok = unsafe { GetCursorPos(&mut pos) }.is_ok();
    if !cursor_ok {
        pos = POINT { x: 0, y: 0 };
    }

    // 光标所在显示器的工作区（不含任务栏）
    let mut work = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if cursor_ok {
        unsafe {
            let monitor = MonitorFromPoint(pos, MONITOR_DEFAULTTONEAREST);
            if !monitor.0.is_null() {
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(monitor, &mut mi).as_bool() {
                    work = mi.rcWork;
                }
            }
        }
    }

    // 默认放在光标右下方
    let (mut x, mut y) = (pos.x + 8, pos.y + 8);
    // 工作区获取失败时兜底为当前候选区域，保证面板仍在屏幕内可见
    let right = if work.right > work.left { work.right } else { x + w };
    let bottom = if work.bottom > work.top { work.bottom } else { y + h };

    // 右侧放不下则向左翻转，下方放不下则向上翻转
    if x + w > right {
        x = pos.x - 8 - w;
    }
    if y + h > bottom {
        y = pos.y - 8 - h;
    }
    // 边界保护（极端情况下贴近工作区边缘）
    x = x.clamp(work.left, (right - w).max(work.left));
    y = y.clamp(work.top, (bottom - h).max(work.top));

    // 记录当前前台窗口：面板收起时把焦点归还给用户原来的窗口
    let fg = unsafe { GetForegroundWindow() };
    if let Some(state) = app.try_state::<FocusState>() {
        if let Ok(mut saved) = state.lock() {
            *saved = Some(fg.0 as isize);
        }
    }

    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));

    // 截取面板区域的桌面 → 前端作为毛玻璃背景（先于显示，避免白屏闪烁）
    // 注意：inner_size() 已是物理像素，勿再乘 scale_factor
    match capture::capture_region(x, y, w, h, 0.5) {
        Some(data_url) => {
            if let Err(e) = app.emit("panel-background", data_url) {
                eprintln!("[ClipLite] 推送毛玻璃背景失败: {e}");
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        None => eprintln!("[ClipLite] 截屏失败，回退纯色背景 ({x},{y} {w}x{h})"),
    }

    eprintln!("[ClipLite] 呼出: 光标=({},{}) → 面板=({},{}) 尺寸={}x{}", pos.x, pos.y, x, y, w, h);

    let _ = window.show();
    let _ = window.set_focus();
    eprintln!(
        "[ClipLite] show 后状态: visible={:?} focused={:?}",
        window.is_visible(),
        window.is_focused()
    );
}

/// 收起面板，并把焦点归还给面板出现前的那个窗口（尽力而为）。
pub fn hide_panel_with_focus(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    if let Some(state) = app.try_state::<FocusState>() {
        let saved = state.lock().ok().and_then(|s| *s);
        if let Some(hwnd) = saved {
            if hwnd != 0 {
                let _ = unsafe { SetForegroundWindow(windows::Win32::Foundation::HWND(hwnd as *mut std::ffi::c_void)) };
            }
        }
    }
}
