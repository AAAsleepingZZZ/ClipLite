//! 剪贴板监听线程：创建隐藏消息窗口并注册为剪贴板格式监听器，
//! 在收到 WM_CLIPBOARDUPDATE 时读取剪贴板内容并写入历史库。

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    RegisterClassW, SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HCURSOR, HICON, MSG,
    WNDCLASSW, WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE,
};

use crate::clipboard::Clipboard;
use crate::store::{hash_bytes, Store};

/// 原始像素字节超过该值（10MB）的图片不记录
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// 监听线程上下文，通过窗口 GWLP_USERDATA 传给窗口过程
struct ListenerContext {
    store: Arc<Mutex<Store>>,
    app: AppHandle,
    clipboard: Arc<Clipboard>,
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

        let inserted = if self.clipboard.has_image() {
            match self.clipboard.read_image() {
                None => return,
                Some(img) => {
                    // 超大图片（原始像素 >10MB）直接跳过
                    if img.bytes.len() > MAX_IMAGE_BYTES {
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
                        Ok(store) => store.insert_image(&tmp_path),
                        Err(_) => return,
                    }
                }
            }
        } else if self.clipboard.has_files() {
            // 文件列表（文件管理器复制）：只记录路径，不读内容
            match self.clipboard.read_files() {
                None => return,
                Some(paths) => match self.store.lock() {
                    Ok(store) => store.insert_files(&paths),
                    Err(_) => return,
                },
            }
        } else {
            match self.clipboard.read_text() {
                None => return,
                Some(text) if text.trim().is_empty() => return,
                Some(text) => match self.store.lock() {
                    Ok(store) => store.insert_text(&text),
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
pub fn spawn(store: Arc<Mutex<Store>>, app: AppHandle, clipboard: Arc<Clipboard>) {
    std::thread::Builder::new()
        .name("clipboard-listener".to_string())
        .spawn(move || run_listener(store, app, clipboard))
        .expect("无法启动剪贴板监听线程");
}

/// 创建隐藏消息窗口并进入消息循环（阻塞直到进程退出）。
/// 全部为 Windows 原生 API，统一放在 unsafe 块中。
fn run_listener(store: Arc<Mutex<Store>>, app: AppHandle, clipboard: Arc<Clipboard>) {
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
        let ctx = Box::into_raw(Box::new(ListenerContext { store, app, clipboard }));
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
