//! Native Windows screen capture (multi-monitor + virtual desktop) encoded to PNG data URL.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MonitorInfoDto {
    pub index: usize,
    pub label: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenCaptureDto {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
    pub monitors: Vec<MonitorInfoDto>,
    pub active_monitor_index: Option<usize>,
}

/// Convert a 32-bit BGRA pixel buffer (Windows DIB) into opaque RGBA (`alpha = 255`).
pub fn bgra_to_rgba_opaque(bgra: &[u8]) -> Result<Vec<u8>, String> {
    if !bgra.len().is_multiple_of(4) {
        return Err("Bộ đệm pixel BGRA không hợp lệ".into());
    }
    let mut rgba = Vec::with_capacity(bgra.len());
    let (chunks, _) = bgra.as_chunks::<4>();
    for px in chunks {
        rgba.push(px[2]);
        rgba.push(px[1]);
        rgba.push(px[0]);
        rgba.push(255);
    }
    Ok(rgba)
}

/// Encode an RGBA buffer as a lossless `data:image/png;base64,...` URI.
pub fn encode_rgba_png_data_url(width: u32, height: u32, rgba: &[u8]) -> Result<String, String> {
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return Err("Kích thước ảnh chụp màn hình vượt giới hạn cho phép".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Kích thước ảnh vượt giới hạn bộ nhớ".to_string())?;
    if rgba.len() != expected {
        return Err("Dữ liệu pixel RGBA không khớp kích thước ảnh".into());
    }

    let mut png_bytes = Vec::with_capacity((expected / 8).max(1024));
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Lỗi tạo header PNG: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("Lỗi nén ảnh PNG: {e}"))?;
    }
    Ok(format!("data:image/png;base64,{}", B64.encode(&png_bytes)))
}

