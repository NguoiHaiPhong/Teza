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

pub fn pick_directory(owner_hwnd: Option<usize>) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "windows")]
    {
        win32::run_pick_directory(owner_hwnd)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = owner_hwnd;
        Err("Chọn thư mục bản địa hiện chỉ hỗ trợ Windows".into())
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
            | "epub"
            | "mobi"
            | "djvu"
            | "pages"
            | "numbers"
            | "key"
            | "vsdx"
            | "mpp"
            | "accdb"
            | "mdb"
            | "sqlite"
            | "db"
            | "bak"
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
            | "srt"
            | "vtt"
            | "ass"
            | "ics"
            | "vcf"
            | "jpg"
            | "jpeg"
            | "jfif"
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
            | "jxl"
            | "raw"
            | "dng"
            | "cr2"
            | "nef"
            | "arw"
            | "psd"
            | "ai"
            | "eps"
            | "xcf"
            | "cdr"
            | "indd"
            | "fig"
            | "sketch"
            | "mp4"
            | "m4v"
            | "mov"
            | "webm"
            | "mkv"
            | "avi"
            | "wmv"
            | "flv"
            | "3gp"
            | "ogv"
            | "mpg"
            | "mpeg"
            | "m2ts"
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
            | "zst"
            | "lz4"
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
            | "blend"
            | "obj"
            | "fbx"
            | "glb"
            | "gltf"
            | "pem"
            | "crt"
            | "cer"
            | "p12"
            | "pfx"
            | "ovpn"
            | "torrent"
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaFamily {
    Image,
    Video,
    Audio,
    Other,
}

fn media_family_of(ext: &str) -> MediaFamily {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "svg"
        | "ico" | "heic" | "heif" | "avif" | "jxl" => MediaFamily::Image,
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv" | "3gp" | "ogv"
        | "mpg" | "mpeg" | "m2ts" => MediaFamily::Video,
        "mp3" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "flac" | "wma" | "amr" => {
            MediaFamily::Audio
        }
        _ => MediaFamily::Other,
    }
}

fn extensions_equivalent(a: &str, b: &str) -> bool {
    let la = a.trim().trim_start_matches('.').to_ascii_lowercase();
    let lb = b.trim().trim_start_matches('.').to_ascii_lowercase();
    if la == lb {
        return true;
    }
    matches!(
        (la.as_str(), lb.as_str()),
        ("jpg" | "jpeg" | "jfif", "jpg" | "jpeg" | "jfif")
            | ("tif" | "tiff", "tif" | "tiff")
            | ("heic" | "heif", "heic" | "heif")
            | ("htm" | "html", "htm" | "html")
            | ("yml" | "yaml", "yml" | "yaml")
            | ("mpg" | "mpeg", "mpg" | "mpeg")
    )
}

fn extensions_family_compatible(candidate: &str, detected: &str) -> bool {
    if candidate.eq_ignore_ascii_case(detected) || extensions_equivalent(candidate, detected) {
        return true;
    }
    let cf = media_family_of(candidate);
    let df = media_family_of(detected);
    match df {
        MediaFamily::Image => cf == MediaFamily::Image,
        MediaFamily::Video | MediaFamily::Audio => {
            matches!(cf, MediaFamily::Video | MediaFamily::Audio)
        }
        MediaFamily::Other => true,
    }
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

fn percent_decode_url_component(raw: &str, plus_as_space: bool) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if plus_as_space && bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_teza_media_vault_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("teza-media://")
        || lower.starts_with("http://teza-media.localhost/")
        || lower.starts_with("https://teza-media.localhost/")
}

