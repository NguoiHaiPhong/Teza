//! Native OS open/save file dialogs using Win32 `comdlg32` and safe filename/extension resolution.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub fn pick_single_file(owner_hwnd: Option<usize>) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::run_dialog(owner_hwnd, false, "", None)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = owner_hwnd;
        Err("Chọn tệp bản địa hiện chỉ hỗ trợ Windows; hãy kéo thả tệp vào cửa sổ".into())
    }
}

pub fn pick_save_file(
    owner_hwnd: Option<usize>,
    suggested_name: &str,
    default_ext: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::run_dialog(owner_hwnd, true, suggested_name, default_ext)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (owner_hwnd, suggested_name, default_ext);
        Err("Lưu tệp bản địa hiện chỉ hỗ trợ Windows".into())
    }
}

pub fn is_known_file_extension(ext: &str) -> bool {
    let lower = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "pdf"
            | "doc"
            | "docx"
            | "docm"
            | "xls"
            | "xlsx"
            | "xlsm"
            | "xlsb"
            | "ppt"
            | "pptx"
            | "pptm"
            | "rtf"
            | "odt"
            | "ods"
            | "odp"
            | "csv"
            | "tsv"
            | "txt"
            | "md"
            | "json"
            | "xml"
            | "yaml"
            | "yml"
            | "html"
            | "htm"
            | "eml"
            | "msg"
            | "log"
            | "ini"
            | "cfg"
            | "conf"
            | "sql"
            | "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "webp"
            | "bmp"
            | "tif"
            | "tiff"
            | "svg"
            | "ico"
            | "heic"
            | "heif"
            | "avif"
            | "psd"
            | "ai"
            | "eps"
            | "mp4"
            | "m4v"
            | "mov"
            | "webm"
            | "mkv"
            | "avi"
            | "wmv"
            | "flv"
            | "3gp"
            | "mp3"
            | "wav"
            | "aac"
            | "m4a"
            | "ogg"
            | "opus"
            | "flac"
            | "wma"
            | "amr"
            | "zip"
            | "rar"
            | "7z"
            | "tar"
            | "gz"
            | "tgz"
            | "bz2"
            | "xz"
            | "iso"
            | "cab"
            | "exe"
            | "msi"
            | "apk"
            | "dmg"
            | "deb"
            | "rpm"
            | "dwg"
            | "dxf"
            | "step"
            | "stp"
            | "iges"
            | "igs"
            | "stl"
            | "gbr"
            | "pcb"
            | "sch"
            | "bin"
            | "hex"
            | "dll"
            | "py"
            | "js"
            | "ts"
            | "rs"
            | "c"
            | "cpp"
            | "h"
            | "java"
            | "cs"
            | "go"
            | "sh"
            | "bat"
            | "ps1"
            | "cmd"
    )
}

pub fn is_plausible_extension(ext: &str) -> bool {
    !ext.is_empty()
        && ext.len() <= 10
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic())
}

pub fn extract_extension_from_name(name: &str) -> Option<String> {
    let cleaned = name
        .trim()
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let seg = cleaned.rsplit(['/', '\\']).next().unwrap_or(cleaned);
    let (stem, ext) = seg.rsplit_once('.')?;
    if stem.trim_matches('.').is_empty() {
        return None;
    }
    if is_plausible_extension(ext) {
        Some(ext.to_ascii_lowercase())
    } else {
        None
    }
}

pub fn extract_extension_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let seg = path.rsplit('/').next().unwrap_or("");
    let ext = extract_extension_from_name(seg)?;
    is_known_file_extension(&ext).then_some(ext)
}

