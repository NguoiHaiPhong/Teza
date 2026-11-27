//! Hybrid Local Managed Media Vault: content-addressed storage with O(1) RAM 64 KiB
//! chunked AES-256-GCM encryption at rest, DPAPI-sealed vault key, singleflight download
//! deduplication, and SQLite metadata index integration.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};

use crate::data_dir::{self, StoragePaths};
use crate::dpapi;
use crate::session::{normalize_allowed_media_url, MediaDownloadError, Session};
use crate::store::{
    MediaObjectRecord, MediaVaultStatsDto, MessageMediaRecord, MessageStore,
    MEDIA_CDN_STATUS_EXPIRED, MEDIA_CDN_STATUS_OK, MEDIA_CDN_STATUS_UNKNOWN,
    MEDIA_LOCAL_STATE_LOCAL,
};

const DEFAULT_QUOTA_MB: u64 = 1024;
const DOWNLOAD_CONCURRENCY_LIMIT: usize = 4;
const TOUCH_FLUSH_THRESHOLD: usize = 32;
const IO_BUF_LEN: usize = 64 * 1024;
const MIME_PROBE_WINDOW_BYTES: usize = 64 * 1024;
const CLEAR_CACHE_TMP_STALE_AGE: Duration = Duration::from_secs(3600);

static NEXT_VAULT_TMP_ID: AtomicU64 = AtomicU64::new(1);

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn bytes_contain_head_or_tail(bytes: &[u8], needle: &[u8]) -> bool {
    let head_len = bytes.len().min(MIME_PROBE_WINDOW_BYTES);
    if contains_bytes(&bytes[..head_len], needle) {
        return true;
    }
    if bytes.len() > MIME_PROBE_WINDOW_BYTES {
        let tail_start = bytes.len() - MIME_PROBE_WINDOW_BYTES;
        return contains_bytes(&bytes[tail_start..], needle);
    }
    false
}

/// Returns `true` when `mime` is missing, generic octet-stream, or a generic ZIP container
/// MIME that should be refined via magic-byte sniffing (e.g., to distinguish images/audio/video
/// or OOXML/ODF documents from generic blobs).
pub fn is_generic_mime(mime: &str) -> bool {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        ""
            | "application/octet-stream"
            | "binary/octet-stream"
            | "application/binary"
            | "application/zip"
            | "application/x-zip-compressed"
            | "application/x-zip"
            | "multipart/x-zip"
    )
}

fn is_zip_container_mime(mime: &str) -> bool {
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

fn sniff_isobmff_mime(bytes: &[u8]) -> &'static str {
    let major = &bytes[8..12];
    match major {
        b"M4A " | b"M4B " => "audio/mp4",
        b"qt  " => "video/quicktime",
        b"avif" | b"avis" => "image/avif",
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" => "image/heic",
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
                    return "image/avif";
                }
                if matches!(
                    brand,
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis"
                ) {
                    return "image/heic";
                }
                off += 4;
            }
            "image/heic"
        }
        b"3gp4" | b"3gp5" | b"3gp6" | b"3gp7" | b"3g2a" | b"3g2b" => "video/3gpp",
        _ => "video/mp4",
    }
}

/// Sniffs a safe, concrete MIME type from file magic bytes (never returns `image/svg+xml`).
pub fn sniff_mime_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" {
        match &bytes[8..12] {
            b"WEBP" => return Some("image/webp"),
            b"WAVE" => return Some("audio/wav"),
            b"AVI " => return Some("video/x-msvideo"),
            _ => {}
        }
    }
    if bytes.starts_with(b"BM") && bytes.len() >= 14 && bytes[6..10] == [0, 0, 0, 0] {
        return Some("image/bmp");
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some("image/tiff");
    }
    if bytes.len() >= 6
        && bytes[0..4] == [0x00, 0x00, 0x01, 0x00]
        && u16::from_le_bytes([bytes[4], bytes[5]]) > 0
    {
        return Some("image/x-icon");
    }
    if bytes.starts_with(&[
        0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20, 0x0D, 0x0A, 0x87, 0x0A,
    ]) {
        return Some("image/jxl");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if bytes.starts_with(b"Rar!\x1a\x07\x00") || bytes.starts_with(b"Rar!\x1a\x07\x01\x00") {
        return Some("application/vnd.rar");
    }
    if bytes.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some("application/x-7z-compressed");
    }
    if bytes.starts_with(&[0x1F, 0x8B, 0x08]) {
        return Some("application/gzip");
    }
    if bytes.len() >= 10
        && bytes.starts_with(b"BZh")
        && bytes[3].is_ascii_digit()
        && &bytes[4..10] == b"\x31\x41\x59\x26\x53\x59"
    {
        return Some("application/x-bzip2");
    }
    if bytes.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Some("application/x-xz");
    }
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some("application/zstd");
    }
    if bytes.starts_with(b"{\\rtf") {
        return Some("application/rtf");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(sniff_isobmff_mime(bytes));
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let head = &bytes[..bytes.len().min(64)];
        if head
            .windows(4)
            .any(|w| w.eq_ignore_ascii_case(b"webm"))
        {
            return Some("video/webm");
        }
        return Some("video/x-matroska");
    }
    if bytes.starts_with(b"OggS") {
        return Some("audio/ogg");
    }
    if bytes.starts_with(b"fLaC") {
        return Some("audio/flac");
    }
    if bytes.starts_with(b"#!AMR\n") || bytes.starts_with(b"#!AMR-WB\n") {
        return Some("audio/amr");
    }
    if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2
            && bytes[0] == 0xFF
            && matches!(bytes[1], 0xFB | 0xFA | 0xF3 | 0xF2 | 0xE3 | 0xE2))
    {
        return Some("audio/mpeg");
    }
    if is_valid_aac_adts_header(bytes) {
        return Some("audio/aac");
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.bin") {
            return Some("application/vnd.ms-excel.sheet.binary.macroenabled.12");
        }
        if bytes_contain_head_or_tail(bytes, b"xl/workbook.xml")
            || bytes_contain_head_or_tail(bytes, b"xl/worksheets/")
        {
            if bytes_contain_head_or_tail(bytes, b"xl/vbaProject.bin") {
                return Some("application/vnd.ms-excel.sheet.macroenabled.12");
            }
            return Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
        }
        if bytes_contain_head_or_tail(bytes, b"word/document.xml") {
            if bytes_contain_head_or_tail(bytes, b"word/vbaProject.bin") {
                return Some("application/vnd.ms-word.document.macroenabled.12");
            }
            return Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document");
        }
        if bytes_contain_head_or_tail(bytes, b"ppt/presentation.xml") {
            if bytes_contain_head_or_tail(bytes, b"ppt/vbaProject.bin") {
                return Some("application/vnd.ms-powerpoint.presentation.macroenabled.12");
            }
            return Some(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            );
        }
        if bytes_contain_head_or_tail(bytes, b"mimetypeapplication/vnd.oasis.opendocument.text") {
            return Some("application/vnd.oasis.opendocument.text");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"mimetypeapplication/vnd.oasis.opendocument.spreadsheet",
        ) {
            return Some("application/vnd.oasis.opendocument.spreadsheet");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"mimetypeapplication/vnd.oasis.opendocument.presentation",
        ) {
            return Some("application/vnd.oasis.opendocument.presentation");
        }
        if bytes_contain_head_or_tail(bytes, b"mimetypeapplication/epub+zip") {
            return Some("application/epub+zip");
        }
        if bytes_contain_head_or_tail(bytes, b"AndroidManifest.xml") {
            return Some("application/vnd.android.package-archive");
        }
        return Some("application/zip");
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        if bytes_contain_head_or_tail(bytes, b"W\0o\0r\0d\0D\0o\0c\0u\0m\0e\0n\0t\0\0") {
            return Some("application/msword");
        }
        if bytes_contain_head_or_tail(
            bytes,
            b"P\0o\0w\0e\0r\0P\0o\0i\0n\0t\0 \0D\0o\0c\0u\0m\0e\0n\0t\0\0",
        ) {
            return Some("application/vnd.ms-powerpoint");
        }
        if bytes_contain_head_or_tail(bytes, b"W\0o\0r\0k\0b\0o\0o\0k\0\0")
            || bytes_contain_head_or_tail(bytes, b"B\0o\0o\0k\0\0")
        {
            return Some("application/vnd.ms-excel");
        }
    }
    None
}

/// Maps a known file extension (without leading dot) to a safe MIME type (never SVG).
pub fn mime_from_extension_safe(ext: &str) -> Option<&'static str> {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jfif" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "ico" => Some("image/x-icon"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "avif" => Some("image/avif"),
        "jxl" => Some("image/jxl"),
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        "webm" => Some("video/webm"),
        "mkv" => Some("video/x-matroska"),
        "avi" => Some("video/x-msvideo"),
        "wmv" => Some("video/x-ms-wmv"),
        "flv" => Some("video/x-flv"),
        "3gp" => Some("video/3gpp"),
        "ogv" => Some("video/ogg"),
        "mpg" | "mpeg" => Some("video/mpeg"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "ogg" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "amr" => Some("audio/amr"),
        "pdf" => Some("application/pdf"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "xlsm" => Some("application/vnd.ms-excel.sheet.macroenabled.12"),
        "xlsb" => Some("application/vnd.ms-excel.sheet.binary.macroenabled.12"),
        "xls" => Some("application/vnd.ms-excel"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "docm" => Some("application/vnd.ms-word.document.macroenabled.12"),
        "doc" => Some("application/msword"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "pptm" => Some("application/vnd.ms-powerpoint.presentation.macroenabled.12"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        "ods" => Some("application/vnd.oasis.opendocument.spreadsheet"),
        "odp" => Some("application/vnd.oasis.opendocument.presentation"),
        "epub" => Some("application/epub+zip"),
        "apk" => Some("application/vnd.android.package-archive"),
        "zip" => Some("application/zip"),
        "rar" => Some("application/vnd.rar"),
        "7z" => Some("application/x-7z-compressed"),
        "gz" | "tgz" => Some("application/gzip"),
        "tar" => Some("application/x-tar"),
        "bz2" => Some("application/x-bzip2"),
        "xz" => Some("application/x-xz"),
        "zst" => Some("application/zstd"),
        "txt" | "log" => Some("text/plain"),
        "md" => Some("text/markdown"),
        "csv" => Some("text/csv"),
        "tsv" => Some("text/tab-separated-values"),
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "rtf" => Some("application/rtf"),
        "html" | "htm" => Some("text/html"),
        _ => None,
    }
}

fn extract_ext_hint(s: &str) -> Option<&str> {
    let path = s.split(['?', '#']).next().unwrap_or("");
    let seg = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, ext) = seg.rsplit_once('.')?;
    if stem.trim_matches('.').is_empty() || ext.is_empty() || ext.len() > 10 {
        return None;
    }
    Some(ext)
}

fn resolve_effective_mime_from_probe(
    provided_mime: &str,
    probe: &[u8],
    original_name: Option<&str>,
    url_opt: Option<&str>,
) -> String {
    let trimmed = provided_mime.trim();
    if !is_generic_mime(trimmed) && !crate::session::is_forbidden_svg_mime(trimmed) {
        return trimmed.to_string();
    }
    if let Some(sniffed) = sniff_mime_from_bytes(probe) {
        return sniffed.to_string();
    }
    if is_zip_container_mime(trimmed) {
        return "application/zip".to_string();
    }
    if let Some(ext) = original_name
        .and_then(extract_ext_hint)
        .or_else(|| url_opt.and_then(extract_ext_hint))
    {
        if let Some(m) = mime_from_extension_safe(ext) {
            return m.to_string();
        }
    }
    "application/octet-stream".to_string()
}

fn read_file_head_and_tail_probe(path: &Path) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let total = meta.len();
    if total == 0 {
        return Vec::new();
    }
    let window = MIME_PROBE_WINDOW_BYTES as u64;
    if total <= window * 2 {
        let mut buf = Vec::with_capacity(total as usize);
        let _ = file.read_to_end(&mut buf);
        return buf;
    }
    let mut head = vec![0u8; MIME_PROBE_WINDOW_BYTES];
    if file.read_exact(&mut head).is_err() {
        return Vec::new();
    }
    if file.seek(SeekFrom::Start(total - window)).is_ok() {
        let mut tail = vec![0u8; MIME_PROBE_WINDOW_BYTES];
        if file.read_exact(&mut tail).is_ok() {
            head.extend_from_slice(&tail);
        }
    }
    head
}

fn slice_head_and_tail_probe(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if bytes.len() <= MIME_PROBE_WINDOW_BYTES * 2 {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(MIME_PROBE_WINDOW_BYTES * 2);
    out.extend_from_slice(&bytes[..MIME_PROBE_WINDOW_BYTES]);
    out.extend_from_slice(&bytes[bytes.len() - MIME_PROBE_WINDOW_BYTES..]);
    std::borrow::Cow::Owned(out)
}

fn default_quota_mb() -> u64 {
    DEFAULT_QUOTA_MB
}

fn default_true() -> bool {
    true
}

fn volatile_zeroize(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned mutable reference to a byte inside `bytes`.
        unsafe {
            std::ptr::write_volatile(b, 0);
        }
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

fn encode_hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn decode_sha256_hex(hex: &str) -> Result<[u8; 32]> {
    let trimmed = hex.trim();
    if trimmed.len() != 64 {
        bail!("SHA-256 hex không hợp lệ (độ dài {})", trimmed.len());
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let byte = u8::from_str_radix(&trimmed[i * 2..i * 2 + 2], 16)
            .with_context(|| format!("SHA-256 hex chứa ký tự không hợp lệ tại vị trí {i}"))?;
        *slot = byte;
    }
    Ok(out)
}

/// Generates a 32-byte CSPRNG token encoded as 64 lowercase hex characters (e.g., for `teza-media://` session auth).
pub fn generate_protocol_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("OS CSPRNG failure")?;
    let hex = encode_hex_lower(&bytes);
    volatile_zeroize(&mut bytes);
    Ok(hex)
}

/// Constant-time byte-slice comparison for session token verification.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MediaVaultConfig {
    #[serde(default)]
    pub custom_vault_dir: Option<String>,
    #[serde(default = "default_quota_mb")]
    pub quota_mb: u64,
    #[serde(default = "default_true")]
    pub encrypt_at_rest: bool,
    #[serde(default = "default_true")]
    pub auto_cache_images: bool,
    #[serde(default = "default_true")]
    pub auto_cache_voice: bool,
    #[serde(default)]
    pub auto_cache_video_files: bool,
}

impl Default for MediaVaultConfig {
    fn default() -> Self {
        Self {
            custom_vault_dir: None,
            quota_mb: DEFAULT_QUOTA_MB,
            encrypt_at_rest: true,
            auto_cache_images: true,
            auto_cache_voice: true,
            auto_cache_video_files: false,
        }
    }
}

impl MediaVaultConfig {
    pub fn load_from_file(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut cfg = serde_json::from_str::<Self>(&raw).unwrap_or_default();
        cfg.custom_vault_dir = cfg
            .custom_vault_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let mut clean = self.clone();
        clean.custom_vault_dir = clean
            .custom_vault_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let json = serde_json::to_string_pretty(&clean)?;
        data_dir::write_private_atomic(path, json.as_bytes())
    }
}

/// 256-bit vault encryption key with volatile zeroization on `Drop` and redacted `Debug`.
pub struct VaultKey {
    bytes: [u8; 32],
}

impl VaultKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    pub fn load_or_create(key_path: &Path) -> Result<Self> {
        if key_path.exists() {
            let sealed = std::fs::read_to_string(key_path)
                .with_context(|| format!("Không đọc được tệp khóa media {}", key_path.display()))?;
            let mut plain = dpapi::unprotect_secret(sealed.trim())
                .context("Không giải mã được khóa media vault bằng DPAPI")?;
            let mut raw = B64
                .decode(plain.trim())
                .context("Khóa media vault giải mã không hợp lệ")?;
            // SAFETY: Clearing UTF-8 string bytes immediately before drop.
            unsafe {
                volatile_zeroize(plain.as_bytes_mut());
            }
            if raw.len() != 32 {
                volatile_zeroize(&mut raw);
                bail!("Độ dài khóa media vault không đúng 32 byte");
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&raw);
            volatile_zeroize(&mut raw);
            return Ok(Self { bytes: key });
        }

        let mut key = [0u8; 32];
        getrandom::fill(&mut key).context("Không tạo được khóa ngẫu nhiên CSPRNG")?;
        let mut b64 = B64.encode(key);
        let sealed_res = dpapi::protect_secret(&b64);
        // SAFETY: Clearing UTF-8 base64 secret immediately before drop.
        unsafe {
            volatile_zeroize(b64.as_bytes_mut());
        }
        let sealed = match sealed_res {
            Ok(s) => s,
            Err(err) => {
                volatile_zeroize(&mut key);
                return Err(err);
            }
        };
        if let Err(err) = data_dir::write_private_atomic(key_path, sealed.as_bytes()) {
            volatile_zeroize(&mut key);
            return Err(err);
        }
        Ok(Self { bytes: key })
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey([REDACTED])")
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        volatile_zeroize(&mut self.bytes);
    }
}