fn extract_extension_from_url_depth(url: &str, depth: u8) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || depth > 2 {
        return None;
    }

    let without_frag = trimmed.split('#').next().unwrap_or(trimmed);
    let (path, query_opt) = match without_frag.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (without_frag, None),
    };

    // Unwrap `teza-media://` / `http://teza-media.localhost/resolve?url=<encoded>` inner remote URL.
    if is_teza_media_vault_url(trimmed) {
        if let Some(query) = query_opt {
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k.eq_ignore_ascii_case("url") {
                        let inner = percent_decode_url_component(v, true);
                        return extract_extension_from_url_depth(&inner, depth + 1);
                    }
                }
            }
        }
        return None;
    }

    let seg = path.rsplit('/').next().unwrap_or("");
    let decoded_seg = percent_decode_url_component(seg, false);
    if let Some(ext) = extract_extension_from_name(&decoded_seg) {
        if is_known_file_extension(&ext) {
            return Some(ext);
        }
    }

    // Fallback: inspect common filename query parameters (e.g., `?filename=report.pdf`,
    // `?response-content-disposition=attachment;filename="pic.jpg"`).
    if let Some(query) = query_opt {
        for pair in query.split('&') {
            let Some((k_raw, v_raw)) = pair.split_once('=') else {
                continue;
            };
            let key = percent_decode_url_component(k_raw, true)
                .trim()
                .to_ascii_lowercase();
            let val = percent_decode_url_component(v_raw, true);
            if matches!(key.as_str(), "filename" | "file" | "name" | "download") {
                if let Some(ext) = extract_extension_from_name(&val) {
                    if is_known_file_extension(&ext) {
                        return Some(ext);
                    }
                }
            } else if key == "response-content-disposition" {
                for part in val.split(';') {
                    let part_trim = part.trim();
                    let lower_part = part_trim.to_ascii_lowercase();
                    let fname_raw = if lower_part.starts_with("filename*=") {
                        part_trim[10..]
                            .split("''")
                            .nth(1)
                            .map(|s| percent_decode_url_component(s, false))
                    } else if lower_part.starts_with("filename=") {
                        Some(part_trim[9..].trim_matches('"').to_string())
                    } else {
                        None
                    };
                    if let Some(fname) = fname_raw {
                        if let Some(ext) = extract_extension_from_name(&fname) {
                            if is_known_file_extension(&ext) {
                                return Some(ext);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn extract_extension_from_url(url: &str) -> Option<String> {
    extract_extension_from_url_depth(url, 0)
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
        "application/zip"
        | "application/x-zip-compressed"
        | "application/x-zip"
        | "multipart/x-zip" => None,
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
        "application/epub+zip" => Some("epub"),
        "application/vnd.android.package-archive" => Some("apk"),
        "application/pdf" => Some("pdf"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" | "image/jfif" => Some("jpg"),
        "image/png" | "image/x-png" | "image/apng" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" | "image/x-ms-bmp" | "image/x-bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "image/tiff" | "image/x-tiff" => Some("tiff"),
        "image/heic" | "image/heic-sequence" => Some("heic"),
        "image/heif" | "image/heif-sequence" => Some("heif"),
        "image/avif" | "image/avif-sequence" => Some("avif"),
        "image/jxl" => Some("jxl"),
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
        "video/mpeg" => Some("mpg"),
        "video/ogg" => Some("ogv"),
        "audio/mpeg" | "audio/mp3" | "audio/x-mpeg" | "audio/x-mp3" => Some("mp3"),
        "audio/aac" | "audio/x-aac" | "audio/aacp" | "audio/vnd.dlna.adts" => Some("aac"),
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => Some("m4a"),
        "audio/ogg" | "audio/vorbis" | "application/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/webm" => Some("webm"),
        "audio/3gpp" => Some("3gp"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/amr" | "audio/amr-wb" => Some("amr"),
        "application/vnd.rar" | "application/x-rar-compressed" => Some("rar"),
        "application/x-7z-compressed" | "application/x-7z" => Some("7z"),
        "application/gzip" | "application/x-gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/x-bzip2" => Some("bz2"),
        "application/x-xz" => Some("xz"),
        "application/zstd" => Some("zst"),
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

fn is_valid_aac_adts_header(bytes: &[u8]) -> bool {
    if bytes.len() < 7 || bytes[0] != 0xFF || (bytes[1] & 0xF6) != 0xF0 {
        return false;
    }
    let sampling_freq_idx = (bytes[2] >> 2) & 0x0F;
    if sampling_freq_idx >= 13 {
        return false;
    }
    let frame_len = (((bytes[3] & 0x03) as usize) << 11)
        | ((bytes[4] as usize) << 3)
        | (((bytes[5] & 0xE0) as usize) >> 5);
    frame_len >= 7
}

fn sniff_isobmff_ext(bytes: &[u8]) -> &'static str {
    match &bytes[8..12] {
        b"M4A " | b"M4B " => "m4a",
        b"qt  " => "mov",
        b"avif" | b"avis" => "avif",
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" => "heic",
        b"mif1" | b"msf1" => {
            let box_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
            let scan_end = if box_len >= 16 {
                box_len.min(bytes.len()).min(64)
            } else {
                bytes.len().min(64)
            };
            let mut off = 16usize;
            while off + 4 <= scan_end {
                let brand = &bytes[off..off + 4];
                if brand == b"avif" || brand == b"avis" {
                    return "avif";
                }
                if matches!(
                    brand,
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis"
                ) {
                    return "heic";
                }
                off += 4;
            }
            "heic"
        }
        b"3gp4" | b"3gp5" | b"3gp6" | b"3gp7" | b"3g2a" | b"3g2b" => "3gp",
        _ => "mp4",
    }
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
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some("tiff");
    }
    if bytes.len() >= 6
        && bytes[0..4] == [0x00, 0x00, 0x01, 0x00]
        && u16::from_le_bytes([bytes[4], bytes[5]]) > 0
    {
        return Some("ico");
    }
    if bytes.starts_with(&[
        0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ]) {
        return Some("jxl");
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
    if bytes.len() >= 10
        && bytes.starts_with(b"BZh")
        && bytes[3].is_ascii_digit()
        && &bytes[4..10] == b"\x31\x41\x59\x26\x53\x59"
    {
        return Some("bz2");
    }
    if bytes.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Some("xz");
    }
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some("zst");
    }
    if bytes.starts_with(b"{\\rtf") {
        return Some("rtf");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(sniff_isobmff_ext(bytes));
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
    if bytes.starts_with(b"#!AMR\n") || bytes.starts_with(b"#!AMR-WB\n") {
        return Some("amr");
    }
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2
            && bytes[0] == 0xFF
            && matches!(bytes[1], 0xFB | 0xFA | 0xF3 | 0xF2 | 0xE3 | 0xE2))
    {
        return Some("mp3");
    }
    if is_valid_aac_adts_header(bytes) {
        return Some("aac");
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.bin") {
            return Some("xlsb");
        }
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.xml")
            || bytes_contain_head_or_tail(bytes, b"xl/worksheets/")
        {
            if bytes_contain_head_or_tail(bytes, b"xl/vbaProject.bin") {
                return Some("xlsm");
            }
            return Some("xlsx");
        }
        if bytes_contain_head_or_tail(bytes, b"word/document.xml") {
            if bytes_contain_head_or_tail(bytes, b"word/vbaProject.bin") {
                return Some("docm");
            }
            return Some("docx");
        }
        if bytes_contain_head_or_tail(bytes, b"ppt/presentation.xml") {
            if bytes_contain_head_or_tail(bytes, b"ppt/vbaProject.bin") {
                return Some("pptm");
            }
            return Some("pptx");
        }
        if bytes_contain_head_or_tail(bytes, b"mimetypeapplication/vnd.oasis.opendocument.text") {
            return Some("odt");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"mimetypeapplication/vnd.oasis.opendocument.spreadsheet",
        ) {
            return Some("ods");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"mimetypeapplication/vnd.oasis.opendocument.presentation",
        ) {
            return Some("odp");
        }
        if bytes_contain_head_or_tail(bytes, b"mimetypeapplication/epub+zip") {
            return Some("epub");
        }
        if bytes_contain_head_or_tail(bytes, b"AndroidManifest.xml") {
            return Some("apk");
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
        "" | "download"
            | "attachment"
            | "hình ảnh"
            | "ảnh"
            | "ảnh gif"
            | "gif"
            | "nhãn dán"
            | "sticker"
            | "tin nhắn thoại"
            | "voice"
            | "voice message"
            | "audio"
            | "tập tin"
            | "tập tin đính kèm"
            | "attached file"
            | "image"
            | "photo"
            | "video"
            | "teza-image"
            | "teza-video"
            | "teza-voice"
            | "teza-sticker"
            | "file"
            | "media"
            | "blob"
            | "resolve"
            | "untitled"
            | "사진"
            | "이미지"
            | "스티커"
            | "음성 메시지"
            | "동영상"
            | "파일"
    )
}

/// Detects synthetic placeholder filenames WITH an extension (e.g., `image.jpg` generated by the
/// parser for untitled Zalo photos or `sticker_12_345.png` for stickers whose true bytes may be
/// PNG/WebP/GIF).
fn is_synthetic_media_placeholder_with_ext(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (stem, ext) = trimmed.rsplit_once('.')?;
    if !is_plausible_extension(ext) {
        return None;
    }
    let lower_stem = stem.trim().to_ascii_lowercase();
    let lower_ext = ext.to_ascii_lowercase();
    let is_placeholder_stem = matches!(
        lower_stem.as_str(),
        "image"
            | "photo"
            | "teza-image"
            | "teza-video"
            | "teza-voice"
            | "teza-sticker"
            | "download"
            | "attachment"
            | "media"
            | "resolve"
            | "blob"
    ) || (lower_stem.starts_with("sticker_")
        && lower_stem[8..]
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'_'));
    if !is_placeholder_stem {
        return None;
    }
    if matches!(
        media_family_of(&lower_ext),
        MediaFamily::Image | MediaFamily::Video | MediaFamily::Audio
    ) || lower_ext == "bin"
    {
        Some(stem.trim().to_string())
    } else {
        None
    }
}

fn is_windows_reserved_device_name(stem: &str) -> bool {
    let first_segment = stem.split('.').next().unwrap_or(stem);
    let upper = first_segment.trim().to_ascii_uppercase();
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
    matches!(
        base.as_str(),
        "application/zip"
            | "application/x-zip-compressed"
            | "application/x-zip"
            | "multipart/x-zip"
    )
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

fn truncate_utf16_units(s: &str, max_units: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for ch in s.chars() {
        let len = ch.len_utf16();
        if units + len > max_units {
            break;
        }
        out.push(ch);
        units += len;
    }
    out
}

pub fn sanitize_suggested_filename_preserving_ext(
    raw: &str,
    resolved_ext: Option<&str>,
) -> String {
    let cleaned = sanitize_single_line_filename_chars(raw);
    if let Some(ext_raw) = resolved_ext {
        let ext = ext_raw.trim().trim_start_matches('.').to_ascii_lowercase();
        if is_plausible_extension(&ext) {
            let without_trailing_dots = cleaned.trim_end_matches([' ', '.']);
            let mut stem = if let Some((head, tail_ext)) = without_trailing_dots.rsplit_once('.') {
                if extensions_equivalent(tail_ext, &ext) {
                    head.trim_end_matches([' ', '.']).to_string()
                } else {
                    without_trailing_dots.to_string()
                }
            } else {
                without_trailing_dots.to_string()
            };
            stem = truncate_utf16_units(&stem, 160)
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

    let trimmed = truncate_utf16_units(cleaned.trim_end_matches([' ', '.']), 180)
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

    let sniffed_ext = sniff_extension_from_bytes(bytes).map(str::to_string);
    let mime_ext = extension_from_mime(mime).map(str::to_string);
    let url_ext =
        extract_extension_from_url(final_url).or_else(|| extract_extension_from_url(url));
    let detected_payload_ext = sniffed_ext.clone().or_else(|| mime_ext.clone());
    let detected_family = detected_payload_ext
        .as_deref()
        .map(media_family_of)
        .unwrap_or(MediaFamily::Other);

    let synthetic_placeholder_stem = is_synthetic_media_placeholder_with_ext(suggested_name);
    let raw_known_suggested_ext = extract_extension_from_name(suggested_name)
        .filter(|e| is_known_file_extension(e));
    let known_cd_ext = cd_clean
        .and_then(extract_extension_from_name)
        .filter(|e| is_known_file_extension(e));
    let fallback_suggested_ext = extract_extension_from_name(suggested_name);
    let fallback_cd_ext = cd_clean.and_then(extract_extension_from_name);

    // If `suggested_name` is a synthetic placeholder with ext (like `image.jpg` or `sticker_1_2.png`)
    // and we actually detected a real media/file format from bytes/MIME/CD/URL, let the real
    // format override the placeholder extension instead of mislabeling PNG/WebP/GIF as `.jpg`.
    let real_override_for_placeholder = synthetic_placeholder_stem.is_some()
        && (known_cd_ext.is_some()
            || matches!(
                detected_family,
                MediaFamily::Image | MediaFamily::Video | MediaFamily::Audio
            )
            || url_ext.is_some());

    // If actual bytes/MIME prove this is an image/video/audio payload, ignore a non-media suffix
    // accidentally parsed from a caption or display name (e.g., "Họp lúc 10.h", "Khu A.c", "Bùi V.C").
    let known_suggested_ext = if real_override_for_placeholder {
        None
    } else if let Some(ref sugg) = raw_known_suggested_ext {
        if matches!(
            detected_family,
            MediaFamily::Image | MediaFamily::Video | MediaFamily::Audio
        ) && !extensions_family_compatible(
            sugg,
            detected_payload_ext.as_deref().unwrap_or(""),
        ) {
            None
        } else {
            Some(sugg.clone())
        }
    } else {
        None
    };

    let base_name_owned: String = if is_generic_placeholder_name(suggested_name) {
        cd_clean
            .unwrap_or_else(|| {
                let s = suggested_name.trim();
                if s.is_empty() {
                    "download"
                } else {
                    s
                }
            })
            .to_string()
    } else if let Some(ref placeholder_stem) = synthetic_placeholder_stem {
        if let Some(cd) = cd_clean {
            cd.to_string()
        } else if real_override_for_placeholder {
            placeholder_stem.clone()
        } else {
            suggested_name.trim().to_string()
        }
    } else {
        suggested_name.trim().to_string()
    };

    // For unambiguous image/video/audio magic bytes, trust actual bytes over a potentially stale
    // CDN URL extension (e.g., Zalo CDN serving WebP bytes from a `.jpg` URL).
    let sniffed_media_ext = sniffed_ext.clone().filter(|e| {
        matches!(
            media_family_of(e),
            MediaFamily::Image | MediaFamily::Video | MediaFamily::Audio
        )
    });

    let resolved_ext = known_suggested_ext
        .or(known_cd_ext)
        .or(sniffed_media_ext)
        .or(url_ext)
        .or(mime_ext)
        .or(sniffed_ext)
        .or_else(|| is_zip_mime(mime).then(|| "zip".to_string()))
        .or(raw_known_suggested_ext)
        .or(fallback_suggested_ext)
        .or(fallback_cd_ext);

    let final_name =
        sanitize_suggested_filename_preserving_ext(&base_name_owned, resolved_ext.as_deref());
    (final_name, resolved_ext)
}

pub fn ensure_save_path_extension(path: PathBuf, expected_ext: Option<&str>) -> PathBuf {
    let Some(ext) = expected_ext
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| is_plausible_extension(e))
    else {
        return path;
    };

    let expected_family = media_family_of(&ext);
    let cur_opt = path.extension().and_then(|e| e.to_str());
    let should_append = match cur_opt {
        None => true,
        Some(cur) => {
            let c = cur.trim();
            if c.is_empty() || c.chars().all(|ch| ch.is_ascii_digit()) {
                true
            } else if c.eq_ignore_ascii_case(&ext) {
                false
            } else if c.len() == 1 {
                // Single-letter suffix (e.g., Vietnamese "10.h" or "khu A.c") is never an
                // intentional extension replacement for a different file type.
                true
            } else if matches!(
                expected_family,
                MediaFamily::Image | MediaFamily::Video | MediaFamily::Audio
            ) {
                !extensions_family_compatible(c, &ext)
            } else {
                !is_known_file_extension(c)
            }
        }
    };

    if !should_append {
        return path;
    }

    let mut raw: OsString = path.into_os_string();
    if let Some(s) = raw.to_str() {
        // Trim trailing dots/spaces strictly within the final filename component.
        let sep_end = s.rfind(['\\', '/']).map(|i| i + 1).unwrap_or(0);
        let (dir_part, file_part) = s.split_at(sep_end);
        let trimmed_file = file_part.trim_end_matches(['.', ' ']);
        let mut rebuilt = String::with_capacity(s.len() + ext.len() + 1);
        rebuilt.push_str(dir_part);
        if trimmed_file.is_empty() {
            rebuilt.push_str("download");
        } else {
            rebuilt.push_str(trimmed_file);
        }
        rebuilt.push('.');
        rebuilt.push_str(&ext);
        return PathBuf::from(rebuilt);
    }
    raw.push(".");
    raw.push(&ext);
    PathBuf::from(raw)
}

/// Builds an RFC 6266 / RFC 5987 `Content-Disposition: inline; filename="..."; filename*=UTF-8''...`
/// header value so WebView2 native "Save image as..." and `<video>`/`<audio>` Download controls
/// always propose a safe filename with the resolved file extension instead of `/resolve`.
pub fn build_inline_content_disposition(file_name: &str) -> String {
    let clean = sanitize_suggested_filename_preserving_ext(file_name, None);
    let ext_opt = extract_extension_from_name(&clean);
    let stem_raw = if let Some(ref ext) = ext_opt {
        let drop_len = ext.len() + 1;
        if clean.len() > drop_len {
            &clean[..clean.len() - drop_len]
        } else {
            "media"
        }
    } else {
        clean.as_str()
    };

    let ascii_stem: String = stem_raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches([' ', '.', '_'])
        .to_string();
    let ascii_stem_final = if ascii_stem.is_empty() {
        "media".to_string()
    } else {
        ascii_stem
    };
    let ascii_name = match ext_opt {
        Some(ref ext) => format!("{ascii_stem_final}.{ext}"),
        None => ascii_stem_final,
    };

    let mut utf8_encoded = String::with_capacity(clean.len() * 3);
    for &b in clean.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            utf8_encoded.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(utf8_encoded, "%{b:02X}");
        }
    }
    format!("inline; filename=\"{ascii_name}\"; filename*=UTF-8''{utf8_encoded}")
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
            | "msu"
            | "ps1"
            | "psm1"
            | "psd1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "sct"
            | "wsc"
            | "scr"
            | "pif"
            | "hta"
            | "cpl"
            | "jar"
            | "lnk"
            | "url"
            | "scf"
            | "msc"
            | "chm"
            | "iso"
            | "img"
            | "vhd"
            | "vhdx"
            | "appref-ms"
            | "application"
            | "settingcontent-ms"
            | "library-ms"
            | "search-ms"
            | "searchconnector-ms"
            | "docm"
            | "xlsm"
            | "pptm"
            | "xlsb"
            | "py"
            | "pyw"
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

#[cfg(target_os = "windows")]
fn resolve_explorer_path_from(system_root: Option<OsString>) -> PathBuf {
    let root = system_root
        .and_then(|v| {
            if v.to_str().is_some_and(|s| s.trim().is_empty()) {
                return None;
            }
            let p = PathBuf::from(v);
            p.is_absolute().then_some(p)
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("explorer.exe")
}

#[cfg(target_os = "windows")]
fn resolve_explorer_path() -> PathBuf {
    resolve_explorer_path_from(std::env::var_os("SystemRoot"))
}

pub fn open_url_with_shell(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err("URL không hợp lệ".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("Chỉ hỗ trợ mở liên kết http/https".into());
    }
    #[cfg(target_os = "windows")]
    {
        win32::shell_open_url(trimmed)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(trimmed)
            .spawn()
            .map_err(|e| format!("Không mở được trình duyệt: {e}"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(trimmed)
            .spawn()
            .map_err(|e| format!("Không mở được trình duyệt: {e}"))?;
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
        let explorer = resolve_explorer_path();
        std::process::Command::new(explorer)
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

pub fn open_directory(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err("Không tìm thấy thư mục trên máy".into());
    }
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let clean = normalize_os_display_path(&resolved);
    #[cfg(target_os = "windows")]
    {
        let path_str = clean
            .to_str()
            .ok_or_else(|| "Đường dẫn thư mục chứa ký tự không hợp lệ".to_string())?;
        if path_str.is_empty()
            || path_str.contains('"')
            || path_str.contains('\0')
            || path_str.chars().any(char::is_control)
        {
            return Err("Đường dẫn thư mục không hợp lệ".into());
        }
        let explorer = resolve_explorer_path();
        std::process::Command::new(explorer)
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục trong Explorer: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục: {e}"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&clean)
            .spawn()
            .map_err(|e| format!("Không thể mở thư mục: {e}"))?;
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
        fn SHBrowseForFolderW(lpbi: *mut BrowseInfoW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, psz_path: *mut u16) -> i32;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(pv: *mut c_void);
    }

    #[repr(C)]
    struct BrowseInfoW {
        hwnd_owner: *mut c_void,
        pidl_root: *const c_void,
        psz_display_name: *mut u16,
        lpsz_title: *const u16,
        ul_flags: u32,
        lpfn: *mut c_void,
        l_param: isize,
        i_image: i32,
    }

    const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
    const BIF_EDITBOX: u32 = 0x0000_0010;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const SW_SHOWNORMAL: i32 = 1;

    pub fn run_pick_directory(owner_hwnd: Option<usize>) -> Result<Option<PathBuf>, String> {
        let hr = unsafe {
            CoInitializeEx(
                null_mut(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };
        let mut display_buf = vec![0u16; 260];
        let title: Vec<u16> = "Chọn thư mục lưu trữ Media cục bộ cho Teza\0"
            .encode_utf16()
            .collect();
        let flags = if hr >= 0 {
            BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX
        } else {
            BIF_RETURNONLYFSDIRS | BIF_EDITBOX
        };
        let mut bi = BrowseInfoW {
            hwnd_owner: owner_hwnd.map(|h| h as *mut c_void).unwrap_or(null_mut()),
            pidl_root: null(),
            psz_display_name: display_buf.as_mut_ptr(),
            lpsz_title: title.as_ptr(),
            ul_flags: flags,
            lpfn: null_mut(),
            l_param: 0,
            i_image: 0,
        };

        let pidl = unsafe { SHBrowseForFolderW(&mut bi) };
        if pidl.is_null() {
            if hr >= 0 {
                unsafe {
                    CoUninitialize();
                }
            }
            return Ok(None);
        }

        let mut path_buf = vec![0u16; 1024];
        let ok = unsafe { SHGetPathFromIDListW(pidl, path_buf.as_mut_ptr()) };
        unsafe {
            CoTaskMemFree(pidl);
        }
        if hr >= 0 {
            unsafe {
                CoUninitialize();
            }
        }
        if ok == 0 {
            return Err(
                "Không thể đọc đường dẫn thư mục đã chọn (vui lòng chọn thư mục cục bộ hợp lệ)"
                    .into(),
            );
        }
        let end = path_buf
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(path_buf.len());
        let path_str = String::from_utf16(&path_buf[..end]).map_err(|e| e.to_string())?;
        let trimmed = path_str.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(trimmed)))
    }

    fn shell_execute_open(target: &str, err_prefix: &str) -> Result<(), String> {
        if target.is_empty() || target.contains('\0') {
            return Err(format!("{err_prefix}: mục tiêu không hợp lệ"));
        }
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = format!("{target}\0").encode_utf16().collect();
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
            return Err(format!("{err_prefix} (mã {code})"));
        }
        Ok(())
    }

    pub fn shell_open_file(path: &std::path::Path) -> Result<(), String> {
        let Some(path_str) = path.to_str() else {
            return Err("Đường dẫn tệp chứa ký tự không hợp lệ".into());
        };
        if path_str.contains('\0') {
            return Err("Đường dẫn tệp không hợp lệ".into());
        }
        shell_execute_open(path_str, "Không thể mở tệp bằng ứng dụng mặc định")
    }

    pub fn shell_open_url(url: &str) -> Result<(), String> {
        shell_execute_open(url, "Không mở được trình duyệt")
    }

    const MB_YESNO: u32 = 0x0000_0004;
    const MB_ICONWARNING: u32 = 0x0000_0030;
    const MB_DEFBUTTON2: u32 = 0x0000_0100;
    const IDYES: i32 = 6;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct SaveDialogConfig {
        pub effective_ext: Option<String>,
        pub safe_name: String,
        pub filter_str: String,
        /// Value passed to Win32 `OPENFILENAMEW.lpstrDefExt` (without trailing NUL).
        /// Win32 `GetSaveFileNameW` silently truncates `lpstrDefExt` to 3 characters when the
        /// wildcard `*.*` filter is active (turning `.xlsx` -> `.xls`, `.docx` -> `.doc`,
        /// `.avif` -> `.avi`, `.webp` -> `.web`). For extensions > 3 chars we pass `Some("")`
        /// (non-NULL empty string) so filter 1 (`*.webp`, `*.xlsx`) still auto-appends the full
        /// extension while `*.*` appends nothing and `ensure_save_path_extension` guarantees the
        /// exact full extension afterward.
        pub lpstr_def_ext: Option<String>,
    }

    pub(super) fn build_save_dialog_config(
        suggested_name: &str,
        default_ext: Option<&str>,
    ) -> SaveDialogConfig {
        let effective_ext = default_ext
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| is_plausible_extension(e))
            .or_else(|| extract_extension_from_name(suggested_name));

        let filter_str = if let Some(ref ext) = effective_ext {
            format!(
                "Tệp {} (*.{ext})\0*.{ext}\0Tất cả tệp (*.*)\0*.*\0\0",
                ext.to_ascii_uppercase()
            )
        } else {
            "Tất cả tệp (*.*)\0*.*\0\0".to_string()
        };

        let lpstr_def_ext = effective_ext.as_deref().map(|ext| {
            if ext.len() <= 3 {
                ext.to_string()
            } else {
                String::new()
            }
        });

        let safe_name =
            sanitize_suggested_filename_preserving_ext(suggested_name, effective_ext.as_deref());

        SaveDialogConfig {
            effective_ext,
            safe_name,
            filter_str,
            lpstr_def_ext,
        }
    }

    pub fn run_dialog(
        owner_hwnd: Option<usize>,
        save_mode: bool,
        suggested_name: &str,
        default_ext: Option<&str>,
    ) -> Result<Option<PathBuf>, String> {
        let save_cfg = save_mode.then(|| build_save_dialog_config(suggested_name, default_ext));
        let effective_ext = save_cfg.as_ref().and_then(|c| c.effective_ext.clone());

        let filter_str = if let Some(ref cfg) = save_cfg {
            cfg.filter_str.clone()
        } else {
            "Tất cả tệp (*.*)\0*.*\0Hình ảnh (*.png;*.jpg;*.jpeg;*.webp;*.gif)\0*.png;*.jpg;*.jpeg;*.webp;*.gif\0\0"
                .to_string()
        };
        let filter: Vec<u16> = filter_str.encode_utf16().collect();
        let def_ext_wide: Option<Vec<u16>> = save_cfg
            .as_ref()
            .and_then(|c| c.lpstr_def_ext.as_deref())
            .map(|ext| format!("{ext}\0").encode_utf16().collect());

        let title_str = if save_mode {
            "Lưu tệp về máy\0"
        } else {
            "Chọn tệp hoặc hình ảnh để gửi\0"
        };
        let title: Vec<u16> = title_str.encode_utf16().collect();
        let mut file_buf = vec![0u16; 32_768];

        if let Some(ref cfg) = save_cfg {
            let encoded: Vec<u16> = cfg.safe_name.encode_utf16().collect();
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
            sanitize_suggested_filename_preserving_ext("CON.foo.pdf", Some("pdf")),
            "_CON.foo.pdf"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("nul.tar.gz", Some("gz")),
            "_nul.tar.gz"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("nul.tar.gz", None),
            "_nul.tar.gz"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("aux .backup.zip", Some("zip")),
            "_aux .backup.zip"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("LPT9.a.b.c", Some("c")),
            "_LPT9.a.b.c"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("normal.CON.pdf", Some("pdf")),
            "normal.CON.pdf"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("console.foo.pdf", Some("pdf")),
            "console.foo.pdf"
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
            "link.url",
            "cmd.scf",
            "snapin.msc",
            "help.chm",
            "disk.iso",
            "image.img",
            "drive.vhd",
            "drive.vhdx",
            "clickonce.appref-ms",
            "clickonce.application",
            "patch.msu",
            "setting.settingcontent-ms",
            "lib.library-ms",
            "query.search-ms",
            "conn.searchconnector-ms",
            "scriptlet.sct",
            "component.wsc",
            "report.docm",
            "sheet.xlsm",
            "slides.pptm",
            "binary.xlsb",
            "tool.py",
            "gui.pyw",
        ] {
            assert!(
                is_dangerous_executable_ext(Path::new(dangerous)),
                "expected blocked: {dangerous}"
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_explorer_path_uses_system_root_with_safe_fallback() {
        assert_eq!(
            resolve_explorer_path_from(Some(OsString::from(r"C:\Windows"))),
            PathBuf::from(r"C:\Windows\explorer.exe")
        );
        assert_eq!(
            resolve_explorer_path_from(Some(OsString::from(r"D:\WINNT"))),
            PathBuf::from(r"D:\WINNT\explorer.exe")
        );
        assert_eq!(
            resolve_explorer_path_from(None),
            PathBuf::from(r"C:\Windows\explorer.exe")
        );
        assert_eq!(
            resolve_explorer_path_from(Some(OsString::from("   "))),
            PathBuf::from(r"C:\Windows\explorer.exe")
        );
        assert_eq!(
            resolve_explorer_path_from(Some(OsString::from(r"relative\dir"))),
            PathBuf::from(r"C:\Windows\explorer.exe")
        );
    }

    #[test]
    fn open_url_with_shell_rejects_non_http_and_control_chars() {
        for bad in [
            "",
            "   ",
            "javascript:alert(1)",
            "file:///C:/Windows/win.ini",
            r"C:\Windows\System32\calc.exe",
            "https://example.com\n/foo",
            "https://example.com/\0bar",
            "ftp://example.com/file",
        ] {
            assert!(
                open_url_with_shell(bad).is_err(),
                "expected open_url_with_shell to reject: {bad:?}"
            );
        }
    }

    #[test]
    fn open_directory_rejects_missing_path_and_regular_files() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_open_dir_test_{stamp}"));
        let missing = dir.join("does_not_exist");
        assert!(open_directory(&missing).is_err());

        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plain.txt");
        std::fs::write(&file, b"hi").unwrap();
        assert!(open_directory(&file).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_images_stickers_captions_with_dots_and_teza_media_urls() {
        let png_hdr = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let jpeg_hdr = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let webp_hdr = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
        let gif_hdr = b"GIF89a\x01\x00\x01\x00";
        let heic_hdr = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00heic";
        let avif_mif1_hdr = b"\x00\x00\x00\x1cftypmif1\x00\x00\x00\x00mif1avif";

        // Synthetic parser placeholder `image.jpg` must adopt true PNG/WebP/GIF magic bytes
        assert_eq!(
            resolve_download_filename_and_ext(
                "image.jpg",
                None,
                "https://photo-stal-6.zdn.vn/a/blob",
                "https://photo-stal-6.zdn.vn/a/blob",
                "application/octet-stream",
                png_hdr,
            ),
            ("image.png".into(), Some("png".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "image.jpg",
                None,
                "https://photo-stal-6.zdn.vn/a/pic.jpg",
                "https://photo-stal-6.zdn.vn/a/pic.jpg",
                "image/webp",
                webp_hdr,
            ),
            ("image.webp".into(), Some("webp".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "sticker_12_345.png",
                None,
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "application/octet-stream",
                webp_hdr,
            ),
            ("sticker_12_345.webp".into(), Some("webp".into()))
        );

        // Photo captions & display names with dots colliding with code/non-image exts (.h, .c, .ai, .go)
        assert_eq!(
            resolve_download_filename_and_ext(
                "Họp lúc 10.h",
                None,
                "https://photo-stal-6.zdn.vn/blob",
                "https://photo-stal-6.zdn.vn/blob",
                "image/jpeg",
                &jpeg_hdr,
            ),
            ("Họp lúc 10.h.jpg".into(), Some("jpg".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "Nguyễn V.C",
                None,
                "https://s120-ava-talk.zadn.vn/avatar",
                "https://s120-ava-talk.zadn.vn/avatar",
                "application/octet-stream",
                &jpeg_hdr,
            ),
            ("Nguyễn V.C.jpg".into(), Some("jpg".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "photo 2026.11.26",
                None,
                "https://photo-stal-6.zdn.vn/blob",
                "https://photo-stal-6.zdn.vn/blob",
                "application/octet-stream",
                gif_hdr,
            ),
            ("photo 2026.11.26.gif".into(), Some("gif".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "Ảnh chụp từ iPhone",
                None,
                "https://photo-stal-6.zdn.vn/blob",
                "https://photo-stal-6.zdn.vn/blob",
                "application/octet-stream",
                heic_hdr,
            ),
            ("Ảnh chụp từ iPhone.heic".into(), Some("heic".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "사진",
                Some("holiday_shot.avif"),
                "https://photo-stal-6.zdn.vn/blob",
                "https://photo-stal-6.zdn.vn/blob",
                "application/octet-stream",
                avif_mif1_hdr,
            ),
            ("holiday_shot.avif".into(), Some("avif".into()))
        );

        // `teza-media://` and `http://teza-media.localhost/resolve?url=...` unwrapping
        assert_eq!(
            extract_extension_from_url(
                "http://teza-media.localhost/resolve?url=https%3A%2F%2Ff10-zpc.zdn.vn%2Fa%2Fpic.webp%3Ft%3D1&kind=image&t=tok123"
            ),
            Some("webp".to_string())
        );
        assert_eq!(
            extract_extension_from_url(
                "teza-media://localhost/resolve?url=https%3A%2F%2Ffile-stal-9.dlfl.vn%2Fdoc%2520final.pdf&kind=file&t=tok123"
            ),
            Some("pdf".to_string())
        );
        assert_eq!(
            extract_extension_from_url(
                "https://file-stal-9.dlfl.vn/download?filename=B%C3%A1o%20c%C3%A1o.xlsx&token=xyz"
            ),
            Some("xlsx".to_string())
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "teza-image",
                None,
                "http://teza-media.localhost/resolve?url=https%3A%2F%2Ff10-zpc.zdn.vn%2Fa%2Fpic.png&kind=image&t=tok",
                "http://teza-media.localhost/resolve?url=https%3A%2F%2Ff10-zpc.zdn.vn%2Fa%2Fpic.png&kind=image&t=tok",
                "application/octet-stream",
                b"",
            ),
            ("teza-image.png".into(), Some("png".into()))
        );

        // `teza-video`, `teza-voice`, `teza-sticker` fallbacks must yield to real Content-Disposition / original_name
        // and adopt true sniffed extensions when given with synthetic extensions
        assert_eq!(
            resolve_download_filename_and_ext(
                "teza-video",
                Some("demo_clip.mov"),
                "https://video-stal-11.dlmd.me/v/raw",
                "https://video-stal-11.dlmd.me/v/raw",
                "video/quicktime",
                b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00",
            ),
            ("demo_clip.mov".into(), Some("mov".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "teza-voice",
                Some("ghi_am_cuoc_hop.m4a"),
                "https://voice-stal-1.dlmd.me/v/raw",
                "https://voice-stal-1.dlmd.me/v/raw",
                "audio/mp4",
                b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00",
            ),
            ("ghi_am_cuoc_hop.m4a".into(), Some("m4a".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "teza-sticker",
                Some("cat_wave.webp"),
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "image/webp",
                webp_hdr,
            ),
            ("cat_wave.webp".into(), Some("webp".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "teza-sticker.png",
                None,
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc",
                "application/octet-stream",
                webp_hdr,
            ),
            ("teza-sticker.webp".into(), Some("webp".into()))
        );
    }

    #[test]
    fn sniffs_voice_audio_video_archives_and_macro_ooxml_extensions() {
        // AAC ADTS & AMR voice messages with octet-stream and extensionless URLs
        let aac_adts = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x80, 0xFC, 0x00];
        assert_eq!(
            resolve_download_filename_and_ext(
                "Tin nhắn thoại",
                None,
                "https://voice-stal-1.dlmd.me/v/raw",
                "https://voice-stal-1.dlmd.me/v/raw",
                "application/octet-stream",
                &aac_adts,
            ),
            ("Tin nhắn thoại.aac".into(), Some("aac".into()))
        );
        assert_eq!(
            resolve_download_filename_and_ext(
                "attachment",
                None,
                "https://voice-stal-1.dlmd.me/v/raw",
                "https://voice-stal-1.dlmd.me/v/raw",
                "application/octet-stream",
                b"#!AMR\n\x3c\x00",
            ),
            ("attachment.amr".into(), Some("amr".into()))
        );
        assert_eq!(
            sniff_extension_from_bytes(b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00"),
            Some("m4a")
        );
        assert_eq!(
            sniff_extension_from_bytes(b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00"),
            Some("mov")
        );
        assert_eq!(sniff_extension_from_bytes(b"OggS\x00\x02"), Some("ogg"));
        assert_eq!(sniff_extension_from_bytes(b"RIFF\x24\x00\x00\x00WAVEfmt "), Some("wav"));
        assert_eq!(sniff_extension_from_bytes(b"%PDF-1.7\n"), Some("pdf"));
        assert_eq!(sniff_extension_from_bytes(b"Rar!\x1a\x07\x01\x00"), Some("rar"));
        assert_eq!(
            sniff_extension_from_bytes(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0x00]),
            Some("7z")
        );
        assert_eq!(sniff_extension_from_bytes(b"{\\rtf1\\ansi"), Some("rtf"));

        // Macro-enabled OOXML & ODF & genuine code files
        let mut xlsm = fake_zip_with_entry(b"xl/workbook.xml");
        xlsm.extend_from_slice(b"xl/vbaProject.bin");
        assert_eq!(sniff_extension_from_bytes(&xlsm), Some("xlsm"));
        assert_eq!(
            sniff_extension_from_bytes(&fake_zip_with_entry(
                b"mimetypeapplication/vnd.oasis.opendocument.text"
            )),
            Some("odt")
        );

        // Real `.h` / `.c` source files remain untouched when MIME is text/plain
        assert_eq!(
            resolve_download_filename_and_ext(
                "config.h",
                None,
                "https://file-stal-9.dlfl.vn/blob",
                "https://file-stal-9.dlfl.vn/blob",
                "text/plain",
                b"#pragma once\n",
            ),
            ("config.h".into(), Some("h".into()))
        );
    }

    #[test]
    fn sanitize_and_ensure_save_path_handle_aliases_bare_ext_trailing_spaces_and_media_families() {
        assert_eq!(
            sanitize_suggested_filename_preserving_ext(".jpg", Some("jpg")),
            "download.jpg"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("photo.JPEG", Some("jpg")),
            "photo.jpg"
        );
        assert_eq!(
            sanitize_suggested_filename_preserving_ext("photo.tif", Some("tiff")),
            "photo.tiff"
        );

        // Trailing dot + spaces must not produce `photo. .jpg`
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\Users\Mr.Smith\photo.   "), Some("jpg")),
            PathBuf::from(r"C:\Users\Mr.Smith\photo.jpg")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\Users\Mr.Smith\photo"), Some("webp")),
            PathBuf::from(r"C:\Users\Mr.Smith\photo.webp")
        );
        // Vietnamese time/zone suffix (`10.h`, `khu A.c`) or non-image suffix on an image must keep `.jpg`
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Anh chup 10.h"), Some("jpg")),
            PathBuf::from(r"C:\dir\Anh chup 10.h.jpg")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\Tang 2 khu A.c"), Some("png")),
            PathBuf::from(r"C:\dir\Tang 2 khu A.c.png")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\my_photo.go"), Some("jpg")),
            PathBuf::from(r"C:\dir\my_photo.go.jpg")
        );
        // Intentional image-family rename (e.g. `.jpg` -> `.png` or `.JPEG`) is respected
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\my_photo.png"), Some("jpg")),
            PathBuf::from(r"C:\dir\my_photo.png")
        );
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\my_photo.JPEG"), Some("jpg")),
            PathBuf::from(r"C:\dir\my_photo.JPEG")
        );
        // Uncommon extension preserved when user types a date/dot suffix
        assert_eq!(
            ensure_save_path_extension(PathBuf::from(r"C:\dir\mockup 26.Nov"), Some("fig")),
            PathBuf::from(r"C:\dir\mockup 26.Nov.fig")
        );
    }

    #[test]
    fn build_inline_content_disposition_preserves_extensions_and_utf8_safely() {
        let cd = build_inline_content_disposition("Hình ảnh 26.11.jpg");
        assert!(cd.starts_with("inline; filename=\""));
        assert!(cd.contains(".jpg\""));
        assert!(cd.contains("filename*=UTF-8''"));
        assert!(cd.ends_with(".jpg"));
        assert!(!cd.contains('\r') && !cd.contains('\n'));

        let cd_inject = build_inline_content_disposition("evil\"\r\nX-Injected: 1.png");
        assert!(!cd_inject.contains('\r') && !cd_inject.contains('\n'));
        assert!(cd_inject.ends_with(".png"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn win32_save_dialog_config_prevents_3char_lpstr_def_ext_truncation() {
        // <= 3 char extensions keep lpstrDefExt for native filter + *.* fallback
        let jpg_cfg = win32::build_save_dialog_config("Hình ảnh", Some("jpg"));
        assert_eq!(jpg_cfg.effective_ext.as_deref(), Some("jpg"));
        assert_eq!(jpg_cfg.safe_name, "Hình ảnh.jpg");
        assert_eq!(jpg_cfg.lpstr_def_ext.as_deref(), Some("jpg"));
        assert!(jpg_cfg.filter_str.starts_with("Tệp JPG (*.jpg)\0*.jpg\0"));

        // 4+ char extensions must pass empty string "" (not "webp"/"xlsx"/"avif") so Win32
        // GetSaveFileNameW never truncates to `.web`/`.xls`/`.avi` under `*.*` filter!
        for ext in ["webp", "jpeg", "heic", "avif", "xlsx", "docx", "pptx", "webm"] {
            let cfg = win32::build_save_dialog_config("test", Some(ext));
            assert_eq!(cfg.effective_ext.as_deref(), Some(ext));
            assert_eq!(cfg.safe_name, format!("test.{ext}"));
            assert_eq!(
                cfg.lpstr_def_ext.as_deref(),
                Some(""),
                "expected empty lpstrDefExt for 4+ char ext .{ext}"
            );
            assert!(cfg.filter_str.contains(&format!("*.{ext}\0")));
        }

        // No extension -> None
        let none_cfg = win32::build_save_dialog_config("readme", None);
        assert_eq!(none_cfg.effective_ext, None);
        assert_eq!(none_cfg.lpstr_def_ext, None);
        assert_eq!(none_cfg.filter_str, "Tất cả tệp (*.*)\0*.*\0\0");
    }
}
