//! Native OS open/save file dialogs using Win32 `comdlg32` without extra dependencies.

use std::path::PathBuf;

pub fn pick_single_file(owner_hwnd: Option<usize>) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::run_dialog(owner_hwnd, false, "")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = owner_hwnd;
        Err("Chọn tệp bản địa hiện chỉ hỗ trợ Windows; hãy kéo thả tệp vào cửa sổ".into())
    }
}

pub fn pick_save_file(owner_hwnd: Option<usize>, suggested_name: &str) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::run_dialog(owner_hwnd, true, suggested_name)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (owner_hwnd, suggested_name);
        Err("Lưu tệp bản địa hiện chỉ hỗ trợ Windows".into())
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};

    #[cfg_attr(target_pointer_width = "32", repr(C, packed(1)))]
    #[cfg_attr(target_pointer_width = "64", repr(C))]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: *mut c_void,
        h_instance: *mut c_void,
        lpstr_filter: *const u16,
        lpstr_custom_filter: *mut u16,
        n_max_cust_filter: u32,
        n_filter_index: u32,
        lpstr_file: *mut u16,
        n_max_file: u32,
        lpstr_file_title: *mut u16,
        n_max_file_title: u32,
        lpstr_initial_dir: *const u16,
        lpstr_title: *const u16,
        flags: u32,
        n_file_offset: u16,
        n_file_extension: u16,
        lpstr_def_ext: *const u16,
        l_cust_data: usize,
        lpfn_hook: *mut c_void,
        lp_template_name: *const u16,
        pv_reserved: *mut c_void,
        dw_reserved: u32,
        flags_ex: u32,
    }

    const OFN_OVERWRITEPROMPT: u32 = 0x0000_0002;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_EXPLORER: u32 = 0x0008_0000;

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(lpofn: *mut OpenFileNameW) -> i32;
        fn GetSaveFileNameW(lpofn: *mut OpenFileNameW) -> i32;
        fn CommDlgExtendedError() -> u32;
    }

    fn sanitize_suggested_filename(raw: &str) -> String {
        let cleaned: String = raw
            .trim()
            .chars()
            .filter(|c| {
                !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            })
            .collect();
        if cleaned.is_empty() {
            "download".to_string()
        } else {
            cleaned.chars().take(180).collect()
        }
    }

    pub fn run_dialog(
        owner_hwnd: Option<usize>,
        save_mode: bool,
        suggested_name: &str,
    ) -> Result<Option<PathBuf>, String> {
        let filter: Vec<u16> = "Tất cả tệp (*.*)\0*.*\0Hình ảnh (*.png;*.jpg;*.jpeg;*.webp;*.gif)\0*.png;*.jpg;*.jpeg;*.webp;*.gif\0\0"
            .encode_utf16()
            .collect();
        let title_str = if save_mode {
            "Lưu tệp về máy\0"
        } else {
            "Chọn tệp hoặc hình ảnh để gửi\0"
        };
        let title: Vec<u16> = title_str.encode_utf16().collect();
        let mut file_buf = vec![0u16; 32_768];

        if save_mode {
            let safe_name = sanitize_suggested_filename(suggested_name);
            let encoded: Vec<u16> = safe_name.encode_utf16().collect();
            let copy_len = encoded.len().min(file_buf.len() - 1);
            file_buf[..copy_len].copy_from_slice(&encoded[..copy_len]);
        }

        let flags = if save_mode {
            OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_EXPLORER
        } else {
            OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_EXPLORER
        };

        let mut ofn = OpenFileNameW {
            l_struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
            hwnd_owner: owner_hwnd.map(|h| h as *mut c_void).unwrap_or(null_mut()),
            h_instance: null_mut(),
            lpstr_filter: filter.as_ptr(),
            lpstr_custom_filter: null_mut(),
            n_max_cust_filter: 0,
            n_filter_index: 1,
            lpstr_file: file_buf.as_mut_ptr(),
            n_max_file: file_buf.len() as u32,
            lpstr_file_title: null_mut(),
            n_max_file_title: 0,
            lpstr_initial_dir: null(),
            lpstr_title: title.as_ptr(),
            flags,
            n_file_offset: 0,
            n_file_extension: 0,
            lpstr_def_ext: null(),
            l_cust_data: 0,
            lpfn_hook: null_mut(),
            lp_template_name: null(),
            pv_reserved: null_mut(),
            dw_reserved: 0,
            flags_ex: 0,
        };

        let ok = unsafe {
            if save_mode {
                GetSaveFileNameW(&mut ofn)
            } else {
                GetOpenFileNameW(&mut ofn)
            }
        };
        if ok == 0 {
            let err_code = unsafe { CommDlgExtendedError() };
            if err_code == 0 {
                return Ok(None);
            }
            return Err(format!("Hộp thoại tệp lỗi (mã {err_code})"));
        }

        let end = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
        let path_str = String::from_utf16(&file_buf[..end]).map_err(|e| e.to_string())?;
        if path_str.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(path_str)))
    }
}
