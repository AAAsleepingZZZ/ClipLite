//! 屏幕区域截取：面板弹出时抓取其所在位置的桌面区域，
//! 前端将图片模糊后作为毛玻璃背景（任何 Windows 版本都可靠的方案）。

use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::io::Cursor;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    SRCCOPY,
};

/// 截取屏幕矩形区域（物理像素），输出缩小 `scale` 倍后的 PNG data URL。
/// 任何一步失败都返回 None，调用方回退为纯色背景。
pub fn capture_region(x: i32, y: i32, w: i32, h: i32, scale: f64) -> Option<String> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let out_w = ((w as f64) * scale).round().max(1.0) as i32;
    let out_h = ((h as f64) * scale).round().max(1.0) as i32;

    unsafe {
        // 屏幕 DC（桌面）
        let screen_dc = GetDC(HWND(std::ptr::null_mut()));
        if screen_dc.0.is_null() {
            return None;
        }

        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.0.is_null() {
            let _ = ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return None;
        }

        let bitmap = CreateCompatibleBitmap(screen_dc, out_w, out_h);
        if bitmap.0.is_null() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
            return None;
        }

        let result = (|| {
            let _old = SelectObject(mem_dc, bitmap);
            // 把目标区域缩小拷贝到内存位图（StretchBlt 自带平滑）
            if !StretchBlt(
                mem_dc, 0, 0, out_w, out_h, screen_dc, x, y, w, h, SRCCOPY,
            )
            .as_bool()
            {
                return None;
            }

            // 提取 BGRA 像素（top-down 行序）
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: out_w,
                    biHeight: -out_h, // 负值 = 自上而下，无需行翻转
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0 as u32,
                    ..Default::default()
                },
                bmiColors: Default::default(),
            };

            let mut bgra = vec![0u8; (out_w * out_h * 4) as usize];
            let copied = GetDIBits(
                mem_dc,
                bitmap,
                0,
                out_h as u32,
                Some(bgra.as_mut_ptr() as *mut _),
                &mut header,
                DIB_RGB_COLORS,
            );
            if copied == 0 {
                return None;
            }

            // BGRA → RGBA（图像库与 WebView 均按 RGBA 处理）
            for px in bgra.chunks_exact_mut(4) {
                px[3] = 255; // GDI 捕获的 alpha 为 0，必须填充为不透明，否则 PNG 全透明
                px.swap(0, 2);
            }

            // 高斯模糊（box blur 近似）：毛玻璃效果在 Rust 端完成，
            // 避免前端 CSS filter 在透明窗口上的合成问题
            let img = image::RgbaImage::from_raw(out_w as u32, out_h as u32, bgra)?;
            let blurred = image::imageops::blur(&img, 14.0);
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(blurred)
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .ok()?;

            Some(format!("data:image/png;base64,{}", STANDARD.encode(&png)))
        })();

        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(HWND(std::ptr::null_mut()), screen_dc);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_region_works() {
        let r = capture_region(0, 0, 200, 200, 0.5);
        assert!(r.is_some(), "capture_region 返回 None，毛玻璃链路第一步就失败");
        let s = r.unwrap();
        eprintln!("capture ok, data_url 长度 = {}", s.len());
        assert!(s.starts_with("data:image/png;base64,"));
    }
}