pub fn extension_from_mime(mime: &str) -> Option<&'static str> {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "" | "application/octet-stream" | "binary/octet-stream" | "application/binary" => None,
        // Intentionally leave ZIP MIMEs to magic sniffing so OOXML (.xlsx/.xlsb/.docx/.pptx) wins first.
        "application/zip" | "application/x-zip-compressed" => None,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.ms-excel.sheet.binary.macroenabled.12" => Some("xlsb"),
        "application/vnd.ms-excel.sheet.macroenabled.12" => Some("xlsm"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.ms-word.document.macroenabled.12" => Some("docm"),
        "application/msword" => Some("doc"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "application/vnd.ms-powerpoint.presentation.macroenabled.12" => Some("pptm"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/vnd.oasis.opendocument.text" => Some("odt"),
        "application/vnd.oasis.opendocument.spreadsheet" => Some("ods"),
        "application/vnd.oasis.opendocument.presentation" => Some("odp"),
        "application/pdf" => Some("pdf"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" | "image/x-ms-bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "image/tiff" => Some("tiff"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "video/quicktime" => Some("mov"),
        "video/x-msvideo" => Some("avi"),
        "video/x-matroska" => Some("mkv"),
        "video/x-m4v" => Some("m4v"),
        "video/x-flv" => Some("flv"),
        "video/3gpp" => Some("3gp"),
        "video/x-ms-wmv" => Some("wmv"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/aac" | "audio/x-aac" => Some("aac"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/amr" => Some("amr"),
        "application/vnd.rar" | "application/x-rar-compressed" => Some("rar"),
        "application/x-7z-compressed" => Some("7z"),
        "application/gzip" | "application/x-gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/x-bzip2" => Some("bz2"),
        "application/x-xz" => Some("xz"),
        "text/csv" | "application/csv" => Some("csv"),
        "text/tab-separated-values" => Some("tsv"),
        "text/html" | "application/xhtml+xml" => Some("html"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "application/json" | "text/json" => Some("json"),
        "application/xml" | "text/xml" => Some("xml"),
        "application/rtf" | "text/rtf" => Some("rtf"),
        "message/rfc822" => Some("eml"),
        _ => None,
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn bytes_contain_head_or_tail(bytes: &[u8], needle: &[u8]) -> bool {
    const SCAN_WINDOW: usize = 65_536;
    let head_len = bytes.len().min(SCAN_WINDOW);
    if contains_bytes(&bytes[..head_len], needle) {
        return true;
    }
    if bytes.len() > SCAN_WINDOW {
        let tail_start = bytes.len() - SCAN_WINDOW;
        return contains_bytes(&bytes[tail_start..], needle);
    }
    false
}

pub fn sniff_extension_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        match &bytes[8..12] {
            b"WEBP" => return Some("webp"),
            b"WAVE" => return Some("wav"),
            b"AVI " => return Some("avi"),
            _ => {}
        }
    }
    if bytes.starts_with(b"BM") && bytes.len() >= 14 && bytes[6..10] == [0, 0, 0, 0] {
        return Some("bmp");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("pdf");
    }
    if bytes.starts_with(b"Rar!\x1a\x07\x00") || bytes.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some("rar");
    }
    if bytes.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some("7z");
    }
    if bytes.starts_with(&[0x1F, 0x8B, 0x08]) {
        return Some("gz");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return match &bytes[8..12] {
            b"M4A " | b"M4B " => Some("m4a"),
            b"qt  " => Some("mov"),
            b"heic" | b"heix" | b"hevc" | b"mif1" => Some("heic"),
            b"avif" | b"avis" => Some("avif"),
            _ => Some("mp4"),
        };
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let head = &bytes[..bytes.len().min(64)];
        if head
            .windows(4)
            .any(|w| w.eq_ignore_ascii_case(b"webm"))
        {
            return Some("webm");
        }
        return Some("mkv");
    }
    if bytes.starts_with(b"OggS") {
        return Some("ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return Some("flac");
    }
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && matches!(bytes[1], 0xFB | 0xF3 | 0xF2))
    {
        return Some("mp3");
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.bin") {
            return Some("xlsb");
        }
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.xml")
            || bytes_contain_head_or_tail(bytes, b"xl/worksheets/")
        {
            return Some("xlsx");
        }
        if bytes_contain_head_or_tail(bytes, b"word/document.xml") {
            return Some("docx");
        }
        if bytes_contain_head_or_tail(bytes, b"ppt/presentation.xml") {
            return Some("pptx");
        }
        return Some("zip");
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        if bytes_contain_head_or_tail(bytes, b"W\0o\0r\0d\0D\0o\0c\0u\0m\0e\0n\0t\0\0") {
            return Some("doc");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"P\0o\0w\0e\0r\0P\0o\0i\0n\0t\0 \0D\0o\0c\0u\0m\0e\0n\0t\0\0",
        ) {
            return Some("ppt");
        }
        if bytes_contain_head_or_tail(bytes, b"W\0o\0r\0k\0b\0o\0o\0k\0\0")
            || bytes_contain_head_or_tail(bytes, b"B\0o\0o\0k\0\0")
        {
            return Some("xls");
        }
    }
    None
}

fn is_generic_placeholder_name(raw: &str) -> bool {
    matches!(
        raw.trim().to_lowercase().as_str(),
        "" | "download" | "attachment" | "hình ảnh" | "image" | "photo" | "video" | "teza-image" | "file"
    )
}

fn is_windows_reserved_device_name(stem: &str) -> bool {
    let upper = stem.trim().trim_end_matches('.').to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn is_zip_mime(mime: &str) -> bool {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(base.as_str(), "application/zip" | "application/x-zip-compressed")
}

fn sanitize_single_line_filename_chars(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '\r' | '\n' | '\t') {
            cleaned.push(' ');
        } else if !c.is_control()
            && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        {
            cleaned.push(c);
        }
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn sanitize_suggested_filename_preserving_ext(
    raw: &str,
    resolved_ext: Option<&str>,
) -> String {
    let cleaned = sanitize_single_line_filename_chars(raw);
    if let Some(ext_raw) = resolved_ext {
        let ext = ext_raw.trim().trim_start_matches('.').to_ascii_lowercase();
        if is_plausible_extension(&ext) {
            let dot_ext_len = ext.len() + 1;
            let without_trailing_dots = cleaned.trim_end_matches([' ', '.']);
            let mut stem = if without_trailing_dots.len() > dot_ext_len
                && without_trailing_dots
                    .get(without_trailing_dots.len() - dot_ext_len..)
                    .is_some_and(|tail| {
                        tail.starts_with('.') && tail[1..].eq_ignore_ascii_case(&ext)
                    }) {
                without_trailing_dots[..without_trailing_dots.len() - dot_ext_len]
                    .trim_end_matches([' ', '.'])
                    .to_string()
            } else {
                without_trailing_dots.to_string()
            };
            stem = stem
                .chars()
                .take(160)
                .collect::<String>()
                .trim_end_matches([' ', '.'])
                .to_string();
            if stem.is_empty() {
                stem = "download".to_string();
            } else if is_windows_reserved_device_name(&stem) {
                stem = format!("_{stem}");
            }
            return format!("{stem}.{ext}");
        }
    }

    let trimmed = cleaned
        .trim_end_matches([' ', '.'])
        .chars()
        .take(180)
        .collect::<String>()
        .trim_end_matches([' ', '.'])
        .to_string();
    if trimmed.is_empty() {
        "download".to_string()
    } else if is_windows_reserved_device_name(&trimmed) {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

pub fn resolve_download_filename_and_ext(
    suggested_name: &str,
    content_disposition_name: Option<&str>,
    url: &str,
    final_url: &str,
    mime: &str,
    bytes: &[u8],
) -> (String, Option<String>) {
    let cd_clean = content_disposition_name
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let base_name = if is_generic_placeholder_name(suggested_name) {
        cd_clean.unwrap_or_else(|| {
            if suggested_name.trim().is_empty() {
                "download"
            } else {
                suggested_name.trim()
            }
        })
    } else {
        suggested_name.trim()
    };

    let known_suggested_ext = extract_extension_from_name(suggested_name)
        .filter(|e| is_known_file_extension(e));
    let known_cd_ext = cd_clean
        .and_then(extract_extension_from_name)
        .filter(|e| is_known_file_extension(e));
    let fallback_suggested_ext = extract_extension_from_name(suggested_name);
    let fallback_cd_ext = cd_clean.and_then(extract_extension_from_name);

    let resolved_ext = known_suggested_ext
        .or(known_cd_ext)
        .or_else(|| {
            extract_extension_from_url(final_url).or_else(|| extract_extension_from_url(url))
        })
        .or_else(|| extension_from_mime(mime).map(str::to_string))
        .or_else(|| sniff_extension_from_bytes(bytes).map(str::to_string))
        .or_else(|| is_zip_mime(mime).then(|| "zip".to_string()))
        .or(fallback_suggested_ext)
        .or(fallback_cd_ext);

    let final_name =
        sanitize_suggested_filename_preserving_ext(base_name, resolved_ext.as_deref());
    (final_name, resolved_ext)
}

pub fn ensure_save_path_extension(path: PathBuf, expected_ext: Option<&str>) -> PathBuf {
    let Some(ext) = expected_ext
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| is_plausible_extension(e))
    else {
        return path;
    };

    let cur_opt = path.extension().and_then(|e| e.to_str());
    let should_append = match cur_opt {
        None => true,
        Some(cur) => {
            let c = cur.trim();
            c.is_empty()
                || c.chars().all(|ch| ch.is_ascii_digit())
                || (is_known_file_extension(&ext)
                    && !c.eq_ignore_ascii_case(&ext)
                    && !is_known_file_extension(c))
        }
    };

    if !should_append {
        return path;
    }

    let mut raw: OsString = path.into_os_string();
    if let Some(s) = raw.to_str() {
        if s.ends_with('.') {
            let trimmed = s.trim_end_matches('.').to_string();
            raw = OsString::from(trimmed);
        }
    }
    raw.push(".");
    raw.push(&ext);
    PathBuf::from(raw)
}

pub fn is_dangerous_executable_ext(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "exe"
            | "bat"
            | "cmd"
            | "com"
            | "msi"
            | "msp"
            | "msix"
            | "ps1"
            | "psm1"
            | "psd1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "scr"
            | "pif"
            | "hta"
            | "cpl"
            | "jar"
            | "lnk"
            | "reg"
            | "inf"
            | "dll"
            | "sys"
            | "drv"
            | "sh"
            | "app"
            | "dmg"
            | "pkg"
            | "command"
    )
}

pub fn normalize_os_display_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(s) = path.to_str() {
            if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            if let Some(local) = s.strip_prefix(r"\\?\") {
                return PathBuf::from(local);
            }
        }
        path.to_path_buf()
    }
    #[cfg(not(target_os = "windows"))]
    {
        path.to_path_buf()
    }
}

pub fn open_local_file(path: &Path) -> Result<(), String> {
    if is_dangerous_executable_ext(path) {
        return Err(
            "Vì lý do an toàn, không mở trực tiếp tệp thực thi. Hãy mở thư mục chứa tệp để kiểm tra."
                .into(),
        );
    }
    if !path.is_file() {
        return Err("Không tìm thấy tệp trên máy".into());
    }
    let clean = normalize_os_display_path(path);
    #[cfg(target_os = "windows")]
    {
        win32::shell_open_file(&clean)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở tệp: {e}"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở tệp: {e}"))?;
        Ok(())
    }
}