/// 64 KiB chunked AES-256-GCM file format (`TZMB` v1) supporting O(1) RAM streaming
/// encryption/decryption and O(1) random-access byte-range reads for HTTP `206 Partial Content`.
///
/// Binary layout:
/// - Bytes `0..4`:   magic `b"TZMB"`
/// - Byte  `4`:      version `1u8`
/// - Bytes `5..9`:   `chunk_size: u32` LE (default `65536`)
/// - Bytes `9..21`:  `base_nonce: [u8; 12]`
/// - Bytes `21..29`: `plain_len: u64` LE
/// - Chunks `0..N`:  each `plaintext_chunk_len + 16B GCM tag`, encrypted with
///   `nonce = base_nonce XOR chunk_idx_le` and
///   `AAD = sha256_bytes || chunk_idx_le || is_final_byte`.
///   An empty 0-byte file stores 1 empty final chunk (16B tag) so the header & EOF are authenticated.
pub struct ChunkedAeadFile;

#[derive(Debug, Clone, Copy)]
struct ChunkedHeader {
    chunk_size: u32,
    base_nonce: [u8; 12],
    plain_len: u64,
}

impl ChunkedHeader {
    fn num_chunks(&self) -> u64 {
        if self.plain_len == 0 {
            1
        } else {
            self.plain_len.div_ceil(u64::from(self.chunk_size))
        }
    }

    fn expected_enc_len(&self) -> Result<u64> {
        let tag_total = self
            .num_chunks()
            .checked_mul(ChunkedAeadFile::TAG_LEN as u64)
            .ok_or_else(|| anyhow!("Tràn số kích thước tệp mã hóa"))?;
        (ChunkedAeadFile::HEADER_LEN as u64)
            .checked_add(self.plain_len)
            .and_then(|v| v.checked_add(tag_total))
            .ok_or_else(|| anyhow!("Tràn số kích thước tệp mã hóa"))
    }

    fn chunk_plain_len(&self, chunk_idx: u64) -> usize {
        if self.plain_len == 0 {
            return 0;
        }
        let num = self.num_chunks();
        if chunk_idx + 1 < num {
            self.chunk_size as usize
        } else {
            let start = chunk_idx * u64::from(self.chunk_size);
            (self.plain_len - start) as usize
        }
    }

    fn chunk_enc_offset(&self, chunk_idx: u64) -> Result<u64> {
        let stride = u64::from(self.chunk_size) + ChunkedAeadFile::TAG_LEN as u64;
        chunk_idx
            .checked_mul(stride)
            .and_then(|v| v.checked_add(ChunkedAeadFile::HEADER_LEN as u64))
            .ok_or_else(|| anyhow!("Tràn số vị trí chunk"))
    }
}

impl ChunkedAeadFile {
    pub const MAGIC: [u8; 4] = *b"TZMB";
    pub const VERSION: u8 = 1;
    pub const DEFAULT_CHUNK_SIZE: u32 = 65_536;
    pub const HEADER_LEN: usize = 29;
    pub const TAG_LEN: usize = 16;
    const MAX_ALLOWED_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

    fn derive_chunk_nonce(base_nonce: &[u8; 12], chunk_idx: u64) -> [u8; 12] {
        let mut nonce = *base_nonce;
        let idx_le = chunk_idx.to_le_bytes();
        for i in 0..8 {
            nonce[i] ^= idx_le[i];
        }
        nonce
    }

    fn build_aad(sha256_bytes: &[u8; 32], chunk_idx: u64, is_final: bool) -> [u8; 41] {
        let mut aad = [0u8; 41];
        aad[..32].copy_from_slice(sha256_bytes);
        aad[32..40].copy_from_slice(&chunk_idx.to_le_bytes());
        aad[40] = if is_final { 1 } else { 0 };
        aad
    }

    fn encode_header(chunk_size: u32, base_nonce: &[u8; 12], plain_len: u64) -> [u8; 29] {
        let mut hdr = [0u8; Self::HEADER_LEN];
        hdr[0..4].copy_from_slice(&Self::MAGIC);
        hdr[4] = Self::VERSION;
        hdr[5..9].copy_from_slice(&chunk_size.to_le_bytes());
        hdr[9..21].copy_from_slice(base_nonce);
        hdr[21..29].copy_from_slice(&plain_len.to_le_bytes());
        hdr
    }

