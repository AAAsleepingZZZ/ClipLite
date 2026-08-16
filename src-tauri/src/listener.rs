//! 剪贴板监听线程：创建隐藏消息窗口并注册为剪贴板格式监听器，
//! 在收到 WM_CLIPBOARDUPDATE 时读取剪贴板内容并写入历史库。

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW,
    GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, RegisterClassW,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HCURSOR, HICON, MSG, WNDCLASSW,
    WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE,
};

use crate::clipboard::Clipboard;
use crate::settings::{Settings, MAX_IMAGE_MB_MAX, MAX_IMAGE_MB_MIN};
use crate::store::{hash_bytes, Store};

/// 监听线程上下文，通过窗口 GWLP_USERDATA 传给窗口过程
struct ListenerContext {
    store: Arc<Mutex<Store>>,
    app: AppHandle,
    clipboard: Arc<Clipboard>,
    /// 共享设置（与设置面板同一引用，图片大小上限实时生效）
    settings: Arc<Mutex<Settings>>,
}

/// 隐藏消息窗口的窗口过程：处理 WM_CLIPBOARDUPDATE，其余交给 DefWindowProcW
unsafe extern "system" fn listener_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        // 从窗口用户数据中取回上下文（unsafe：指针由 SetWindowLongPtrW 写入，见 run_listener）
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if ptr != 0 {
            let ctx = &*(ptr as *const ListenerContext);
            ctx.on_clipboard_update();
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl ListenerContext {
    /// 处理一次剪贴板更新：防回环 → 读图片或文本 → 入库 → 通知前端。
    /// 所有失败均静默跳过（剪贴板竞争、IO 失败等不应影响主流程）。
    fn on_clipboard_update(&self) {
        // 自身写入导致的更新（200ms 内）直接忽略，防止回环
        if self.clipboard.is_recent_self_write() {
            return;
        }

        // 记录来源（前台窗口的应用名与窗口标题）；自身窗口返回 None
        let src = foreground_source();
        let source: Option<(&str, &str)> = src.as_ref().map(|(a, t)| (a.as_str(), t.as_str()));

        let inserted = if self.clipboard.has_image() {
            match self.clipboard.read_image() {
                None => return,
                Some(img) => {
                    // 图片大小上限来自设置（MB），超限直接跳过
                    let max_mb = self
                        .settings
                        .lock()
                        .map(|s| s.max_image_mb)
                        .unwrap_or(10);
                    let max_bytes = (max_mb.clamp(MAX_IMAGE_MB_MIN, MAX_IMAGE_MB_MAX)
                        as usize)
                        * 1024
                        * 1024;
                    if img.bytes.len() > max_bytes {
                        return;
                    }
                    // RGBA → PNG 编码
                    let Some(png) = encode_png(&img) else { return };
                    let hash = hash_bytes(&png);
                    // 先写临时文件，再由 Store 查重/改名入库
                    let tmp_path = {
                        let Ok(store) = self.store.lock() else { return };
                        store.images_dir().join(format!("{hash}.tmp"))
                    };
                    if std::fs::write(&tmp_path, &png).is_err() {
                        return;
                    }
                    match self.store.lock() {
                        Ok(store) => store.insert_image(&tmp_path, source),
                        Err(_) => return,
                    }
                }
            }
        } else if self.clipboard.has_files() {
            // 文件列表（文件管理器复制）：只记录路径，不读内容
            match self.clipboard.read_files() {
                None => return,
                Some(paths) => match self.store.lock() {
                    Ok(store) => store.insert_files(&paths, source),
                    Err(_) => return,
                },
            }
        } else {
            match self.clipboard.read_text() {
                None => return,
                Some(text) if text.trim().is_empty() => return,
                Some(text) => match self.store.lock() {
                    Ok(store) => store.insert_text(&text, source),
                    Err(_) => return,
                },
            }
        };

        // 确实新增了条目才通知前端刷新
        if matches!(inserted, Ok(Some(_))) {
            let _ = self.app.emit("clipboard-updated", ());
        }
    }
}

/// 获取当前前台窗口的来源信息：(exe 名, 窗口标题)。
/// 前台窗口是 ClipLite 自身时返回 None；任何一步失败均返回 None（不阻塞入库）。
fn foreground_source() -> Option<(String, String)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        // 窗口标题
        let mut title_buf = [0u16; 512];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

        // 进程 PID → exe 全路径
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let process = match OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        ) {
            Ok(p) => p,
            Err(_) => return None,
        };
        let mut exe_buf = [0u16; 1024];
        let mut exe_len = exe_buf.len() as u32;
        if QueryFullProcessImageNameW(
            process,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(exe_buf.as_mut_ptr()),
            &mut exe_len,
        )
        .is_err()
        {
            return None;
        }
        let exe_path = String::from_utf16_lossy(&exe_buf[..exe_len as usize]);
        let exe_name = std::path::Path::new(&exe_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if exe_name.is_empty() {
            return None;
        }

        // 自身窗口（面板打开时复制了内容）不记录来源
        if let Ok(self_exe) = std::env::current_exe() {
            if self_exe
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                == Some(exe_name.clone())
            {
                return None;
            }
        }
        Some((exe_name, title))
    }
}

/// 将 arboard 的 RGBA 图像编码为 PNG 字节
fn encode_png(img: &arboard::ImageData) -> Option<Vec<u8>> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    let rgba = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.to_vec())?;
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    PngEncoder::new(&mut cursor)
        .write_image(&rgba.into_raw(), img.width as u32, img.height as u32, ExtendedColorType::Rgba8)
        .ok()?;
    Some(buf)
}

/// 启动剪贴板监听线程
pub fn spawn(
    store: Arc<Mutex<Store>>,
    app: AppHandle,
    clipboard: Arc<Clipboard>,
    settings: Arc<Mutex<Settings>>,
) {
    std::thread::Builder::new()
        .name("clipboard-listener".to_string())
        .spawn(move || run_listener(store, app, clipboard, settings))
        .expect("无法启动剪贴板监听线程");
}

/// 创建隐藏消息窗口并进入消息循环（阻塞直到进程退出）。
/// 全部为 Windows 原生 API，统一放在 unsafe 块中。
fn run_listener(
    store: Arc<Mutex<Store>>,
    app: AppHandle,
    clipboard: Arc<Clipboard>,
    settings: Arc<Mutex<Settings>>,
) {
    unsafe {
        let hmodule = match GetModuleHandleW(None) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[ClipLite] 获取模块句柄失败: {e}");
                return;
            }
        };
        let hinstance = HINSTANCE::from(hmodule);
        let class_name = w!("ClipLite.ClipboardListener");
        let window_name = w!("ClipLiteListener");

        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(listener_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
        };
        // 类已存在时注册失败可忽略（单实例保证仅注册一次）
        let _ = RegisterClassW(&wc);

        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[ClipLite] 创建监听窗口失败: {e}");
                return;
            }
        };

        if let Err(e) = AddClipboardFormatListener(hwnd) {
            eprintln!("[ClipLite] AddClipboardFormatListener 失败: {e}");
            return;
        }

        // 把上下文挂到窗口用户数据上，供窗口过程取用
        let ctx = Box::into_raw(Box::new(ListenerContext {
            store,
            app,
            clipboard,
            settings,
        }));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);

        // 消息循环：GetMessageW 返回 0（WM_QUIT）时退出
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 循环结束后回收上下文
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        drop(Box::from_raw(ctx));
    }
}