pub fn reveal_local_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("Không tìm thấy tệp hoặc thư mục trên máy".into());
    }
    let clean = normalize_os_display_path(path);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let path_str = clean
            .to_str()
            .ok_or_else(|| "Đường dẫn tệp chứa ký tự không hợp lệ".to_string())?;
        if path_str.contains('"') || path_str.contains('\0') {
            return Err("Đường dẫn tệp không hợp lệ".into());
        }
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{path_str}\""))
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục chứa tệp: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục chứa tệp: {e}"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = clean
            .parent()
            .ok_or_else(|| "Không xác định được thư mục chứa tệp".to_string())?;
        std::process::Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục chứa tệp: {e}"))?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};

    use super::{
        ensure_save_path_extension, extract_extension_from_name, is_plausible_extension,
        sanitize_suggested_filename_preserving_ext,
    };

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

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> *mut c_void;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
    }

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const SW_SHOWNORMAL: i32 = 1;

    pub fn shell_open_file(path: &std::path::Path) -> Result<(), String> {
        let Some(path_str) = path.to_str() else {
            return Err("Đường dẫn tệp chứa ký tự không hợp lệ".into());
        };
        if path_str.contains('\0') {
            return Err("Đường dẫn tệp không hợp lệ".into());
        }
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = format!("{path_str}\0").encode_utf16().collect();
        let hr = unsafe {
            CoInitializeEx(
                null_mut(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };
        let ret = unsafe {
            ShellExecuteW(
                null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        if hr >= 0 {
            unsafe {
                CoUninitialize();
            }
        }
        let code = ret as isize;
        if code <= 32 {
            return Err(format!("Không thể mở tệp bằng ứng dụng mặc định (mã {code})"));
        }
        Ok(())
    }

    const MB_YESNO: u32 = 0x0000_0004;
    const MB_ICONWARNING: u32 = 0x0000_0030;
    const MB_DEFBUTTON2: u32 = 0x0000_0100;
    const IDYES: i32 = 6;

    pub fn run_dialog(
        owner_hwnd: Option<usize>,
        save_mode: bool,
        suggested_name: &str,
        default_ext: Option<&str>,
    ) -> Result<Option<PathBuf>, String> {
        let effective_ext = if save_mode {
            default_ext
                .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|e| is_plausible_extension(e))
                .or_else(|| extract_extension_from_name(suggested_name))
        } else {
            None
        };

        let filter_str = if save_mode {
            if let Some(ref ext) = effective_ext {
                format!(
                    "Tệp {} (*.{ext})\0*.{ext}\0Tất cả tệp (*.*)\0*.*\0\0",
                    ext.to_ascii_uppercase()
                )
            } else {
                "Tất cả tệp (*.*)\0*.*\0\0".to_string()
            }
        } else {
            "Tất cả tệp (*.*)\0*.*\0Hình ảnh (*.png;*.jpg;*.jpeg;*.webp;*.gif)\0*.png;*.jpg;*.jpeg;*.webp;*.gif\0\0"
                .to_string()
        };
        let filter: Vec<u16> = filter_str.encode_utf16().collect();
        let def_ext_wide: Option<Vec<u16>> = effective_ext
            .as_deref()
            .map(|ext| format!("{ext}\0").encode_utf16().collect());

        let title_str = if save_mode {
            "Lưu tệp về máy\0"
        } else {
            "Chọn tệp hoặc hình ảnh để gửi\0"
        };
        let title: Vec<u16> = title_str.encode_utf16().collect();
        let mut file_buf = vec![0u16; 32_768];

        if save_mode {
            let safe_name = sanitize_suggested_filename_preserving_ext(
                suggested_name,
                effective_ext.as_deref(),
            );
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
            lpstr_def_ext: def_ext_wide.as_ref().map_or(null(), |v| v.as_ptr()),
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
        let chosen = PathBuf::from(path_str);
        if save_mode {
            let adjusted =
                ensure_save_path_extension(chosen.clone(), effective_ext.as_deref());
            if adjusted != chosen && adjusted.exists() {
                let fname = adjusted
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("tệp");
                let msg: Vec<u16> = format!(
                    "Tệp \"{fname}\" đã tồn tại.\nBạn có muốn ghi đè không?\0"
                )
                .encode_utf16()
                .collect();
                let caption: Vec<u16> = "Xác nhận ghi đè\0".encode_utf16().collect();
                let answer = unsafe {
                    MessageBoxW(
                        ofn.hwnd_owner,
                        msg.as_ptr(),
                        caption.as_ptr(),
                        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
                    )
                };
                if answer != IDYES {
                    return Ok(None);
                }
            }
            Ok(Some(adjusted))
        } else {
            Ok(Some(chosen))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_zip_with_entry(entry_name: &[u8]) -> Vec<u8> {
        let mut buf = b"PK\x03\x04\x14\x00\x00\x00\x00\x00".to_vec();
        buf.extend_from_slice(entry_name);
        buf.extend_from_slice(b"\x00\x00PK\x05\x06");
        buf
    }

    #[test]
    fn resolves_xlsx_with_octet_stream_and_normalizes_double_dots() {
        let (name, ext) = resolve_download_filename_and_ext(
            "Check NAD Change PN.xlsx",
            None,
            "https://file-stal-9.dlfl.vn/gr/abc/blob",
            "https://file-stal-9-te-vnso-pt-25.flchat.vn/gr/abc/blob",
            "application/octet-stream",
            b"",
        );
        assert_eq!(name, "Check NAD Change PN.xlsx");
        assert_eq!(ext.as_deref(), Some("xlsx"));

        let (norm, norm_ext) = resolve_download_filename_and_ext(
            "FILE CẤP IC THƯỜNG NOV-2026..xlsx",
            None,
            "https://file-stal-9.dlfl.vn/gr/x",
            "https://file-stal-9.flchat.vn/gr/x",
            "application/octet-stream",
            b"",
        );
        assert_eq!(norm, "FILE CẤP IC THƯỜNG NOV-2026.xlsx");
        assert_eq!(norm_ext.as_deref(), Some("xlsx"));
    }

    #[test]
    fn preserves_extension_on_multiline_and_long_photo_captions() {
        let jpeg_hdr = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let (short_name, short_ext) = resolve_download_filename_and_ext(
            "cháu đk xe qua cổng 2\nvào kho rác ạ",
            None,
            "https://photo-stal-6.zdn.vn/a/pic.jpg",
            "https://photo-stal-6.zdn.vn/a/pic.jpg",
            "image/jpeg",
            &jpeg_hdr,
        );
        assert_eq!(short_name, "cháu đk xe qua cổng 2 vào kho rác ạ.jpg");
        assert_eq!(short_ext.as_deref(), Some("jpg"));

        let long_caption = "A".repeat(220);
        let (long_name, long_ext) = resolve_download_filename_and_ext(
            &long_caption,
            None,
            "https://photo-stal-6.zdn.vn/a/blob",
            "https://photo-stal-6.zdn.vn/a/blob",
            "application/octet-stream",
            &jpeg_hdr,
        );
        assert!(long_name.ends_with(".jpg"));
        assert_eq!(long_ext.as_deref(), Some("jpg"));
        assert!(long_name.len() <= 164);
    }

    #[test]
    fn auto_detects_unnamed_media_ooxml_magic_and_content_disposition() {
        let (photo_name, photo_ext) = resolve_download_filename_and_ext(
            "Hình ảnh",
            None,
            "https://photo-stal-7.zdn.vn/blob",
            "https://photo-stal-7.zdn.vn/blob",
            "image/webp",
            b"",
        );
        assert_eq!(photo_name, "Hình ảnh.webp");
        assert_eq!(photo_ext.as_deref(), Some("webp"));

        let mp4_hdr = [0, 0, 0, 0x18, b'f', b't', b'y', b'p', b'm', b'p', b'4', b'2'];
        let (vid_name, vid_ext) = resolve_download_filename_and_ext(
            "download",
            None,
            "https://video-stal-11.dlmd.me/v/raw",
            "https://video-stal-11-te-vnno-cm-2.mdchat.me/v/raw",
            "application/octet-stream",
            &mp4_hdr,
        );
        assert_eq!(vid_name, "download.mp4");
        assert_eq!(vid_ext.as_deref(), Some("mp4"));

        // OOXML formats labeled as application/octet-stream or application/zip
        assert_eq!(
            resolve_download_filename_and_ext(
                "download",
                None,
                "https://dl.example/raw",
                "https://dl.example/raw",
                "application/octet-stream",
                &fake_zip_with_entry(b"xl/workbook.bin"),
            ),
            ("download.xlsb".into(), Some("xlsb".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "download",
                None,
                "https://dl.example/raw",
                "https://dl.example/raw",
                "application/zip",
                &fake_zip_with_entry(b"xl/workbook.xml"),
            ),
            ("download.xlsx".into(), Some("xlsx".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "download",
                None,
                "https://dl.example/raw",
                "https://dl.example/raw",
                "application/zip",
                &fake_zip_with_entry(b"word/document.xml"),
            ),
            ("download.docx".into(), Some("docx".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "download",
                None,
                "https://dl.example/raw",
                "https://dl.example/raw",
                "application/zip",
                &fake_zip_with_entry(b"ppt/presentation.xml"),
            ),
            ("download.pptx".into(), Some("pptx".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "download",
                None,
                "https://dl.example/raw",
                "https://dl.example/raw",
                "application/zip",
                &fake_zip_with_entry(b"bundle/data.txt"),
            ),
            ("download.zip".into(), Some("zip".into()))
        );

        // Content-Disposition wins when caller only has generic placeholder
        let (cd_name, cd_ext) = resolve_download_filename_and_ext(
            "download",
            Some("Báo cáo tuần 48.pdf"),
            "https://file-stal-9.dlfl.vn/blob",
            "https://file-stal-9.flchat.vn/blob",
            "application/octet-stream",
            b"",
        );
        assert_eq!(cd_name, "Báo cáo tuần 48.pdf");
        assert_eq!(cd_ext.as_deref(), Some("pdf"));
    }

    #[test]
    fn ensure_save_path_extension_guards_dates_renames_and_known_extensions() {
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Check NAD"), Some("xlsx")),
            PathBuf::from(r"C:\dir\Check NAD.xlsx")
        );
        // Must append .xlsx instead of replacing .11 with .xlsx!
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\onhand 24.11"), Some("xlsx")),
            PathBuf::from(r"C:\dir\onhand 24.11.xlsx")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Check longterm 23.Nov"), Some("xlsx")),
            PathBuf::from(r"C:\dir\Check longterm 23.Nov.xlsx")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Check NAD."), Some("xlsx")),
            PathBuf::from(r"C:\dir\Check NAD.xlsx")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Check NAD.csv"), Some("xlsx")),
            PathBuf::from(r"C:\dir\Check NAD.csv")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Check NAD.xlsx"), Some("xlsx")),
            PathBuf::from(r"C:\dir\Check NAD.xlsx")
        );
    }

    #[test]
    fn sanitizes_windows_reserved_device_names_and_ole2_doc_order() {
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("CON.pdf", Some("pdf")),
            "_CON.pdf"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("nul", None),
            "_nul"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("COM1", Some("xlsx")),
            "_COM1.xlsx"
        );

        let mut doc_with_booking = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        doc_with_booking.extend_from_slice(b"B\0o\0o\0k\0i\0n\0g\0");
        doc_with_booking.extend_from_slice(b"W\0o\0r\0d\0D\0o\0c\0u\0m\0e\0n\0t\0\0");
        assert_eq!(sniff_extension_from_bytes(&doc_with_booking), Some("doc"));

        let mut legacy_xls = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        legacy_xls.extend_from_slice(b"W\0o\0r\0k\0b\0o\0o\0k\0\0");
        assert_eq!(sniff_extension_from_bytes(&legacy_xls), Some("xls"));
    }

    #[test]
    fn dangerous_executable_extensions_are_blocked_from_direct_open() {
        for safe in [
            "report.xlsx",
            "doc.pdf",
            "image.png",
            "notes.docx",
            "archive.zip",
            "noext",
        ] {
            assert!(
                !is_dangerous_executable_ext(Path::new(safe)),
                "expected safe: {safe}"
            );
        }
        for dangerous in [
            "setup.exe",
            "run.BAT",
            "script.ps1",
            "shortcut.lnk",
            "installer.msi",
            "macro.vbs",
            "payload.cmd",
        ] {
            assert!(
                is_dangerous_executable_ext(Path::new(dangerous)),
                "expected blocked: {dangerous}"
            );
        }
    }
}