    fn read_and_validate_header(file: &mut File) -> Result<ChunkedHeader> {
        let file_len = file.metadata()?.len();
        if file_len < Self::HEADER_LEN as u64 {
            bail!("Tệp media mã hóa bị hỏng (header quá ngắn)");
        }
        let mut raw = [0u8; Self::HEADER_LEN];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut raw)
            .context("Không đọc được header tệp media mã hóa")?;
        if raw[0..4] != Self::MAGIC {
            bail!("Định dạng tệp media mã hóa không hợp lệ (sai magic TZMB)");
        }
        if raw[4] != Self::VERSION {
            bail!("Phiên bản tệp media mã hóa không được hỗ trợ ({})", raw[4]);
        }
        let chunk_size = u32::from_le_bytes(raw[5..9].try_into().expect("4-byte chunk_size"));
        if chunk_size == 0 || chunk_size > Self::MAX_ALLOWED_CHUNK_SIZE {
            bail!("Kích thước chunk không hợp lệ ({chunk_size})");
        }
        let mut base_nonce = [0u8; 12];
        base_nonce.copy_from_slice(&raw[9..21]);
        let plain_len = u64::from_le_bytes(raw[21..29].try_into().expect("8-byte plain_len"));
        let hdr = ChunkedHeader {
            chunk_size,
            base_nonce,
            plain_len,
        };
        let expected_len = hdr.expected_enc_len()?;
        if file_len != expected_len {
            bail!(
                "Tệp media mã hóa bị cắt cụt hoặc bị thay đổi kích thước (thực tế {file_len}, kỳ vọng {expected_len})"
            );
        }
        Ok(hdr)
    }

    fn open_private_write(path: &Path) -> Result<File> {
        if let Some(parent) = path.parent() {
            data_dir::ensure_private_dir(parent)?;
        }
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(path)
            .with_context(|| format!("Không tạo được tệp {}", path.display()))
    }

    /// Streams `src_path` into `dst_path` encrypted in 64 KiB AES-256-GCM chunks using O(1) RAM.
    /// Returns the plaintext byte length.
    pub fn encrypt_file_streaming(
        src_path: &Path,
        dst_path: &Path,
        key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> Result<u64> {
        let src_file = File::open(src_path)
            .with_context(|| format!("Không mở được tệp nguồn {}", src_path.display()))?;
        let plain_len = src_file.metadata()?.len();
        let mut reader = BufReader::with_capacity(IO_BUF_LEN, src_file);

        let dst_file = Self::open_private_write(dst_path)?;
        let res = Self::encrypt_reader_to_file(
            &mut reader,
            plain_len,
            dst_file,
            key,
            sha256_bytes,
        );
        if res.is_err() {
            let _ = std::fs::remove_file(dst_path);
        }
        res
    }

    /// Encrypts an in-memory slice directly into `dst_path` in 64 KiB chunks.
    /// Returns the plaintext byte length.
    pub fn encrypt_bytes_to_file(
        bytes: &[u8],
        dst_path: &Path,
        key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> Result<u64> {
        let mut cursor = std::io::Cursor::new(bytes);
        let dst_file = Self::open_private_write(dst_path)?;
        let res = Self::encrypt_reader_to_file(
            &mut cursor,
            bytes.len() as u64,
            dst_file,
            key,
            sha256_bytes,
        );
        if res.is_err() {
            let _ = std::fs::remove_file(dst_path);
        }
        res
    }

    fn encrypt_reader_to_file<R: Read>(
        reader: &mut R,
        plain_len: u64,
        dst_file: File,
        key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> Result<u64> {
        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key");
        let chunk_size = Self::DEFAULT_CHUNK_SIZE;
        let mut base_nonce = [0u8; 12];
        getrandom::fill(&mut base_nonce).context("OS CSPRNG failure")?;

        let mut writer = BufWriter::with_capacity(IO_BUF_LEN + Self::TAG_LEN, dst_file);
        let header = Self::encode_header(chunk_size, &base_nonce, plain_len);
        writer.write_all(&header)?;

        if plain_len == 0 {
            let nonce_bytes = Self::derive_chunk_nonce(&base_nonce, 0);
            let aad = Self::build_aad(sha256_bytes, 0, true);
            let ct = cipher
                .encrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload { msg: &[], aad: &aad },
                )
                .map_err(|_| anyhow!("Mã hóa chunk rỗng thất bại"))?;
            writer.write_all(&ct)?;
        } else {
            let num_chunks = plain_len.div_ceil(u64::from(chunk_size));
            let mut plain_buf = vec![0u8; chunk_size as usize];
            for chunk_idx in 0..num_chunks {
                let is_final = chunk_idx + 1 == num_chunks;
                let to_read = if is_final {
                    (plain_len - chunk_idx * u64::from(chunk_size)) as usize
                } else {
                    chunk_size as usize
                };
                reader.read_exact(&mut plain_buf[..to_read])?;
                let nonce_bytes = Self::derive_chunk_nonce(&base_nonce, chunk_idx);
                let aad = Self::build_aad(sha256_bytes, chunk_idx, is_final);
                let ct = cipher
                    .encrypt(
                        Nonce::from_slice(&nonce_bytes),
                        Payload {
                            msg: &plain_buf[..to_read],
                            aad: &aad,
                        },
                    )
                    .map_err(|_| anyhow!("Mã hóa chunk media thất bại"))?;
                writer.write_all(&ct)?;
            }
            let mut extra = [0u8; 1];
            if reader.read(&mut extra)? != 0 {
                bail!("Tệp nguồn thay đổi kích thước trong khi đang mã hóa");
            }
        }

        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(plain_len)
    }

    /// Random-access O(1)-seek range decryption for HTTP `206 Partial Content`.
    /// Returns `(plaintext_slice, total_plain_len)`.
    pub fn read_range(
        enc_path: &Path,
        key: &[u8; 32],
        sha256_bytes: &[u8; 32],
        start: u64,
        end_inclusive: u64,
    ) -> Result<(Vec<u8>, u64)> {
        let mut file = File::open(enc_path)
            .with_context(|| format!("Không mở được tệp media {}", enc_path.display()))?;
        let hdr = Self::read_and_validate_header(&mut file)?;
        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key");

        if hdr.plain_len == 0 {
            if start > 0 {
                bail!("Phạm vi byte vượt quá kích thước tệp rỗng");
            }
            let mut ct_buf = [0u8; Self::TAG_LEN];
            file.read_exact(&mut ct_buf)?;
            let nonce_bytes = Self::derive_chunk_nonce(&hdr.base_nonce, 0);
            let aad = Self::build_aad(sha256_bytes, 0, true);
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload {
                        msg: &ct_buf,
                        aad: &aad,
                    },
                )
                .map_err(|_| anyhow!("Giải mã tệp media thất bại (sai khóa hoặc bị sửa đổi)"))?;
            return Ok((Vec::new(), 0));
        }

        if start >= hdr.plain_len || start > end_inclusive {
            bail!(
                "Phạm vi byte không hợp lệ ({start}..={end_inclusive} / tổng {})",
                hdr.plain_len
            );
        }
        let effective_end = end_inclusive.min(hdr.plain_len - 1);
        let out_len_u64 = effective_end - start + 1;
        let out_len = usize::try_from(out_len_u64)
            .map_err(|_| anyhow!("Phạm vi đọc vượt quá giới hạn bộ nhớ"))?;

        let chunk_size_u64 = u64::from(hdr.chunk_size);
        let num_chunks = hdr.num_chunks();
        let first_chunk = start / chunk_size_u64;
        let last_chunk = effective_end / chunk_size_u64;

        let seek_off = hdr.chunk_enc_offset(first_chunk)?;
        file.seek(SeekFrom::Start(seek_off))?;

        let mut out = Vec::with_capacity(out_len);
        let mut ct_buf = Vec::with_capacity(hdr.chunk_size as usize + Self::TAG_LEN);

        for chunk_idx in first_chunk..=last_chunk {
            let plain_chunk_len = hdr.chunk_plain_len(chunk_idx);
            let enc_chunk_len = plain_chunk_len + Self::TAG_LEN;
            ct_buf.resize(enc_chunk_len, 0);
            file.read_exact(&mut ct_buf)?;

            let is_final = chunk_idx + 1 == num_chunks;
            let nonce_bytes = Self::derive_chunk_nonce(&hdr.base_nonce, chunk_idx);
            let aad = Self::build_aad(sha256_bytes, chunk_idx, is_final);
            let pt = cipher
                .decrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload {
                        msg: &ct_buf,
                        aad: &aad,
                    },
                )
                .map_err(|_| anyhow!("Giải mã chunk media thất bại (sai khóa hoặc dữ liệu bị sửa đổi)"))?;

            let chunk_plain_start = chunk_idx * chunk_size_u64;
            let slice_start = start.saturating_sub(chunk_plain_start) as usize;
            let slice_end = ((effective_end - chunk_plain_start) as usize + 1).min(pt.len());
            out.extend_from_slice(&pt[slice_start..slice_end]);
        }

        Ok((out, hdr.plain_len))
    }

    /// Streams decryption of `enc_path` into `dst_path` using O(1) RAM.
    /// Removes `dst_path` on authentication or I/O failure so tampered bytes are never left behind.
    /// Returns total plaintext bytes written.
    pub fn decrypt_to_file_streaming(
        enc_path: &Path,
        dst_path: &Path,
        key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> Result<u64> {
        let mut src = File::open(enc_path)
            .with_context(|| format!("Không mở được tệp media {}", enc_path.display()))?;
        let hdr = Self::read_and_validate_header(&mut src)?;
        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key");

        if let Some(parent) = dst_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Không tạo được thư mục đích {}", parent.display()))?;
        }
        let dst = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(dst_path)
            .with_context(|| format!("Không mở được tệp đích {}", dst_path.display()))?;

        let write_res: Result<u64> = (|| {
            let mut writer = BufWriter::with_capacity(IO_BUF_LEN, dst);
            let num_chunks = hdr.num_chunks();
            let mut ct_buf = Vec::with_capacity(hdr.chunk_size as usize + Self::TAG_LEN);

            for chunk_idx in 0..num_chunks {
                let plain_chunk_len = hdr.chunk_plain_len(chunk_idx);
                let enc_chunk_len = plain_chunk_len + Self::TAG_LEN;
                ct_buf.resize(enc_chunk_len, 0);
                src.read_exact(&mut ct_buf)?;

                let is_final = chunk_idx + 1 == num_chunks;
                let nonce_bytes = Self::derive_chunk_nonce(&hdr.base_nonce, chunk_idx);
                let aad = Self::build_aad(sha256_bytes, chunk_idx, is_final);
                let pt = cipher
                    .decrypt(
                        Nonce::from_slice(&nonce_bytes),
                        Payload {
                            msg: &ct_buf,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| {
                        anyhow!("Giải mã tệp media thất bại (sai khóa hoặc dữ liệu bị sửa đổi)")
                    })?;
                if !pt.is_empty() {
                    writer.write_all(&pt)?;
                }
            }
            writer.flush()?;
            Ok(hdr.plain_len)
        })();

        if write_res.is_err() {
            let _ = std::fs::remove_file(dst_path);
        }
        write_res
    }

    /// Streams direct chunk-by-chunk re-encryption from `src_enc_path` (encrypted under `src_key`)
    /// into `dst_enc_path` (encrypted under `dst_key`) using O(1) RAM and ZERO plaintext temp files on disk.
    /// Returns the total byte length of the newly written encrypted file.
    pub fn reencrypt_file_streaming(
        src_enc_path: &Path,
        dst_enc_path: &Path,
        src_key: &[u8; 32],
        dst_key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> Result<u64> {
        Self::reencrypt_file_streaming_checked(
            src_enc_path,
            dst_enc_path,
            src_key,
            dst_key,
            sha256_bytes,
        )
        .map_err(|e| match e {
            ReencryptFailure::CorruptSource(err) | ReencryptFailure::Other(err) => err,
        })
    }

    fn reencrypt_file_streaming_checked(
        src_enc_path: &Path,
        dst_enc_path: &Path,
        src_key: &[u8; 32],
        dst_key: &[u8; 32],
        sha256_bytes: &[u8; 32],
    ) -> std::result::Result<u64, ReencryptFailure> {
        let mut src = File::open(src_enc_path)
            .with_context(|| format!("Không mở được tệp media {}", src_enc_path.display()))
            .map_err(ReencryptFailure::Other)?;
        let hdr = Self::read_and_validate_header(&mut src).map_err(|err| {
            let is_non_corrupt_io = err.downcast_ref::<std::io::Error>().is_some_and(|io_err| {
                io_err.kind() != std::io::ErrorKind::UnexpectedEof
            });
            if is_non_corrupt_io {
                ReencryptFailure::Other(err)
            } else {
                ReencryptFailure::CorruptSource(err)
            }
        })?;
        let src_cipher = Aes256Gcm::new_from_slice(src_key).expect("32-byte AES-256 key");
        let dst_cipher = Aes256Gcm::new_from_slice(dst_key).expect("32-byte AES-256 key");

        let mut dst_base_nonce = [0u8; 12];
        getrandom::fill(&mut dst_base_nonce)
            .context("OS CSPRNG failure")
            .map_err(ReencryptFailure::Other)?;

        let dst = Self::open_private_write(dst_enc_path).map_err(ReencryptFailure::Other)?;
        let res: std::result::Result<u64, ReencryptFailure> = (|| {
            let mut writer =
                BufWriter::with_capacity(hdr.chunk_size as usize + Self::TAG_LEN, dst);
            let dst_header = Self::encode_header(hdr.chunk_size, &dst_base_nonce, hdr.plain_len);
            writer
                .write_all(&dst_header)
                .map_err(|e| ReencryptFailure::Other(e.into()))?;
            let mut written: u64 = Self::HEADER_LEN as u64;

            let num_chunks = hdr.num_chunks();
            let mut ct_buf = Vec::with_capacity(hdr.chunk_size as usize + Self::TAG_LEN);

            for chunk_idx in 0..num_chunks {
                let plain_chunk_len = hdr.chunk_plain_len(chunk_idx);
                let enc_chunk_len = plain_chunk_len + Self::TAG_LEN;
                ct_buf.resize(enc_chunk_len, 0);
                if let Err(io_err) = src.read_exact(&mut ct_buf) {
                    return if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                        Err(ReencryptFailure::CorruptSource(io_err.into()))
                    } else {
                        Err(ReencryptFailure::Other(io_err.into()))
                    };
                }

                let is_final = chunk_idx + 1 == num_chunks;
                let src_nonce = Self::derive_chunk_nonce(&hdr.base_nonce, chunk_idx);
                let aad = Self::build_aad(sha256_bytes, chunk_idx, is_final);
                let mut pt = src_cipher
                    .decrypt(
                        Nonce::from_slice(&src_nonce),
                        Payload {
                            msg: &ct_buf,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| {
                        ReencryptFailure::CorruptSource(anyhow!(
                            "Giải mã tệp media cục bộ thất bại (sai khóa hoặc dữ liệu bị sửa đổi)"
                        ))
                    })?;

                let dst_nonce = Self::derive_chunk_nonce(&dst_base_nonce, chunk_idx);
                let enc_res = dst_cipher.encrypt(
                    Nonce::from_slice(&dst_nonce),
                    Payload {
                        msg: &pt,
                        aad: &aad,
                    },
                );
                volatile_zeroize(&mut pt);
                let out_ct = enc_res.map_err(|_| {
                    ReencryptFailure::Other(anyhow!("Mã hóa chunk media đám mây thất bại"))
                })?;
                writer
                    .write_all(&out_ct)
                    .map_err(|e| ReencryptFailure::Other(e.into()))?;
                written = written.saturating_add(out_ct.len() as u64);
            }

            writer
                .flush()
                .map_err(|e| ReencryptFailure::Other(e.into()))?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|e| ReencryptFailure::Other(e.into()))?;
            Ok(written)
        })();

        if res.is_err() {
            let _ = std::fs::remove_file(dst_enc_path);
        }
        res
    }
}

enum ReencryptFailure {
    CorruptSource(anyhow::Error),
    Other(anyhow::Error),
}

struct InflightGuard<'a> {
    map: &'a StdMutex<HashMap<String, Arc<Notify>>>,
    key: String,
    notify: Arc<Notify>,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.map.lock() {
            if guard
                .get(&self.key)
                .is_some_and(|cur| Arc::ptr_eq(cur, &self.notify))
            {
                guard.remove(&self.key);
            }
        }
        self.notify.notify_waiters();
    }
}

pub struct MediaVault {
    vault_dir: PathBuf,
    config: StdRwLock<MediaVaultConfig>,
    key: Arc<VaultKey>,
    inflight: StdMutex<HashMap<String, Arc<Notify>>>,
    download_sem: Arc<Semaphore>,
    pending_touches: StdMutex<HashMap<String, i64>>,
}

impl fmt::Debug for MediaVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaVault")
            .field("vault_dir", &self.vault_dir)
            .field("config", &self.config())
            .field("key", &self.key)
            .finish()
    }
}