pub fn capture_screen(
    owner_hwnd: Option<usize>,
    monitor_index: Option<usize>,
    capture_all: bool,
) -> Result<ScreenCaptureDto, String> {
    #[cfg(target_os = "windows")]
    {
        win32::capture_screen(owner_hwnd, monitor_index, capture_all)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (owner_hwnd, monitor_index, capture_all);
        Err("Chụp ảnh màn hình trực tiếp hiện chỉ hỗ trợ Windows".into())
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use super::{bgra_to_rgba_opaque, encode_rgba_png_data_url, MonitorInfoDto, ScreenCaptureDto};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr::{null, null_mut};

    const MONITORINFOF_PRIMARY: u32 = 1;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SM_XVIRTUALSCREEN: i32 = 76;
    const SM_YVIRTUALSCREEN: i32 = 77;
    const SM_CXVIRTUALSCREEN: i32 = 78;
    const SM_CYVIRTUALSCREEN: i32 = 79;
    const SRCCOPY: u32 = 0x00CC_0020;
    const CAPTUREBLT: u32 = 0x4000_0000;
    const DIB_RGB_COLORS: u32 = 0;
    const BI_RGB: u32 = 0;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: Rect,
        rc_work: Rect,
        dw_flags: u32,
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        bmi_header: BitmapInfoHeader,
        bmi_colors: [u32; 1],
    }

    type MonitorEnumProc =
        extern "system" fn(hmon: *mut c_void, hdc: *mut c_void, lprc: *mut Rect, dw_data: isize) -> i32;

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
        fn EnumDisplayMonitors(
            hdc: *mut c_void,
            lprc_clip: *const Rect,
            lpfn_enum: Option<MonitorEnumProc>,
            dw_data: isize,
        ) -> i32;
        fn GetMonitorInfoW(hmonitor: *mut c_void, lpmi: *mut MonitorInfo) -> i32;
        fn MonitorFromWindow(hwnd: *mut c_void, dw_flags: u32) -> *mut c_void;
        fn GetSystemMetrics(n_index: i32) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(hdc: *mut c_void) -> *mut c_void;
        fn DeleteDC(hdc: *mut c_void) -> i32;
        fn CreateCompatibleBitmap(hdc: *mut c_void, cx: i32, cy: i32) -> *mut c_void;
        fn SelectObject(hdc: *mut c_void, h: *mut c_void) -> *mut c_void;
        fn DeleteObject(ho: *mut c_void) -> i32;
        fn BitBlt(
            hdc: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            hdc_src: *mut c_void,
            x1: i32,
            y1: i32,
            rop: u32,
        ) -> i32;
        fn GetDIBits(
            hdc: *mut c_void,
            hbm: *mut c_void,
            start: u32,
            c_lines: u32,
            lpv_bits: *mut c_void,
            lpbmi: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
    }

    struct ScreenDcGuard(*mut c_void);
    impl Drop for ScreenDcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    ReleaseDC(null_mut(), self.0);
                }
            }
        }
    }

    struct MemDcGuard(*mut c_void);
    impl Drop for MemDcGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteDC(self.0);
                }
            }
        }
    }

    struct BitmapGuard(*mut c_void);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    DeleteObject(self.0);
                }
            }
        }
    }

    struct SelectGuard {
        dc: *mut c_void,
        prev: *mut c_void,
    }
    impl Drop for SelectGuard {
        fn drop(&mut self) {
            if !self.dc.is_null() && !self.prev.is_null() {
                unsafe {
                    SelectObject(self.dc, self.prev);
                }
            }
        }
    }

    extern "system" fn enum_monitors_proc(
        hmon: *mut c_void,
        _hdc: *mut c_void,
        _lprc: *mut Rect,
        dw_data: isize,
    ) -> i32 {
        if hmon.is_null() || dw_data == 0 {
            return 1;
        }
        let mut info = MonitorInfo {
            cb_size: size_of::<MonitorInfo>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            dw_flags: 0,
        };
        if unsafe { GetMonitorInfoW(hmon, &mut info) } != 0 {
            let w = (info.rc_monitor.right - info.rc_monitor.left).max(0) as u32;
            let h = (info.rc_monitor.bottom - info.rc_monitor.top).max(0) as u32;
            if w > 0 && h > 0 {
                let list = unsafe { &mut *(dw_data as *mut Vec<MonitorInfoDto>) };
                list.push(MonitorInfoDto {
                    index: 0,
                    label: String::new(),
                    left: info.rc_monitor.left,
                    top: info.rc_monitor.top,
                    width: w,
                    height: h,
                    is_primary: (info.dw_flags & MONITORINFOF_PRIMARY) != 0,
                });
            }
        }
        1
    }

    fn enumerate_monitors() -> Vec<MonitorInfoDto> {
        let mut monitors: Vec<MonitorInfoDto> = Vec::new();
        let data_ptr = (&mut monitors as *mut Vec<MonitorInfoDto>) as isize;
        unsafe {
            EnumDisplayMonitors(null_mut(), null(), Some(enum_monitors_proc), data_ptr);
        }
        monitors.sort_by_key(|m| (!m.is_primary, m.left, m.top));
        for (idx, m) in monitors.iter_mut().enumerate() {
            m.index = idx;
            m.label = format!("Màn hình {}", idx + 1);
        }
        monitors
    }

    fn find_owner_monitor_index(owner_hwnd: Option<usize>, monitors: &[MonitorInfoDto]) -> Option<usize> {
        let hwnd = owner_hwnd? as *mut c_void;
        if hwnd.is_null() {
            return None;
        }
        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if hmon.is_null() {
            return None;
        }
        let mut info = MonitorInfo {
            cb_size: size_of::<MonitorInfo>() as u32,
            rc_monitor: Rect::default(),
            rc_work: Rect::default(),
            dw_flags: 0,
        };
        if unsafe { GetMonitorInfoW(hmon, &mut info) } == 0 {
            return None;
        }
        let w = (info.rc_monitor.right - info.rc_monitor.left).max(0) as u32;
        let h = (info.rc_monitor.bottom - info.rc_monitor.top).max(0) as u32;
        monitors
            .iter()
            .position(|m| {
                m.left == info.rc_monitor.left
                    && m.top == info.rc_monitor.top
                    && m.width == w
                    && m.height == h
            })
            .or_else(|| monitors.iter().position(|m| m.is_primary))
    }

    pub fn capture_screen(
        owner_hwnd: Option<usize>,
        monitor_index: Option<usize>,
        capture_all: bool,
    ) -> Result<ScreenCaptureDto, String> {
        let monitors = enumerate_monitors();

        let (x, y, width, height, active_monitor_index) = if capture_all || monitors.is_empty() {
            let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
            let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
            let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
            let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
            if vw <= 0 || vh <= 0 {
                return Err("Không xác định được kích thước màn hình để chụp".into());
            }
            (vx, vy, vw as u32, vh as u32, None)
        } else {
            let idx = monitor_index
                .filter(|&i| i < monitors.len())
                .or_else(|| find_owner_monitor_index(owner_hwnd, &monitors))
                .unwrap_or(0);
            let m = &monitors[idx];
            (m.left, m.top, m.width, m.height, Some(idx))
        };

        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            return Err("Kích thước vùng chụp vượt quá giới hạn 16384px".into());
        }

        let screen_dc_raw = unsafe { GetDC(null_mut()) };
        if screen_dc_raw.is_null() {
            return Err("Không lấy được Device Context màn hình Windows".into());
        }
        let screen_dc = ScreenDcGuard(screen_dc_raw);

        let mem_dc_raw = unsafe { CreateCompatibleDC(screen_dc.0) };
        if mem_dc_raw.is_null() {
            return Err("Không tạo được bộ nhớ DC để chụp màn hình".into());
        }
        let mem_dc = MemDcGuard(mem_dc_raw);

        let bmp_raw = unsafe { CreateCompatibleBitmap(screen_dc.0, width as i32, height as i32) };
        if bmp_raw.is_null() {
            return Err("Không tạo được bitmap chụp màn hình".into());
        }
        let bmp = BitmapGuard(bmp_raw);

        let prev_obj = unsafe { SelectObject(mem_dc.0, bmp.0) };
        if prev_obj.is_null() {
            return Err("Không chọn được bitmap vào DC".into());
        }
        let _sel = SelectGuard {
            dc: mem_dc.0,
            prev: prev_obj,
        };

        let blt_ok = unsafe {
            BitBlt(
                mem_dc.0,
                0,
                0,
                width as i32,
                height as i32,
                screen_dc.0,
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            )
        };
        if blt_ok == 0 {
            return Err("Chụp màn hình thất bại (BitBlt bị từ chối hoặc màn hình đang khóa)".into());
        }

        let byte_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| "Kích thước ảnh vượt giới hạn bộ nhớ".to_string())?;
        let mut bgra = vec![0u8; byte_len];

        let mut bmi = BitmapInfo {
            bmi_header: BitmapInfoHeader {
                bi_size: size_of::<BitmapInfoHeader>() as u32,
                bi_width: width as i32,
                bi_height: -(height as i32), // top-down DIB
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: BI_RGB,
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            bmi_colors: [0],
        };

        let copied = unsafe {
            GetDIBits(
                mem_dc.0,
                bmp.0,
                0,
                height,
                bgra.as_mut_ptr() as *mut c_void,
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        if copied == 0 {
            return Err("Không trích xuất được dữ liệu ảnh màn hình (GetDIBits)".into());
        }

        drop(_sel);
        let rgba = bgra_to_rgba_opaque(&bgra)?;
        let data_url = encode_rgba_png_data_url(width, height, &rgba)?;

        Ok(ScreenCaptureDto {
            data_url,
            width,
            height,
            monitors,
            active_monitor_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_to_rgba_opaque_swaps_channels_and_forces_alpha() {
        let bgra = [10u8, 20, 30, 0, 40, 50, 60, 128];
        let rgba = bgra_to_rgba_opaque(&bgra).unwrap();
        assert_eq!(rgba, vec![30, 20, 10, 255, 60, 50, 40, 255]);
        assert!(bgra_to_rgba_opaque(&[1, 2, 3]).is_err());
    }

    #[test]
    fn encode_rgba_png_data_url_produces_valid_png_and_validates_inputs() {
        assert!(encode_rgba_png_data_url(0, 2, &[]).is_err());
        assert!(encode_rgba_png_data_url(2, 0, &[]).is_err());
        assert!(encode_rgba_png_data_url(16_385, 1, &[]).is_err());
        assert!(encode_rgba_png_data_url(2, 2, &[0u8; 8]).is_err());

        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let url = encode_rgba_png_data_url(2, 2, &rgba).unwrap();
        let b64 = url.strip_prefix("data:image/png;base64,").unwrap();
        let decoded = B64.decode(b64).unwrap();
        assert!(decoded.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
