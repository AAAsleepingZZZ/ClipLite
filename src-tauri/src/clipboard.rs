//! 剪贴板读写封装（基于 arboard），并维护"自身写入"时间戳用于防回环。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
use windows::Win32::System::Ole::{CF_BITMAP, CF_DIBV5};

/// 自身写入后的冷却窗口（毫秒）：此窗口内收到的剪贴板变化视为自身写入的回环，应忽略
const SELF_WRITE_WINDOW_MS: u64 = 200;

/// 剪贴板封装：读写走 arboard，图片存在性判断用 Win32 API。
pub struct Clipboard {
    /// 最近一次自身写入的时间戳（毫秒）
    marker: Arc<AtomicU64>,
}

impl Clipboard {
    pub fn new(marker: Arc<AtomicU64>) -> Self {
        Self { marker }
    }

    /// 剪贴板当前是否包含位图（CF_DIBV5 或 CF_BITMAP）
    pub fn has_image(&self) -> bool {
        unsafe {
            IsClipboardFormatAvailable(CF_BITMAP.0 as u32).is_ok()
                || IsClipboardFormatAvailable(CF_DIBV5.0 as u32).is_ok()
        }
    }

    /// 读取剪贴板文本，失败返回 None
    pub fn read_text(&self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    /// 读取剪贴板图片（RGBA 像素），失败返回 None
    pub fn read_image(&self) -> Option<arboard::ImageData<'static>> {
        arboard::Clipboard::new().ok()?.get_image().ok()
    }

    /// 写入文本并标记自身写入
    pub fn write_text(&self, text: &str) -> Result<(), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(text.to_string()).map_err(|e| e.to_string())?;
        self.mark_self_write();
        Ok(())
    }

    /// 写入图片并标记自身写入
    pub fn write_image(&self, img: &arboard::ImageData) -> Result<(), String> {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_image(img.clone()).map_err(|e| e.to_string())?;
        self.mark_self_write();
        Ok(())
    }

    /// 记录一次自身写入（毫秒时间戳）
    pub fn mark_self_write(&self) {
        self.marker.store(now_ms(), Ordering::Relaxed);
    }

    /// 是否处于自身写入后的冷却窗口内（监听线程据此防回环）
    pub fn is_recent_self_write(&self) -> bool {
        now_ms().saturating_sub(self.marker.load(Ordering::Relaxed)) < SELF_WRITE_WINDOW_MS
    }
}

/// 当前 Unix 毫秒时间戳
pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}
