//! 系统托盘：显示面板 / 清空历史 / 退出。

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;

use crate::show_panel;
use crate::store::Store;

/// 创建系统托盘图标与菜单
pub fn create_tray(app: &AppHandle, store: Arc<Mutex<Store>>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示面板", true, None::<&str>)?;
    let clear = MenuItem::with_id(app, "clear", "清空历史", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &clear, &exit])?;

    let mut builder = TrayIconBuilder::with_id("cliplite-tray")
        .menu(&menu)
        // 左键单击不弹菜单（留给"呼出面板"），菜单通过右键打开
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => show_panel(app),
            "clear" => {
                if let Ok(store) = store.lock() {
                    if let Err(e) = store.clear() {
                        eprintln!("[ClipLite] 清空历史失败: {e}");
                    }
                }
            }
            // 退出 = 真正退出进程
            "exit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击托盘图标同样呼出面板
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_panel(tray.app_handle());
            }
        });

    // 托盘图标默认使用应用图标
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
