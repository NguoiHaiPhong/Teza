//! Native Windows clipboard helper for reliable text and image copying without WebView2 gesture limits.

pub fn copy_text_native(text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win32::copy_utf16_text(text)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = text;
        Err("Clipboard bản địa hiện chỉ hỗ trợ Windows".into())
    }
}

pub fn copy_rgba_image_native(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let dib = rgba_to_dib_bytes(width, height, rgba)?;
    #[cfg(target_os = "windows")]
    {
        win32::copy_dib(&dib)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = dib;
        Err("Clipboard ảnh bản địa hiện chỉ hỗ trợ Windows".into())
    }
}

pub fn encode_utf16_nul_le(text: &str) -> Vec<u8> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let mut raw = Vec::with_capacity(utf16.len() * 2);
    for unit in utf16 {
        raw.extend_from_slice(&unit.to_le_bytes());
    }
    raw
}

pub fn rgba_to_dib_bytes(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return Err("Kích thước ảnh không hợp lệ để sao chép".into());
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Kích thước ảnh vượt giới hạn".to_string())?;
    if rgba.len() != expected {
        return Err("Dữ liệu pixel RGBA không khớp kích thước ảnh".into());
    }

    let mut out = Vec::with_capacity(40 + expected);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight (positive = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
    out.extend_from_slice(&(expected as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    let row_bytes = (width as usize) * 4;
    for y in (0..height as usize).rev() {
        let row = &rgba[y * row_bytes..(y + 1) * row_bytes];
        for px in row.as_chunks::<4>().0 {
            out.push(px[2]); // B
            out.push(px[1]); // G
            out.push(px[0]); // R
            out.push(px[3]); // A
        }
    }
    Ok(out)
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::ptr::{copy_nonoverlapping, null_mut};
    use std::time::Duration;

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd_new_owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(u_format: u32, h_mem: *mut c_void) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> *mut c_void;
        fn GlobalLock(h_mem: *mut c_void) -> *mut u8;
        fn GlobalUnlock(h_mem: *mut c_void) -> i32;
        fn GlobalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    struct ClipboardLock;
    impl Drop for ClipboardLock {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<(), String> {
        let mut opened = false;
        for _ in 0..6 {
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !opened {
            return Err("Không mở được Clipboard Windows (đang bị ứng dụng khác khóa)".into());
        }
        let _guard = ClipboardLock;

        if unsafe { EmptyClipboard() } == 0 {
            return Err("Không làm trống được Clipboard".into());
        }

        let h_mem = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if h_mem.is_null() {
            return Err("Không cấp phát được bộ nhớ Clipboard".into());
        }

        let dst = unsafe { GlobalLock(h_mem) };
        if dst.is_null() {
            unsafe {
                GlobalFree(h_mem);
            }
            return Err("Không khóa được bộ nhớ Clipboard".into());
        }
        unsafe {
            copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            GlobalUnlock(h_mem);
        }

        let res = unsafe { SetClipboardData(format, h_mem) };
        if res.is_null() {
            unsafe {
                GlobalFree(h_mem);
            }
            return Err("Ghi dữ liệu vào Clipboard thất bại".into());
        }
        Ok(())
    }

    pub fn copy_utf16_text(text: &str) -> Result<(), String> {
        let raw = super::encode_utf16_nul_le(text);
        set_clipboard_bytes(CF_UNICODETEXT, &raw)
    }

    pub fn copy_dib(dib: &[u8]) -> Result<(), String> {
        set_clipboard_bytes(CF_DIB, dib)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_utf16_nul_le_handles_vietnamese_empty_and_surrogates() {
        let empty = encode_utf16_nul_le("");
        assert_eq!(empty, [0u8, 0u8]);

        let text = "Xin chào 🚀";
        let encoded = encode_utf16_nul_le(text);
        assert_eq!(encoded.len() % 2, 0);
        assert_eq!(&encoded[encoded.len() - 2..], &[0, 0]);
        let units: Vec<u16> = encoded[..encoded.len() - 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), text);
    }

    #[test]
    fn rgba_to_dib_bytes_rejects_invalid_dimensions_and_mismatched_buffers() {
        assert!(rgba_to_dib_bytes(0, 1, &[]).is_err());
        assert!(rgba_to_dib_bytes(1, 0, &[]).is_err());
        assert!(rgba_to_dib_bytes(16_385, 1, &[]).is_err());
        assert!(rgba_to_dib_bytes(2, 2, &[0u8; 12]).is_err());
    }

    #[test]
    fn rgba_to_dib_builds_bottom_up_bgra_header() {
        // 2x2 RGBA: top row = red, green; bottom row = blue, white
        let rgba = [
            255, 0, 0, 255, // (0,0) top-left red
            0, 255, 0, 255, // (1,0) top-right green
            0, 0, 255, 255, // (0,1) bottom-left blue
            255, 255, 255, 128, // (1,1) bottom-right semi-white
        ];
        let dib = rgba_to_dib_bytes(2, 2, &rgba).unwrap();
        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[4..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32);
        // Bottom row appears first in bottom-up DIB: blue -> BGRA(255,0,0,255)
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]);
        assert_eq!(&dib[44..48], &[255, 255, 255, 128]);
        // Top row appears second: red -> BGRA(0,0,255,255)
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]);
        assert_eq!(&dib[52..56], &[0, 255, 0, 255]);
    }
}