fn wipe_dir_contents(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn is_active_transfer_tmp_name(name: &str) -> bool {
    name.starts_with("gdrive-up-")
        || name.starts_with("gdrive-dl-")
        || name.starts_with("cloud-rehydrate-")
        || name.starts_with("dl-")
        || name.starts_with("stage-")
        || name.ends_with(".part")
}

/// Cleans stale scratch files in `vault/tmp` while preserving active or recent transfer files
/// (`gdrive-up-*`, `gdrive-dl-*`, `cloud-rehydrate-*`, `dl-*.part`, `stage-*`) so clearing cache
/// never deletes files belonging to concurrent Google Drive or CDN transfers.
fn wipe_stale_tmp_contents(dir: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let current_pid_tag = format!("-{}-", std::process::id());
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if is_active_transfer_tmp_name(&name) {
            if name.contains(&current_pid_tag) {
                continue;
            }
            let is_recent = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|mtime| now.duration_since(mtime).ok())
                .is_none_or(|age| age < min_age);
            if is_recent {
                continue;
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}

fn hash_file_streaming(path: &Path) -> Result<(String, [u8; 32], u64)> {
    let mut file = File::open(path)
        .with_context(|| format!("Không mở được tệp để băm {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; IO_BUF_LEN];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total.saturating_add(n as u64);
    }
    let digest = hasher.finalize();
    let mut sha_bytes = [0u8; 32];
    sha_bytes.copy_from_slice(&digest);
    Ok((encode_hex_lower(&sha_bytes), sha_bytes, total))
}

fn copy_file_streaming(src_path: &Path, dst_path: &Path, sync: bool) -> Result<u64> {
    let mut src = File::open(src_path)
        .with_context(|| format!("Không mở được tệp nguồn {}", src_path.display()))?;
    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Không tạo được thư mục {}", parent.display()))?;
    }
    let dst = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst_path)
        .with_context(|| format!("Không mở được tệp đích {}", dst_path.display()))?;
    let res: Result<u64> = (|| {
        let mut writer = BufWriter::with_capacity(IO_BUF_LEN, dst);
        let mut buf = vec![0u8; IO_BUF_LEN];
        let mut total: u64 = 0;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            total = total.saturating_add(n as u64);
        }
        writer.flush()?;
        if sync {
            writer.get_ref().sync_all()?;
        }
        Ok(total)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(dst_path);
    }
    res
}

fn build_rel_path(kind: &str, sha256_hex: &str, encrypted: bool) -> String {
    let bucket = if kind.eq_ignore_ascii_case("thumb") {
        "thumbs"
    } else {
        "blobs"
    };
    let ext = if encrypted { "enc" } else { "bin" };
    let p1 = &sha256_hex[0..2];
    let p2 = &sha256_hex[2..4];
    format!("{bucket}/{p1}/{p2}/{sha256_hex}.{ext}")
}

fn finalize_staged_blob(stage_path: &Path, final_path: &Path) -> Result<()> {
    if let Some(parent) = final_path.parent() {
        data_dir::ensure_private_dir(parent)?;
    }
    if let Err(err) = std::fs::rename(stage_path, final_path) {
        if final_path.exists() {
            let _ = std::fs::remove_file(stage_path);
        } else {
            let _ = std::fs::remove_file(stage_path);
            return Err(err).with_context(|| {
                format!("Không lưu được tệp media vào {}", final_path.display())
            });
        }
    }
    Ok(())
}

impl MediaVault {
    pub fn open(paths: &StoragePaths, uid: Option<&str>, config: MediaVaultConfig) -> Result<Self> {
        let custom_root = config
            .custom_vault_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Path::new);
        let vault_dir = paths.media_vault_dir(uid, custom_root);
        data_dir::ensure_private_dir(&vault_dir)?;
        data_dir::ensure_private_dir(&vault_dir.join("blobs"))?;
        data_dir::ensure_private_dir(&vault_dir.join("thumbs"))?;
        let tmp_dir = vault_dir.join("tmp");
        data_dir::ensure_private_dir(&tmp_dir)?;
        wipe_dir_contents(&tmp_dir);

        let key_path = paths.media_vault_key_file(uid);
        let key = Arc::new(VaultKey::load_or_create(&key_path)?);

        Ok(Self {
            vault_dir,
            config: StdRwLock::new(config),
            key,
            inflight: StdMutex::new(HashMap::new()),
            download_sem: Arc::new(Semaphore::new(DOWNLOAD_CONCURRENCY_LIMIT)),
            pending_touches: StdMutex::new(HashMap::new()),
        })
    }

    pub fn sha256_url(url: &str) -> String {
        let canonical = normalize_allowed_media_url(url)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| url.trim().to_string());
        let digest = Sha256::digest(canonical.as_bytes());
        encode_hex_lower(&digest)
    }

    pub fn vault_dir(&self) -> &Path {
        &self.vault_dir
    }

    pub fn config(&self) -> MediaVaultConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn update_config(&self, config: MediaVaultConfig) {
        let mut guard = self.config.write().unwrap_or_else(|e| e.into_inner());
        *guard = config;
    }

    /// Validates that `local_rel_path` stays strictly inside `vault_dir/blobs` or `vault_dir/thumbs`.
    pub fn resolve_blob_path(&self, local_rel_path: &str) -> Result<PathBuf> {
        let trimmed = local_rel_path.trim();
        if trimmed.is_empty() || trimmed.contains('\\') {
            bail!("Đường dẫn media cục bộ không hợp lệ");
        }
        let rel = Path::new(trimmed);
        let mut comps = rel.components();
        let Some(Component::Normal(first)) = comps.next() else {
            bail!("Đường dẫn media cục bộ không hợp lệ");
        };
        if first != "blobs" && first != "thumbs" {
            bail!("Đường dẫn media cục bộ nằm ngoài thư mục kho");
        }
        let mut full = self.vault_dir.join(first);
        for c in comps {
            match c {
                Component::Normal(seg) => full.push(seg),
                _ => bail!("Đường dẫn media cục bộ chứa thành phần không hợp lệ"),
            }
        }
        Ok(full)
    }

    fn next_tmp_path(&self, prefix: &str, ext: &str) -> PathBuf {
        let id = NEXT_VAULT_TMP_ID.fetch_add(1, Ordering::Relaxed);
        self.vault_dir
            .join("tmp")
            .join(format!("{prefix}-{}-{id}.{ext}", std::process::id()))
    }

    /// Allocates a unique scratch file path inside `vault/tmp/` (auto-cleaned on next vault open).
    pub fn next_cloud_tmp_path(&self, prefix: &str, ext: &str) -> PathBuf {
        self.next_tmp_path(prefix, ext)
    }

    fn record_access_touch(&self, sha256: &str) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let mut map = self
            .pending_touches
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(sha256.to_string(), now);
        map.len()
    }

    pub async fn flush_access_touches(&self, store: &Arc<AsyncMutex<MessageStore>>) {
        let batch: Vec<(String, i64)> = {
            let mut map = self
                .pending_touches
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if map.is_empty() {
                return;
            }
            map.drain().collect()
        };
        let guard = store.lock().await;
        let _ = guard.batch_touch_media_access(&batch);
    }

    /// Checks whether `url` has a locally present blob on disk without holding `store` across filesystem I/O.
    pub async fn lookup_local_by_url(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        url: &str,
    ) -> Result<Option<MediaObjectRecord>> {
        let url_hash = Self::sha256_url(url);
        let maybe_obj = {
            let guard = store.lock().await;
            guard.lookup_media_by_url_hash(&url_hash)?
        };
        let Some(obj) = maybe_obj else {
            return Ok(None);
        };
        if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            return Ok(None);
        }
        let Some(rel) = obj.local_rel_path.as_deref() else {
            return Ok(None);
        };
        let Ok(path) = self.resolve_blob_path(rel) else {
            return Ok(None);
        };
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            Ok(Some(obj))
        } else {
            Ok(None)
        }
    }

    fn default_variant_for_kind(kind: &str) -> &'static str {
        if kind.eq_ignore_ascii_case("thumb") {
            "thumb"
        } else {
            "primary"
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_media_metadata(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        sha256_hex: &str,
        kind: &str,
        mime: &str,
        size_bytes: u64,
        local_rel_path: String,
        is_encrypted: bool,
        url_opt: Option<&str>,
        msg_ref: Option<(&str, &str, &str)>,
        original_name: Option<&str>,
        cdn_status: i32,
        preserve_existing_updated_ts: bool,
    ) -> Result<MediaObjectRecord> {
        let now = chrono::Utc::now().timestamp_millis();
        let default_variant = Self::default_variant_for_kind(kind);
        let obj = MediaObjectRecord {
            sha256: sha256_hex.to_string(),
            kind: kind.to_string(),
            mime: mime.to_string(),
            size_bytes,
            thumb_sha256: None,
            width: None,
            height: None,
            duration_ms: None,
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some(local_rel_path),
            is_encrypted,
            pin_count: 0,
            created_ts: now,
            last_accessed_ts: now,
            cloud_provider: None,
            cloud_file_id: None,
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: None,
        };
        let url_hash = url_opt.map(Self::sha256_url).unwrap_or_default();

        let guard = store.lock().await;
        guard.upsert_media_object(&obj)?;
        let link_updated_ts = if preserve_existing_updated_ts {
            0
        } else {
            guard.next_local_mutation_ts().unwrap_or_else(|_| now.max(1))
        };
        if let Some((tid, mid, raw_variant)) = msg_ref {
            let trimmed = raw_variant.trim();
            let variant = if trimmed.is_empty() {
                default_variant.to_string()
            } else {
                trimmed.to_string()
            };
            guard.link_message_media(&MessageMediaRecord {
                thread_id: tid.to_string(),
                msg_id: mid.to_string(),
                variant,
                url_hash,
                sha256: Some(sha256_hex.to_string()),
                original_name: original_name.map(str::to_string),
                saved_user_path: None,
                cdn_status,
                updated_ts: link_updated_ts,
            })?;
        } else if !url_hash.is_empty() {
            guard.link_message_media(&MessageMediaRecord {
                thread_id: "__url__".to_string(),
                msg_id: url_hash.clone(),
                variant: default_variant.to_string(),
                url_hash,
                sha256: Some(sha256_hex.to_string()),
                original_name: original_name.map(str::to_string),
                saved_user_path: None,
                cdn_status,
                updated_ts: link_updated_ts,
            })?;
        }
        let persisted = guard.get_media_object(sha256_hex)?.unwrap_or(obj);
        Ok(persisted)
    }

    async fn check_existing_local_blob(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        sha256_hex: &str,
    ) -> Result<Option<(String, bool)>> {
        let existing = {
            let guard = store.lock().await;
            guard.get_media_object(sha256_hex)?
        };
        if let Some(obj) = existing {
            if obj.local_state == MEDIA_LOCAL_STATE_LOCAL {
                if let Some(rel) = obj.local_rel_path {
                    if let Ok(path) = self.resolve_blob_path(&rel) {
                        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                            return Ok(Some((rel, obj.is_encrypted)));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_file_with_precomputed_hash(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        src_path: &Path,
        sha256_hex: String,
        sha256_bytes: [u8; 32],
        size_bytes: u64,
        kind: &str,
        mime: &str,
        url_opt: Option<&str>,
        msg_ref: Option<(&str, &str, &str)>,
        original_name: Option<&str>,
        cdn_status: i32,
        preserve_existing_updated_ts: bool,
    ) -> Result<MediaObjectRecord> {
        let effective_mime = if is_generic_mime(mime) {
            let src_probe = src_path.to_path_buf();
            let probe = tokio::task::spawn_blocking(move || read_file_head_and_tail_probe(&src_probe))
                .await
                .unwrap_or_default();
            resolve_effective_mime_from_probe(mime, &probe, original_name, url_opt)
        } else {
            mime.trim().to_string()
        };

        let (rel_path, is_encrypted) = if let Some(hit) =
            self.check_existing_local_blob(store, &sha256_hex).await?
        {
            hit
        } else {
            let encrypt = self.config().encrypt_at_rest;
            let rel = build_rel_path(kind, &sha256_hex, encrypt);
            let final_path = self.resolve_blob_path(&rel)?;
            let stage_path = self.next_tmp_path("stage", if encrypt { "enc" } else { "bin" });
            let src = src_path.to_path_buf();
            let key = self.key.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                if encrypt {
                    ChunkedAeadFile::encrypt_file_streaming(
                        &src,
                        &stage_path,
                        key.as_bytes(),
                        &sha256_bytes,
                    )?;
                } else {
                    copy_file_streaming(&src, &stage_path, true)?;
                }
                finalize_staged_blob(&stage_path, &final_path)
            })
            .await
            .context("Tác vụ mã hóa/lưu media bị hủy")??;
            (rel, encrypt)
        };

        self.commit_media_metadata(
            store,
            &sha256_hex,
            kind,
            &effective_mime,
            size_bytes,
            rel_path,
            is_encrypted,
            url_opt,
            msg_ref,
            original_name,
            cdn_status,
            preserve_existing_updated_ts,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_local_file(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        src_path: &Path,
        kind: &str,
        mime: &str,
        url_opt: Option<&str>,
        msg_ref: Option<(&str, &str)>,
        original_name: Option<&str>,
    ) -> Result<MediaObjectRecord> {
        let src_owned = src_path.to_path_buf();
        let (sha256_hex, sha256_bytes, size_bytes) =
            tokio::task::spawn_blocking(move || hash_file_streaming(&src_owned))
                .await
                .context("Tác vụ tính SHA-256 bị hủy")??;

        let default_variant = Self::default_variant_for_kind(kind);
        let cdn_status = if url_opt.is_some() {
            MEDIA_CDN_STATUS_OK
        } else {
            MEDIA_CDN_STATUS_UNKNOWN
        };
        self.ingest_file_with_precomputed_hash(
            store,
            src_path,
            sha256_hex,
            sha256_bytes,
            size_bytes,
            kind,
            mime,
            url_opt,
            msg_ref.map(|(t, m)| (t, m, default_variant)),
            original_name,
            cdn_status,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_bytes(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        bytes: &[u8],
        kind: &str,
        mime: &str,
        url_opt: Option<&str>,
        msg_ref: Option<(&str, &str)>,
        original_name: Option<&str>,
    ) -> Result<MediaObjectRecord> {
        let effective_mime = if is_generic_mime(mime) {
            let probe = slice_head_and_tail_probe(bytes);
            resolve_effective_mime_from_probe(mime, &probe, original_name, url_opt)
        } else {
            mime.trim().to_string()
        };

        let digest = Sha256::digest(bytes);
        let mut sha256_bytes = [0u8; 32];
        sha256_bytes.copy_from_slice(&digest);
        let sha256_hex = encode_hex_lower(&sha256_bytes);
        let size_bytes = bytes.len() as u64;

        let (rel_path, is_encrypted) = if let Some(hit) =
            self.check_existing_local_blob(store, &sha256_hex).await?
        {
            hit
        } else {
            let encrypt = self.config().encrypt_at_rest;
            let rel = build_rel_path(kind, &sha256_hex, encrypt);
            let final_path = self.resolve_blob_path(&rel)?;
            let stage_path = self.next_tmp_path("stage", if encrypt { "enc" } else { "bin" });
            let owned = bytes.to_vec();
            let key = self.key.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                if encrypt {
                    ChunkedAeadFile::encrypt_bytes_to_file(
                        &owned,
                        &stage_path,
                        key.as_bytes(),
                        &sha256_bytes,
                    )?;
                } else {
                    if let Some(parent) = stage_path.parent() {
                        data_dir::ensure_private_dir(parent)?;
                    }
                    let mut f = ChunkedAeadFile::open_private_write(&stage_path)?;
                    f.write_all(&owned)?;
                    f.sync_all()?;
                }
                finalize_staged_blob(&stage_path, &final_path)
            })
            .await
            .context("Tác vụ lưu media bị hủy")??;
            (rel, encrypt)
        };

        let default_variant = Self::default_variant_for_kind(kind);
        let cdn_status = if url_opt.is_some() {
            MEDIA_CDN_STATUS_OK
        } else {
            MEDIA_CDN_STATUS_UNKNOWN
        };
        self.commit_media_metadata(
            store,
            &sha256_hex,
            kind,
            &effective_mime,
            size_bytes,
            rel_path,
            is_encrypted,
            url_opt,
            msg_ref.map(|(t, m)| (t, m, default_variant)),
            original_name,
            cdn_status,
            false,
        )
        .await
    }

    /// Sniffs the concrete MIME type of a cached local blob by inspecting its decrypted head & tail
    /// windows (at most 128 KiB total). Returns `None` if unknown or unreadable.
    pub async fn sniff_cached_mime(&self, obj: &MediaObjectRecord) -> Option<&'static str> {
        let total = obj.size_bytes;
        if total == 0 || obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            return None;
        }
        let window = MIME_PROBE_WINDOW_BYTES as u64;
        let probe = if total <= window * 2 {
            self.read_range(obj, 0, total - 1).await.ok()?
        } else {
            let mut buf = self.read_range(obj, 0, window - 1).await.ok()?;
            if let Ok(tail) = self.read_range(obj, total - window, total - 1).await {
                buf.extend_from_slice(&tail);
            }
            buf
        };
        sniff_mime_from_bytes(&probe)
    }

    async fn try_hit_cache(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        url_hash: &str,
        kind: &str,
        msg_ref: Option<(&str, &str)>,
    ) -> Result<Option<MediaObjectRecord>> {
        let maybe_obj = {
            let guard = store.lock().await;
            guard.lookup_media_by_url_hash(url_hash)?
        };
        let Some(mut obj) = maybe_obj else {
            return Ok(None);
        };
        if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            return Ok(None);
        }
        let Some(rel) = obj.local_rel_path.as_deref() else {
            return Ok(None);
        };
        let Ok(path) = self.resolve_blob_path(rel) else {
            return Ok(None);
        };
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(None);
        }

        // Self-heal legacy blobs cached with generic MIME (e.g. `application/octet-stream`)
        // so `teza-media://` and Save-As always receive accurate Content-Type & extensions.
        if is_generic_mime(&obj.mime) && obj.size_bytes > 0 {
            if let Some(sniffed) = self.sniff_cached_mime(&obj).await {
                if !sniffed.eq_ignore_ascii_case(obj.mime.trim()) {
                    obj.mime = sniffed.to_string();
                    let guard = store.lock().await;
                    let _ = guard.upsert_media_object(&obj);
                }
            }
        }

        let pending_count = self.record_access_touch(&obj.sha256);
        if let Some((tid, mid)) = msg_ref {
            let variant = Self::default_variant_for_kind(kind);
            let guard = store.lock().await;
            let existing_link = guard
                .get_message_media_record(tid, mid, variant)
                .ok()
                .flatten();
            let needs_link = match existing_link {
                None => true,
                Some(ref rec) => {
                    rec.sha256.as_deref() != Some(obj.sha256.as_str())
                        || (rec.url_hash.is_empty() && !url_hash.is_empty())
                }
            };
            if needs_link {
                let mut_ts = guard
                    .next_local_mutation_ts()
                    .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis().max(1));
                let _ = guard.link_message_media(&MessageMediaRecord {
                    thread_id: tid.to_string(),
                    msg_id: mid.to_string(),
                    variant: variant.to_string(),
                    url_hash: url_hash.to_string(),
                    sha256: Some(obj.sha256.clone()),
                    original_name: None,
                    saved_user_path: None,
                    cdn_status: MEDIA_CDN_STATUS_UNKNOWN,
                    updated_ts: mut_ts,
                });
            }
        }
        if pending_count >= TOUCH_FLUSH_THRESHOLD {
            self.flush_access_touches(store).await;
        }
        Ok(Some(obj))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_cached_url<F>(
        &self,
        session: &Session,
        store: &Arc<AsyncMutex<MessageStore>>,
        url: &str,
        kind: &str,
        msg_ref: Option<(&str, &str)>,
        mut on_progress: F,
    ) -> Result<MediaObjectRecord>
    where
        F: FnMut(u64, Option<u64>),
    {
        let url_hash = Self::sha256_url(url);
        if let Some(hit) = self.try_hit_cache(store, &url_hash, kind, msg_ref).await? {
            return Ok(hit);
        }

        // Singleflight coordination: wait if another task is already downloading `url_hash`.
        let mut waited_once = false;
        let leader_guard = loop {
            enum Slot {
                Leader(Arc<Notify>),
                Follower(Arc<Notify>),
            }
            let slot = {
                let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(existing) = map.get(&url_hash) {
                    Slot::Follower(existing.clone())
                } else {
                    let notify = Arc::new(Notify::new());
                    map.insert(url_hash.clone(), notify.clone());
                    Slot::Leader(notify)
                }
            };
            match slot {
                Slot::Leader(notify) => {
                    break InflightGuard {
                        map: &self.inflight,
                        key: url_hash.clone(),
                        notify,
                    };
                }
                Slot::Follower(notify) => {
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    let still_inflight = {
                        let map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
                        map.get(&url_hash)
                            .is_some_and(|cur| Arc::ptr_eq(cur, &notify))
                    };
                    if still_inflight {
                        notified.await;
                    }
                    if let Some(hit) = self.try_hit_cache(store, &url_hash, kind, msg_ref).await? {
                        return Ok(hit);
                    }
                    if waited_once {
                        // Leader failed; proceed to take leadership once for a clean error/retry.
                        continue;
                    }
                    waited_once = true;
                }
            }
        };

        let _permit = self
            .download_sem
            .acquire()
            .await
            .context("Hàng đợi tải media đã đóng")?;

        // Re-check after acquiring semaphore in case another path cached it.
        if let Some(hit) = self.try_hit_cache(store, &url_hash, kind, msg_ref).await? {
            drop(leader_guard);
            return Ok(hit);
        }

        let tmp_path = self.next_tmp_path("dl", "part");
        let meta = match session
            .download_media_to_temp_file(url, &tmp_path, &mut on_progress)
            .await
        {
            Ok(meta) => meta,
            Err(err) => {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                if err
                    .downcast_ref::<MediaDownloadError>()
                    .is_some_and(MediaDownloadError::is_cdn_expired)
                {
                    let guard = store.lock().await;
                    let updated = guard
                        .mark_media_cdn_status(&url_hash, MEDIA_CDN_STATUS_EXPIRED)
                        .unwrap_or(0);
                    if updated == 0 {
                        let (tid, mid) = match msg_ref {
                            Some((t, m)) => (t.to_string(), m.to_string()),
                            None => ("__url__".to_string(), url_hash.clone()),
                        };
                        let variant = Self::default_variant_for_kind(kind).to_string();
                        let _ = guard.link_message_media(&MessageMediaRecord {
                            thread_id: tid,
                            msg_id: mid,
                            variant,
                            url_hash: url_hash.clone(),
                            sha256: None,
                            original_name: None,
                            saved_user_path: None,
                            cdn_status: MEDIA_CDN_STATUS_EXPIRED,
                            updated_ts: 0,
                        });
                    }
                }
                drop(leader_guard);
                return Err(err);
            }
        };

        let sha256_bytes = match decode_sha256_hex(&meta.sha256_hex) {
            Ok(b) => b,
            Err(err) => {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                drop(leader_guard);
                return Err(err);
            }
        };

        let default_variant = Self::default_variant_for_kind(kind);
        let ingest_res = self
            .ingest_file_with_precomputed_hash(
                store,
                &tmp_path,
                meta.sha256_hex,
                sha256_bytes,
                meta.size_bytes,
                kind,
                &meta.mime,
                Some(url),
                msg_ref.map(|(t, m)| (t, m, default_variant)),
                meta.file_name.as_deref(),
                MEDIA_CDN_STATUS_OK,
                false,
            )
            .await;
        let _ = tokio::fs::remove_file(&tmp_path).await;
        drop(leader_guard);

        let obj = ingest_res?;
        if self.config().quota_mb > 0 {
            let _ = self
                .run_lru_eviction_excluding(store, Some(&obj.sha256))
                .await;
        }
        Ok(obj)
    }

    /// Reads an inclusive byte range `[start..=end_inclusive]` from a cached local blob.
    /// Supports both encrypted (`TZMB` 64 KiB chunked AES-256-GCM) and plaintext blobs in O(1) seek time.
    pub async fn read_range(
        &self,
        obj: &MediaObjectRecord,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Vec<u8>> {
        if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            bail!("Tệp media chưa có sẵn trong kho cục bộ");
        }
        let rel = obj
            .local_rel_path
            .as_deref()
            .ok_or_else(|| anyhow!("Thiếu đường dẫn tệp media cục bộ"))?;
        let blob_path = self.resolve_blob_path(rel)?;
        let _ = self.record_access_touch(&obj.sha256);

        if obj.is_encrypted {
            let sha256_bytes = decode_sha256_hex(&obj.sha256)?;
            let key = self.key.clone();
            let (bytes, _total) = tokio::task::spawn_blocking(move || {
                ChunkedAeadFile::read_range(
                    &blob_path,
                    key.as_bytes(),
                    &sha256_bytes,
                    start,
                    end_inclusive,
                )
            })
            .await
            .context("Tác vụ đọc/giải mã range bị hủy")??;
            Ok(bytes)
        } else {
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                let mut file = File::open(&blob_path)
                    .with_context(|| format!("Không mở được tệp media {}", blob_path.display()))?;
                let total = file.metadata()?.len();
                if total == 0 {
                    if start > 0 {
                        bail!("Phạm vi byte vượt quá kích thước tệp rỗng");
                    }
                    return Ok(Vec::new());
                }
                if start >= total || start > end_inclusive {
                    bail!("Phạm vi byte không hợp lệ ({start}..={end_inclusive} / tổng {total})");
                }
                let effective_end = end_inclusive.min(total - 1);
                let len = usize::try_from(effective_end - start + 1)
                    .map_err(|_| anyhow!("Phạm vi đọc vượt quá giới hạn bộ nhớ"))?;
                file.seek(SeekFrom::Start(start))?;
                let mut buf = vec![0u8; len];
                file.read_exact(&mut buf)?;
                Ok(buf)
            })
            .await
            .context("Tác vụ đọc range bị hủy")?
        }
    }

    /// Exports a cached blob to `dst_path` using O(1) RAM streaming decryption (or copy if plaintext).
    pub async fn export_to_path(&self, obj: &MediaObjectRecord, dst_path: &Path) -> Result<u64> {
        if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            bail!("Tệp media chưa có sẵn trong kho cục bộ");
        }
        let rel = obj
            .local_rel_path
            .as_deref()
            .ok_or_else(|| anyhow!("Thiếu đường dẫn tệp media cục bộ"))?;
        let blob_path = self.resolve_blob_path(rel)?;
        let _ = self.record_access_touch(&obj.sha256);
        let dst = dst_path.to_path_buf();

        if obj.is_encrypted {
            let sha256_bytes = decode_sha256_hex(&obj.sha256)?;
            let key = self.key.clone();
            tokio::task::spawn_blocking(move || {
                ChunkedAeadFile::decrypt_to_file_streaming(
                    &blob_path,
                    &dst,
                    key.as_bytes(),
                    &sha256_bytes,
                )
            })
            .await
            .context("Tác vụ xuất giải mã tệp bị hủy")?
        } else {
            tokio::task::spawn_blocking(move || copy_file_streaming(&blob_path, &dst, false))
                .await
                .context("Tác vụ sao chép tệp bị hủy")?
        }
    }

    /// Sweeps oldest unpinned non-thumbnail blobs down to 85% of `quota_mb`.
    /// No-op returning `Ok(0)` when `quota_mb == 0` (unlimited).
    pub async fn run_lru_eviction(&self, store: &Arc<AsyncMutex<MessageStore>>) -> Result<u64> {
        self.run_lru_eviction_excluding(store, None).await
    }

    /// Sweeps oldest unpinned non-thumbnail blobs down to 85% of `quota_mb`, while strictly
    /// preserving `exclude_sha256` (e.g., an oversized blob just cached by `ensure_cached_url`).
    pub async fn run_lru_eviction_excluding(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        exclude_sha256: Option<&str>,
    ) -> Result<u64> {
        self.flush_access_touches(store).await;
        let quota_mb = self.config().quota_mb;
        if quota_mb == 0 {
            return Ok(0);
        }
        let quota_bytes = quota_mb.saturating_mul(1024 * 1024);
        let target_bytes = (u128::from(quota_bytes) * 85 / 100) as u64;

        let mut current_total = {
            let guard = store.lock().await;
            guard.compute_media_stats()?.total_bytes
        };
        if current_total <= quota_bytes {
            return Ok(0);
        }

        let mut freed_bytes: u64 = 0;
        loop {
            if current_total <= target_bytes {
                break;
            }
            let candidates = {
                let guard = store.lock().await;
                guard.select_lru_eviction_candidates(64)?
            };
            if candidates.is_empty() {
                break;
            }

            let mut victims: Vec<(String, Option<String>, u64)> = Vec::new();
            let mut planned_total = current_total;
            for c in candidates {
                if exclude_sha256.is_some_and(|ex| c.sha256 == ex) {
                    continue;
                }
                if planned_total <= target_bytes {
                    break;
                }
                planned_total = planned_total.saturating_sub(c.size_bytes);
                victims.push((c.sha256, c.local_rel_path, c.size_bytes));
            }
            if victims.is_empty() {
                break;
            }

            {
                let guard = store.lock().await;
                for (sha, _, _) in &victims {
                    let _ = guard.mark_media_evicted(sha);
                }
            }

            let mut rels = Vec::with_capacity(victims.len());
            for (_, rel_opt, size) in victims {
                if let Some(rel) = rel_opt {
                    rels.push(rel);
                }
                freed_bytes = freed_bytes.saturating_add(size);
                current_total = current_total.saturating_sub(size);
            }
            self.unlink_rel_paths(&rels).await;
        }

        Ok(freed_bytes)
    }

    pub async fn clear_cache(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        include_pinned: bool,
    ) -> Result<MediaVaultStatsDto> {
        {
            let mut map = self
                .pending_touches
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            map.clear();
        }
        let (blobs, stats) = {
            let guard = store.lock().await;
            let blobs = guard.list_unpinned_local_blobs(include_pinned)?;
            for (sha, _) in &blobs {
                let _ = guard.mark_media_evicted(sha);
            }
            let stats = guard.compute_media_stats()?;
            (blobs, stats)
        };

        let rels: Vec<String> = blobs.into_iter().map(|(_, rel)| rel).collect();
        self.unlink_rel_paths(&rels).await;
        let tmp_dir = self.vault_dir.join("tmp");
        let _ = tokio::task::spawn_blocking(move || {
            wipe_stale_tmp_contents(&tmp_dir, CLEAR_CACHE_TMP_STALE_AGE)
        })
        .await;
        Ok(stats)
    }

    /// Best-effort unlinks validated relative vault blob paths outside any SQLite lock.
    pub async fn unlink_rel_paths(&self, rel_paths: &[String]) {
        for rel in rel_paths {
            if let Ok(path) = self.resolve_blob_path(rel) {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }

    /// Streams a locally stored blob (encrypted or plaintext) into `tmp_out_path` encrypted under
    /// `cloud_key` using `TZMB` v1 64 KiB chunked AES-256-GCM with `obj.sha256` bound in AAD.
    /// Uses O(1) RAM and removes `tmp_out_path` on error. Returns the encrypted file byte length.
    pub async fn prepare_cloud_encrypted_temp(
        &self,
        obj: &MediaObjectRecord,
        cloud_key: &[u8; 32],
        tmp_out_path: &Path,
    ) -> Result<u64> {
        if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
            bail!("Tệp media chưa có sẵn trong kho cục bộ để tải lên đám mây");
        }
        let rel = obj
            .local_rel_path
            .as_deref()
            .ok_or_else(|| anyhow!("Thiếu đường dẫn tệp media cục bộ"))?;
        let blob_path = self.resolve_blob_path(rel)?;
        let sha256_bytes = decode_sha256_hex(&obj.sha256)?;
        let out_path = tmp_out_path.to_path_buf();
        let mut cloud_key_copy = *cloud_key;
        let is_encrypted = obj.is_encrypted;
        let local_key = self.key.clone();

        tokio::task::spawn_blocking(move || -> Result<u64> {
            let res = if is_encrypted {
                match ChunkedAeadFile::reencrypt_file_streaming_checked(
                    &blob_path,
                    &out_path,
                    local_key.as_bytes(),
                    &cloud_key_copy,
                    &sha256_bytes,
                ) {
                    Ok(n) => Ok(n),
                    Err(ReencryptFailure::CorruptSource(err)) => {
                        // Permanently corrupted local encrypted cache file (invalid header, truncated
                        // chunk, or AEAD tag mismatch): remove the unreadable cache file after `src`
                        // is closed so Step 1 can evict it and continue uploading healthy blobs.
                        let _ = std::fs::remove_file(&blob_path);
                        Err(err)
                    }
                    Err(ReencryptFailure::Other(err)) => Err(err),
                }
            } else {
                ChunkedAeadFile::encrypt_file_streaming(
                    &blob_path,
                    &out_path,
                    &cloud_key_copy,
                    &sha256_bytes,
                )?;
                let enc_len = std::fs::metadata(&out_path)
                    .with_context(|| {
                        format!(
                            "Không đọc được kích thước tệp mã hóa đám mây {}",
                            out_path.display()
                        )
                    })?
                    .len();
                Ok(enc_len)
            };
            volatile_zeroize(&mut cloud_key_copy);
            if res.is_err() {
                let _ = std::fs::remove_file(&out_path);
            }
            res
        })
        .await
        .context("Tác vụ chuẩn bị tệp mã hóa đám mây bị hủy")?
    }

    /// Stream-decrypts a `TZMB` v1 cloud-encrypted blob using `cloud_key` + `expected_sha256` AAD
    /// into a temporary file under `vault/tmp/`, verifies the decrypted plaintext SHA-256 matches
    /// `expected_sha256`, ingests it into the Local Vault (respecting current `encrypt_at_rest`),
    /// and always cleans up intermediate temp files.
    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_from_cloud_encrypted_temp(
        &self,
        store: &Arc<AsyncMutex<MessageStore>>,
        enc_tmp_path: &Path,
        cloud_key: &[u8; 32],
        expected_sha256: &str,
        kind: &str,
        mime: &str,
        url_opt: Option<&str>,
        msg_ref: Option<(&str, &str, &str)>,
        original_name: Option<&str>,
    ) -> Result<MediaObjectRecord> {
        let expected_sha_bytes = decode_sha256_hex(expected_sha256)?;
        let plain_tmp = self.next_tmp_path("cloud-rehydrate", "bin");
        let enc_path_owned = enc_tmp_path.to_path_buf();
        let plain_tmp_for_task = plain_tmp.clone();
        let mut cloud_key_copy = *cloud_key;

        let decrypt_res = tokio::task::spawn_blocking(move || -> Result<(String, [u8; 32], u64)> {
            let dec_out = (|| -> Result<(String, [u8; 32], u64)> {
                ChunkedAeadFile::decrypt_to_file_streaming(
                    &enc_path_owned,
                    &plain_tmp_for_task,
                    &cloud_key_copy,
                    &expected_sha_bytes,
                )?;
                let (actual_hex, actual_bytes, size_bytes) =
                    hash_file_streaming(&plain_tmp_for_task)?;
                if !constant_time_eq(&actual_bytes, &expected_sha_bytes) {
                    bail!(
                        "Kiểm tra toàn vẹn SHA-256 thất bại sau khi giải mã tệp từ đám mây"
                    );
                }
                Ok((actual_hex, actual_bytes, size_bytes))
            })();
            volatile_zeroize(&mut cloud_key_copy);
            if dec_out.is_err() {
                let _ = std::fs::remove_file(&plain_tmp_for_task);
            }
            dec_out
        })
        .await
        .context("Tác vụ giải mã tệp đám mây bị hủy")?;

        let (actual_hex, actual_bytes, size_bytes) = match decrypt_res {
            Ok(v) => v,
            Err(err) => {
                let _ = tokio::fs::remove_file(&plain_tmp).await;
                return Err(err);
            }
        };

        let ingest_res = self
            .ingest_file_with_precomputed_hash(
                store,
                &plain_tmp,
                actual_hex,
                actual_bytes,
                size_bytes,
                kind,
                mime,
                url_opt,
                msg_ref,
                original_name,
                MEDIA_CDN_STATUS_UNKNOWN,
                true,
            )
            .await;
        let _ = tokio::fs::remove_file(&plain_tmp).await;

        ingest_res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_temp_dir(label: &str) -> PathBuf {
        let id = NEXT_VAULT_TMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "teza-vault-test-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha256_of(bytes: &[u8]) -> (String, [u8; 32]) {
        let d = Sha256::digest(bytes);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&d);
        (encode_hex_lower(&arr), arr)
    }

    #[test]
    fn chunked_aead_roundtrip_range_reads_and_tamper_detection() {
        let dir = test_temp_dir("chunked");
        let src = dir.join("source.bin");
        let enc = dir.join("blob.enc");
        let out = dir.join("decrypted.bin");

        // 200 KiB payload (> 3 chunks of 64 KiB, with a partial final chunk).
        let mut plain = vec![0u8; 200 * 1024 + 137];
        for (i, b) in plain.iter_mut().enumerate() {
            *b = ((i * 31 + 7) & 0xff) as u8;
        }
        std::fs::write(&src, &plain).unwrap();

        let key = [0x42u8; 32];
        let (_hex, sha_bytes) = sha256_of(&plain);

        let enc_plain_len =
            ChunkedAeadFile::encrypt_file_streaming(&src, &enc, &key, &sha_bytes).unwrap();
        assert_eq!(enc_plain_len, plain.len() as u64);

        // Verify header magic and that disk bytes do not contain raw plaintext slices.
        let enc_raw = std::fs::read(&enc).unwrap();
        assert_eq!(&enc_raw[0..4], b"TZMB");
        assert_eq!(enc_raw[4], 1);
        assert_ne!(&enc_raw[29..61], &plain[0..32]);

        // Single-chunk range inside chunk 0
        let (r0, total) = ChunkedAeadFile::read_range(&enc, &key, &sha_bytes, 10, 99).unwrap();
        assert_eq!(total, plain.len() as u64);
        assert_eq!(r0, &plain[10..=99]);

        // Cross-chunk range spanning chunks 0, 1, 2
        let start = 65_536 - 50;
        let end = 65_536 * 2 + 123;
        let (r_cross, _) =
            ChunkedAeadFile::read_range(&enc, &key, &sha_bytes, start as u64, end as u64).unwrap();
        assert_eq!(r_cross, &plain[start..=end]);

        // Tail range clamped past EOF
        let tail_start = plain.len() - 40;
        let (r_tail, _) = ChunkedAeadFile::read_range(
            &enc,
            &key,
            &sha_bytes,
            tail_start as u64,
            plain.len() as u64 + 500,
        )
        .unwrap();
        assert_eq!(r_tail, &plain[tail_start..]);

        // Out-of-bounds start fails cleanly
        assert!(ChunkedAeadFile::read_range(
            &enc,
            &key,
            &sha_bytes,
            plain.len() as u64,
            plain.len() as u64 + 10
        )
        .is_err());

        // Full streaming decrypt matches byte-for-byte
        let dec_len =
            ChunkedAeadFile::decrypt_to_file_streaming(&enc, &out, &key, &sha_bytes).unwrap();
        assert_eq!(dec_len, plain.len() as u64);
        assert_eq!(std::fs::read(&out).unwrap(), plain);

        // Wrong AAD (different sha256) must fail authentication
        let mut wrong_sha = sha_bytes;
        wrong_sha[0] ^= 0xff;
        assert!(ChunkedAeadFile::read_range(&enc, &key, &wrong_sha, 0, 16).is_err());

        // Tampering 1 byte in chunk 1 must fail authentication and clean up dst on decrypt
        let mut tampered = enc_raw.clone();
        tampered[29 + 65_536 + 16 + 100] ^= 0x01;
        let enc_tampered = dir.join("tampered.enc");
        std::fs::write(&enc_tampered, &tampered).unwrap();
        assert!(ChunkedAeadFile::read_range(&enc_tampered, &key, &sha_bytes, 65_536, 65_600).is_err());
        let bad_out = dir.join("bad_out.bin");
        assert!(
            ChunkedAeadFile::decrypt_to_file_streaming(&enc_tampered, &bad_out, &key, &sha_bytes)
                .is_err()
        );
        assert!(!bad_out.exists());

        // Truncating the encrypted file must fail cleanly
        let enc_trunc = dir.join("trunc.enc");
        std::fs::write(&enc_trunc, &enc_raw[..enc_raw.len() - 1]).unwrap();
        assert!(ChunkedAeadFile::read_range(&enc_trunc, &key, &sha_bytes, 0, 10).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunked_aead_handles_empty_zero_byte_file() {
        let dir = test_temp_dir("empty");
        let src = dir.join("empty.bin");
        let enc = dir.join("empty.enc");
        let out = dir.join("empty_out.bin");
        std::fs::write(&src, b"").unwrap();

        let key = [0x11u8; 32];
        let (_hex, sha_bytes) = sha256_of(b"");
        let n = ChunkedAeadFile::encrypt_file_streaming(&src, &enc, &key, &sha_bytes).unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            std::fs::metadata(&enc).unwrap().len(),
            (ChunkedAeadFile::HEADER_LEN + ChunkedAeadFile::TAG_LEN) as u64
        );

        let (slice, total) = ChunkedAeadFile::read_range(&enc, &key, &sha_bytes, 0, 0).unwrap();
        assert!(slice.is_empty());
        assert_eq!(total, 0);
        assert!(ChunkedAeadFile::read_range(&enc, &key, &sha_bytes, 1, 1).is_err());

        let written =
            ChunkedAeadFile::decrypt_to_file_streaming(&enc, &out, &key, &sha_bytes).unwrap();
        assert_eq!(written, 0);
        assert!(std::fs::read(&out).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_key_seals_with_dpapi_redacts_debug_and_config_roundtrips() {
        let dir = test_temp_dir("key-cfg");
        let key_path = dir.join("accounts").join("u1_media.key");
        let k1 = VaultKey::load_or_create(&key_path).unwrap();
        let dbg = format!("{k1:?}");
        assert!(dbg.contains("REDACTED"));
        assert!(!dbg.contains(&encode_hex_lower(k1.as_bytes())));

        let k2 = VaultKey::load_or_create(&key_path).unwrap();
        assert_eq!(k1.as_bytes(), k2.as_bytes());

        let cfg_path = dir.join("media_config.json");
        assert_eq!(MediaVaultConfig::load_from_file(&cfg_path), MediaVaultConfig::default());

        let custom_cfg = MediaVaultConfig {
            custom_vault_dir: Some("  E:\\TezaMedia  ".into()),
            quota_mb: 2048,
            encrypt_at_rest: false,
            auto_cache_images: true,
            auto_cache_voice: false,
            auto_cache_video_files: true,
        };
        custom_cfg.save_to_file(&cfg_path).unwrap();
        let loaded = MediaVaultConfig::load_from_file(&cfg_path);
        assert_eq!(loaded.custom_vault_dir.as_deref(), Some("E:\\TezaMedia"));
        assert_eq!(loaded.quota_mb, 2048);
        assert!(!loaded.encrypt_at_rest);
        assert!(loaded.auto_cache_video_files);

        let tok = generate_protocol_token().unwrap();
        assert_eq!(tok.len(), 64);
        assert!(constant_time_eq(tok.as_bytes(), tok.as_bytes()));
        assert!(!constant_time_eq(tok.as_bytes(), b"wrong"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn media_vault_ingest_encrypted_and_plaintext_range_export_dedup_lru_and_clear() {
        let dir = test_temp_dir("vault-full");
        let custom_root = dir.join("external_drive");
        let paths = StoragePaths::from_dir(dir.join("appdata")).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("acc1"))).unwrap(),
        ));

        let cfg = MediaVaultConfig {
            custom_vault_dir: Some(custom_root.to_string_lossy().into_owned()),
            quota_mb: 1, // 1 MiB quota
            encrypt_at_rest: true,
            ..Default::default()
        };
        let vault = MediaVault::open(&paths, Some("acc1"), cfg).unwrap();
        assert_eq!(vault.vault_dir(), custom_root.join("acc1"));

        // Stale temp file in tmp/ should be wiped on next open
        let stale_tmp = vault.vault_dir().join("tmp").join("stale.part");
        std::fs::write(&stale_tmp, b"junk").unwrap();
        let vault = MediaVault::open(&paths, Some("acc1"), vault.config()).unwrap();
        assert!(!stale_tmp.exists());

        // Ingest 400 KiB encrypted video with URL + msg_ref
        let video_bytes: Vec<u8> = (0..(400 * 1024)).map(|i| (i % 251) as u8).collect();
        let url_v = "https://video-stal-20.dlmd.me/demo.mp4";
        let obj1 = vault
            .ingest_bytes(
                &store,
                &video_bytes,
                "video",
                "video/mp4",
                Some(url_v),
                Some(("t1", "m1")),
                Some("demo.mp4"),
            )
            .await
            .unwrap();
        assert!(obj1.is_encrypted);
        assert_eq!(obj1.size_bytes, video_bytes.len() as u64);

        // Deduplication: ingesting identical content under a second message reuses the exact same sha256 & disk file
        let obj1_dup = vault
            .ingest_bytes(
                &store,
                &video_bytes,
                "video",
                "video/mp4",
                Some("https://video-stal-20.dlmd.me/demo_copy.mp4"),
                Some(("t1", "m2")),
                Some("demo_copy.mp4"),
            )
            .await
            .unwrap();
        assert_eq!(obj1_dup.sha256, obj1.sha256);
        assert_eq!(obj1_dup.local_rel_path, obj1.local_rel_path);

        // Range read across chunk boundary + full export
        let slice = vault.read_range(&obj1, 65_500, 65_600).await.unwrap();
        assert_eq!(slice, &video_bytes[65_500..=65_600]);
        let exported = dir.join("exported_demo.mp4");
        let exp_len = vault.export_to_path(&obj1, &exported).await.unwrap();
        assert_eq!(exp_len, video_bytes.len() as u64);
        assert_eq!(std::fs::read(&exported).unwrap(), video_bytes);

        // Path traversal rejection on resolve_blob_path
        assert!(vault.resolve_blob_path("../secrets.key").is_err());
        assert!(vault.resolve_blob_path("blobs/../../escape").is_err());
        assert!(vault.resolve_blob_path("other/file.bin").is_err());

        // Switch to plaintext mode (encrypt_at_rest = false) and ingest a local file
        let mut plain_cfg = vault.config();
        plain_cfg.encrypt_at_rest = false;
        vault.update_config(plain_cfg);

        let local_src = dir.join("doc.bin");
        let doc_bytes: Vec<u8> = (0..(450 * 1024)).map(|i| ((i * 7) % 253) as u8).collect();
        std::fs::write(&local_src, &doc_bytes).unwrap();
        let obj2 = vault
            .ingest_local_file(
                &store,
                &local_src,
                "file",
                "application/octet-stream",
                Some("https://file-stal-9.dlfl.vn/doc.bin"),
                Some(("t1", "m3")),
                Some("doc.bin"),
            )
            .await
            .unwrap();
        assert!(!obj2.is_encrypted);
        assert_eq!(
            vault.read_range(&obj2, 100, 299).await.unwrap(),
            &doc_bytes[100..=299]
        );
        let exported2 = dir.join("exported_doc.bin");
        vault.export_to_path(&obj2, &exported2).await.unwrap();
        assert_eq!(std::fs::read(&exported2).unwrap(), doc_bytes);

        // Ingest a 3rd 400 KiB blob -> total ~1250 KiB > 1024 KiB quota!
        // Pin obj1 so LRU evicts obj2 (oldest unpinned) rather than obj1!
        {
            let guard = store.lock().await;
            guard.set_media_pin(&obj1.sha256, true).unwrap();
            guard
                .batch_touch_media_access(&[(obj2.sha256.clone(), 1_000)])
                .unwrap();
        }
        // Make sure obj2 has older last_accessed_ts than obj3 by setting obj2 directly via re-open/timestamp
        let third_bytes: Vec<u8> = (0..(400 * 1024)).map(|i| ((i * 13 + 3) % 251) as u8).collect();
        let obj3 = vault
            .ingest_bytes(
                &store,
                &third_bytes,
                "image",
                "image/png",
                Some("https://f10-zpc.zdn.vn/pic.png"),
                Some(("t1", "m4")),
                Some("pic.png"),
            )
            .await
            .unwrap();
        // Force obj3 newer touch so obj2 is strictly older
        {
            let guard = store.lock().await;
            guard
                .batch_touch_media_access(&[(obj3.sha256.clone(), i64::MAX / 2)])
                .unwrap();
        }

        let freed = vault.run_lru_eviction(&store).await.unwrap();
        assert!(freed >= obj2.size_bytes, "expected obj2 evicted, freed={freed}");
        {
            let guard = store.lock().await;
            assert_eq!(
                guard.get_media_object(&obj1.sha256).unwrap().unwrap().local_state,
                MEDIA_LOCAL_STATE_LOCAL
            );
            assert_eq!(
                guard.get_media_object(&obj2.sha256).unwrap().unwrap().local_state,
                0
            );
            assert_eq!(
                guard.get_media_object(&obj3.sha256).unwrap().unwrap().local_state,
                MEDIA_LOCAL_STATE_LOCAL
            );
        }

        // Clear cache preserving pinned (include_pinned = false) leaves obj1 intact
        let stats_keep_pinned = vault.clear_cache(&store, false).await.unwrap();
        assert_eq!(stats_keep_pinned.object_count, 1);
        assert_eq!(stats_keep_pinned.pinned_count, 1);
        assert_eq!(stats_keep_pinned.total_bytes, obj1.size_bytes);

        // Clear cache including pinned empties everything
        let stats_all = vault.clear_cache(&store, true).await.unwrap();
        assert_eq!(stats_all.object_count, 0);
        assert_eq!(stats_all.total_bytes, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lru_eviction_is_noop_when_quota_is_zero_unlimited() {
        let dir = test_temp_dir("unlimited");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(None)).unwrap(),
        ));
        let cfg = MediaVaultConfig {
            quota_mb: 0,
            ..Default::default()
        };
        let vault = MediaVault::open(&paths, None, cfg).unwrap();
        let data = vec![0xABu8; 128 * 1024];
        let obj = vault
            .ingest_bytes(&store, &data, "image", "image/jpeg", None, None, None)
            .await
            .unwrap();
        let freed = vault.run_lru_eviction(&store).await.unwrap();
        assert_eq!(freed, 0);
        let guard = store.lock().await;
        assert_eq!(
            guard.get_media_object(&obj.sha256).unwrap().unwrap().local_state,
            MEDIA_LOCAL_STATE_LOCAL
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lru_eviction_excluding_preserves_oversized_blob_larger_than_quota() {
        let dir = test_temp_dir("oversized-lru");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u_big"))).unwrap(),
        ));
        let cfg = MediaVaultConfig {
            quota_mb: 1, // 1 MiB quota
            encrypt_at_rest: true,
            ..Default::default()
        };
        let vault = MediaVault::open(&paths, Some("u_big"), cfg).unwrap();

        // Older 300 KiB unpinned blob
        let old_bytes: Vec<u8> = (0..(300 * 1024)).map(|i| (i % 211) as u8).collect();
        let old_obj = vault
            .ingest_bytes(
                &store,
                &old_bytes,
                "image",
                "image/jpeg",
                Some("https://f10-zpc.zdn.vn/old.jpg"),
                Some(("t1", "m_old")),
                Some("old.jpg"),
            )
            .await
            .unwrap();
        let old_disk_path = vault
            .resolve_blob_path(old_obj.local_rel_path.as_deref().unwrap())
            .unwrap();
        assert!(old_disk_path.exists());

        // Newly cached 1.5 MiB blob (> 1 MiB quota_mb!)
        let big_bytes: Vec<u8> = (0..(1536 * 1024)).map(|i| ((i * 17 + 5) % 251) as u8).collect();
        let big_obj = vault
            .ingest_bytes(
                &store,
                &big_bytes,
                "video",
                "video/mp4",
                Some("https://video-stal-20.dlmd.me/huge.mp4"),
                Some(("t1", "m_big")),
                Some("huge.mp4"),
            )
            .await
            .unwrap();

        // Eviction excluding the just-cached oversized blob evicts the older blob, terminates
        // cleanly, and leaves the oversized blob intact and readable on disk.
        let freed = vault
            .run_lru_eviction_excluding(&store, Some(&big_obj.sha256))
            .await
            .unwrap();
        assert_eq!(freed, old_obj.size_bytes);
        assert!(!old_disk_path.exists());

        {
            let guard = store.lock().await;
            assert_eq!(
                guard
                    .get_media_object(&old_obj.sha256)
                    .unwrap()
                    .unwrap()
                    .local_state,
                0
            );
            assert_eq!(
                guard
                    .get_media_object(&big_obj.sha256)
                    .unwrap()
                    .unwrap()
                    .local_state,
                MEDIA_LOCAL_STATE_LOCAL
            );
        }

        // Subsequent range read and streaming export on the oversized blob must succeed cleanly
        let tail_start = big_bytes.len() - 1024;
        let tail = vault
            .read_range(&big_obj, tail_start as u64, (big_bytes.len() - 1) as u64)
            .await
            .unwrap();
        assert_eq!(tail, &big_bytes[tail_start..]);

        let out_path = dir.join("exported_huge.mp4");
        let exported = vault.export_to_path(&big_obj, &out_path).await.unwrap();
        assert_eq!(exported, big_bytes.len() as u64);
        assert_eq!(std::fs::read(&out_path).unwrap(), big_bytes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cloud_reencrypt_and_rehydrate_roundtrip_and_tamper_rejection() {
        let dir = test_temp_dir("cloud-bridge");
        let paths_a = StoragePaths::from_dir(dir.join("device_a")).unwrap();
        let store_a = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths_a.history_db(Some("u1"))).unwrap(),
        ));
        let vault_a = MediaVault::open(
            &paths_a,
            Some("u1"),
            MediaVaultConfig {
                encrypt_at_rest: true,
                ..Default::default()
            },
        )
        .unwrap();

        // Multi-chunk 210 KiB payload (> 3 chunks of 64 KiB)
        let original: Vec<u8> = (0..(210 * 1024 + 37))
            .map(|i| ((i * 29 + 11) % 251) as u8)
            .collect();
        let obj_a = vault_a
            .ingest_bytes(
                &store_a,
                &original,
                "video",
                "video/mp4",
                Some("https://video-stal-20.dlmd.me/cloud.mp4"),
                Some(("t1", "m1")),
                Some("cloud.mp4"),
            )
            .await
            .unwrap();
        assert!(obj_a.is_encrypted);

        // Prepare cloud-encrypted temp file using a completely independent cloud master key
        let cloud_key = [0x9Au8; 32];
        let cloud_tmp = vault_a.next_cloud_tmp_path("cloud-up", "enc");
        let enc_len = vault_a
            .prepare_cloud_encrypted_temp(&obj_a, &cloud_key, &cloud_tmp)
            .await
            .unwrap();
        assert_eq!(enc_len, std::fs::metadata(&cloud_tmp).unwrap().len());
        let cloud_enc_bytes = std::fs::read(&cloud_tmp).unwrap();
        assert_eq!(&cloud_enc_bytes[0..4], b"TZMB");
        // Must differ from local vault encrypted blob (different key & fresh base nonce)
        let local_enc_bytes = std::fs::read(
            vault_a
                .resolve_blob_path(obj_a.local_rel_path.as_deref().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_ne!(cloud_enc_bytes, local_enc_bytes);

        // Device B (independent local VaultKey) rehydrates from cloud_tmp
        let paths_b = StoragePaths::from_dir(dir.join("device_b")).unwrap();
        let store_b = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths_b.history_db(Some("u1"))).unwrap(),
        ));
        let vault_b = MediaVault::open(
            &paths_b,
            Some("u1"),
            MediaVaultConfig {
                encrypt_at_rest: true,
                ..Default::default()
            },
        )
        .unwrap();

        // Wrong cloud key must be rejected
        let wrong_key = [0x11u8; 32];
        assert!(vault_b
            .ingest_from_cloud_encrypted_temp(
                &store_b,
                &cloud_tmp,
                &wrong_key,
                &obj_a.sha256,
                "video",
                "video/mp4",
                None,
                Some(("t1", "m1", "primary")),
                Some("cloud.mp4"),
            )
            .await
            .is_err());

        // Tampered ciphertext must be rejected
        let tampered_tmp = dir.join("tampered_cloud.enc");
        let mut tampered_bytes = cloud_enc_bytes.clone();
        tampered_bytes[ChunkedAeadFile::HEADER_LEN + 42] ^= 0x80;
        std::fs::write(&tampered_tmp, &tampered_bytes).unwrap();
        assert!(vault_b
            .ingest_from_cloud_encrypted_temp(
                &store_b,
                &tampered_tmp,
                &cloud_key,
                &obj_a.sha256,
                "video",
                "video/mp4",
                None,
                Some(("t1", "m1", "primary")),
                Some("cloud.mp4"),
            )
            .await
            .is_err());

        // Valid cloud key + matching sha256 rehydrates into Device B cleanly
        let obj_b = vault_b
            .ingest_from_cloud_encrypted_temp(
                &store_b,
                &cloud_tmp,
                &cloud_key,
                &obj_a.sha256,
                "video",
                "video/mp4",
                Some("https://video-stal-20.dlmd.me/cloud.mp4"),
                Some(("t1", "m1", "primary")),
                Some("cloud.mp4"),
            )
            .await
            .unwrap();
        assert_eq!(obj_b.sha256, obj_a.sha256);
        assert_eq!(obj_b.size_bytes, original.len() as u64);

        let exported_b = dir.join("rehydrated_b.mp4");
        vault_b.export_to_path(&obj_b, &exported_b).await.unwrap();
        assert_eq!(std::fs::read(&exported_b).unwrap(), original);

        // Plaintext local blob -> cloud encrypted -> rehydrate also works
        let mut plain_cfg = vault_a.config();
        plain_cfg.encrypt_at_rest = false;
        vault_a.update_config(plain_cfg);
        let plain_payload = b"plaintext local file re-encrypted for google drive backup".to_vec();
        let obj_plain = vault_a
            .ingest_bytes(
                &store_a,
                &plain_payload,
                "file",
                "text/plain",
                None,
                Some(("t2", "m2")),
                Some("note.txt"),
            )
            .await
            .unwrap();
        assert!(!obj_plain.is_encrypted);

        let cloud_tmp_plain = vault_a.next_cloud_tmp_path("cloud-up-plain", "enc");
        vault_a
            .prepare_cloud_encrypted_temp(&obj_plain, &cloud_key, &cloud_tmp_plain)
            .await
            .unwrap();
        let obj_b_plain = vault_b
            .ingest_from_cloud_encrypted_temp(
                &store_b,
                &cloud_tmp_plain,
                &cloud_key,
                &obj_plain.sha256,
                "file",
                "text/plain",
                None,
                Some(("t2", "m2", "primary")),
                Some("note.txt"),
            )
            .await
            .unwrap();
        let exported_plain = dir.join("rehydrated_plain.txt");
        vault_b
            .export_to_path(&obj_b_plain, &exported_plain)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&exported_plain).unwrap(), plain_payload);

        // Verify vault_b/tmp has no leftover cloud-rehydrate temp files
        let leftovers: Vec<_> = std::fs::read_dir(vault_b.vault_dir().join("tmp"))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sniff_mime_from_bytes_detects_images_video_audio_docs_and_never_svg() {
        assert_eq!(
            sniff_mime_from_bytes(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"),
            Some("image/png")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_mime_from_bytes(b"GIF89a\x01\x00"), Some("image/gif"));
        assert_eq!(
            sniff_mime_from_bytes(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00heic"),
            Some("image/heic")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"\x00\x00\x00\x1cftypmif1\x00\x00\x00\x00mif1avif"),
            Some("image/avif")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00"),
            Some("video/mp4")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00"),
            Some("audio/mp4")
        );
        assert_eq!(
            sniff_mime_from_bytes(b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00"),
            Some("video/quicktime")
        );
        // Raw AAC ADTS frame (sync 0xFFF1, sampling_freq_idx=4 -> 44.1kHz, frame_len=20)
        assert_eq!(
            sniff_mime_from_bytes(&[0xFF, 0xF1, 0x50, 0x80, 0x02, 0x80, 0xFC]),
            Some("audio/aac")
        );
        assert_eq!(sniff_mime_from_bytes(b"#!AMR\n\x3c\x00"), Some("audio/amr"));
        assert_eq!(sniff_mime_from_bytes(b"%PDF-1.7\n"), Some("application/pdf"));

        let mut xlsx = b"PK\x03\x04\x14\x00\x00\x00\x00\x00".to_vec();
        xlsx.extend_from_slice(b"xl/workbook.xml");
        assert_eq!(
            sniff_mime_from_bytes(&xlsx),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        );

        let mut xlsm = xlsx.clone();
        xlsm.extend_from_slice(b"xl/vbaProject.bin");
        assert_eq!(
            sniff_mime_from_bytes(&xlsm),
            Some("application/vnd.ms-excel.sheet.macroenabled.12")
        );

        // SVG must never be elevated as a safe vault MIME
        assert_eq!(
            sniff_mime_from_bytes(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            None
        );
        assert_eq!(mime_from_extension_safe("svg"), None);
    }

    #[tokio::test]
    async fn media_vault_upgrades_generic_mime_on_ingest_and_heals_legacy_cache_hit() {
        let dir = test_temp_dir("mime-heal");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u_mime"))).unwrap(),
        ));
        let vault = MediaVault::open(&paths, Some("u_mime"), MediaVaultConfig::default()).unwrap();

        // 1. Ingest extensionless JPEG bytes with generic application/octet-stream -> upgraded to image/jpeg
        let mut jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0];
        jpeg_bytes.extend_from_slice(&[0xAB; 512]);
        let url_img = "https://photo-stal-6.zdn.vn/a/extensionless_blob";
        let obj_img = vault
            .ingest_bytes(
                &store,
                &jpeg_bytes,
                "image",
                "application/octet-stream",
                Some(url_img),
                Some(("t1", "m_img")),
                None,
            )
            .await
            .unwrap();
        assert_eq!(obj_img.mime, "image/jpeg");

        // 2. Ingest local AAC ADTS file with empty MIME -> upgraded to audio/aac
        let aac_path = dir.join("voice_raw");
        let mut aac_bytes = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x80, 0xFC];
        aac_bytes.extend_from_slice(&[0x11; 256]);
        std::fs::write(&aac_path, &aac_bytes).unwrap();
        let obj_aac = vault
            .ingest_local_file(
                &store,
                &aac_path,
                "audio",
                "",
                Some("https://voice-stal-1.dlmd.me/raw"),
                Some(("t1", "m_aac")),
                None,
            )
            .await
            .unwrap();
        assert_eq!(obj_aac.mime, "audio/aac");

        // 3. Simulate a legacy v0.6.0 cached WebP image persisted with `application/octet-stream`:
        //    on cache hit (`try_hit_cache`), MediaVault must sniff and self-heal to `image/webp`.
        let mut webp_bytes = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
        webp_bytes.extend_from_slice(&[0x22; 256]);
        let url_webp = "https://f10-zpc.zdn.vn/legacy_blob";
        let obj_webp = vault
            .ingest_bytes(
                &store,
                &webp_bytes,
                "image",
                "application/octet-stream",
                Some(url_webp),
                Some(("t1", "m_webp")),
                None,
            )
            .await
            .unwrap();
        assert_eq!(obj_webp.mime, "image/webp");

        // Manually downgrade mime in SQLite back to application/octet-stream to simulate legacy cache row
        {
            let guard = store.lock().await;
            let mut stale = obj_webp.clone();
            stale.mime = "application/octet-stream".to_string();
            // Direct reset via delete + re-insert with stale mime
            let db_path = paths.history_db(Some("u_mime"));
            drop(guard);
            let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
            raw_conn
                .execute(
                    "UPDATE media_objects SET mime = 'application/octet-stream' WHERE sha256 = ?1",
                    rusqlite::params![&obj_webp.sha256],
                )
                .unwrap();
        }
        let url_hash = MediaVault::sha256_url(url_webp);
        let healed = vault
            .try_hit_cache(&store, &url_hash, "image", Some(("t1", "m_webp")))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(healed.mime, "image/webp");

        let persisted = {
            let guard = store.lock().await;
            guard.get_media_object(&obj_webp.sha256).unwrap().unwrap()
        };
        assert_eq!(persisted.mime, "image/webp");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn try_hit_cache_skips_redundant_link_updates_and_preserves_cdn_status() {
        let dir = test_temp_dir("cache-hit-idempotent");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u_cache"))).unwrap(),
        ));
        let vault = MediaVault::open(&paths, Some("u_cache"), MediaVaultConfig::default()).unwrap();

        let url = "https://f10-zpc.zdn.vn/cached_photo.jpg";
        let url_hash = MediaVault::sha256_url(url);
        let payload = b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00cached image payload".to_vec();
        let obj = vault
            .ingest_bytes(
                &store,
                &payload,
                "image",
                "image/jpeg",
                Some(url),
                Some(("t1", "m1")),
                Some("cached.jpg"),
            )
            .await
            .unwrap();

        let initial_updated_ts = {
            let guard = store.lock().await;
            guard
                .mark_media_cdn_status(&url_hash, MEDIA_CDN_STATUS_EXPIRED)
                .unwrap();
            guard
                .get_message_media_record("t1", "m1", "primary")
                .unwrap()
                .unwrap()
                .updated_ts
        };

        // Repeated read-only cache hits for ("t1", "m1") MUST NOT bump updated_ts or clobber EXPIRED!
        for _ in 0..3 {
            let hit = vault
                .try_hit_cache(&store, &url_hash, "image", Some(("t1", "m1")))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(hit.sha256, obj.sha256);
        }
        {
            let guard = store.lock().await;
            let rec = guard
                .get_message_media_record("t1", "m1", "primary")
                .unwrap()
                .unwrap();
            assert_eq!(rec.updated_ts, initial_updated_ts);
            assert_eq!(rec.cdn_status, MEDIA_CDN_STATUS_EXPIRED);
        }

        // Cache hit for a NEW message ("t1", "m2") referencing the same cached URL creates its link cleanly
        let hit2 = vault
            .try_hit_cache(&store, &url_hash, "image", Some(("t1", "m2")))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit2.sha256, obj.sha256);
        {
            let guard = store.lock().await;
            let rec2 = guard
                .get_message_media_record("t1", "m2", "primary")
                .unwrap()
                .unwrap();
            assert_eq!(rec2.sha256.as_deref(), Some(obj.sha256.as_str()));
            assert_eq!(rec2.cdn_status, MEDIA_CDN_STATUS_UNKNOWN);
            assert!(rec2.updated_ts > initial_updated_ts);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cloud_rehydrate_preserves_expired_cdn_and_variant_and_corrupt_local_blob_unblocks() {
        let dir = test_temp_dir("rehydrate-variant-corrupt");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u_reh"))).unwrap(),
        ));
        let vault = MediaVault::open(&paths, Some("u_reh"), MediaVaultConfig::default()).unwrap();

        // Prepare a cloud-encrypted blob
        let payload = b"gallery image payload for variant and expired cdn test".to_vec();
        let (sha_hex, sha_bytes) = sha256_of(&payload);
        let cloud_key = [0x77u8; 32];
        let enc_tmp = vault.next_cloud_tmp_path("gdrive-dl", "tzmb");
        ChunkedAeadFile::encrypt_bytes_to_file(&payload, &enc_tmp, &cloud_key, &sha_bytes).unwrap();

        let expired_url = "https://f10-zpc.zdn.vn/expired_gallery_item.jpg";
        let url_hash = MediaVault::sha256_url(expired_url);
        {
            let guard = store.lock().await;
            guard
                .link_message_media(&MessageMediaRecord {
                    thread_id: "t1".into(),
                    msg_id: "m_gal".into(),
                    variant: "gallery_2".into(),
                    url_hash: url_hash.clone(),
                    sha256: Some(sha_hex.clone()),
                    original_name: Some("gal2.jpg".into()),
                    saved_user_path: None,
                    cdn_status: MEDIA_CDN_STATUS_EXPIRED,
                    updated_ts: 123_456,
                })
                .unwrap();
        }

        let obj = vault
            .ingest_from_cloud_encrypted_temp(
                &store,
                &enc_tmp,
                &cloud_key,
                &sha_hex,
                "image",
                "image/jpeg",
                Some(expired_url),
                Some(("t1", "m_gal", "gallery_2")),
                Some("gal2.jpg"),
            )
            .await
            .unwrap();
        assert_eq!(obj.sha256, sha_hex);

        {
            let guard = store.lock().await;
            // Exact variant kept EXPIRED status and original updated_ts (not clobbered to OK!)
            let rec_gal = guard
                .get_message_media_record("t1", "m_gal", "gallery_2")
                .unwrap()
                .unwrap();
            assert_eq!(rec_gal.cdn_status, MEDIA_CDN_STATUS_EXPIRED);
            assert_eq!(rec_gal.updated_ts, 123_456);
            // NO bogus default "primary" row created!
            assert!(guard
                .get_message_media_record("t1", "m_gal", "primary")
                .unwrap()
                .is_none());
        }

        // Corrupt the local `.enc` blob on disk -> `prepare_cloud_encrypted_temp` returns Err AND
        // removes the unreadable local cache file so Step 1 can evict it and upload newer blobs!
        let local_blob_path = vault
            .resolve_blob_path(obj.local_rel_path.as_deref().unwrap())
            .unwrap();
        assert!(local_blob_path.exists());
        let mut raw = std::fs::read(&local_blob_path).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xFF;
        std::fs::write(&local_blob_path, &raw).unwrap();

        let out_tmp = vault.next_cloud_tmp_path("gdrive-up", "tzmb");
        let prep_res = vault
            .prepare_cloud_encrypted_temp(&obj, &cloud_key, &out_tmp)
            .await;
        assert!(prep_res.is_err());
        assert!(
            !local_blob_path.exists(),
            "corrupted local encrypted blob should be unlinked to avoid head-of-line blocking"
        );
        assert!(!out_tmp.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn clear_cache_preserves_active_transfer_tmp_files() {
        let dir = test_temp_dir("clear-cache-tmp-safe");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u_tmp"))).unwrap(),
        ));
        let vault = MediaVault::open(&paths, Some("u_tmp"), MediaVaultConfig::default()).unwrap();

        let active_up = vault.next_cloud_tmp_path("gdrive-up", "tzmb");
        let active_dl = vault.next_cloud_tmp_path("gdrive-dl", "tzmb");
        let active_part = vault.next_tmp_path("dl", "part");
        let active_rehydrate = vault.next_tmp_path("cloud-rehydrate", "bin");
        let stale_other = vault.vault_dir().join("tmp").join("orphan-scratch.txt");
        for p in [
            &active_up,
            &active_dl,
            &active_part,
            &active_rehydrate,
            &stale_other,
        ] {
            std::fs::write(p, b"in-flight").unwrap();
        }

        let _ = vault.clear_cache(&store, true).await.unwrap();

        assert!(active_up.exists(), "active gdrive-up temp file must survive clear_cache");
        assert!(active_dl.exists(), "active gdrive-dl temp file must survive clear_cache");
        assert!(active_part.exists(), "active dl-*.part temp file must survive clear_cache");
        assert!(
            active_rehydrate.exists(),
            "active cloud-rehydrate temp file must survive clear_cache"
        );
        assert!(
            !stale_other.exists(),
            "non-transfer scratch file should still be removed by clear_cache"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
