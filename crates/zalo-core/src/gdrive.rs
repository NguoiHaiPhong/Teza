//! Google Drive End-to-End Encrypted (E2EE) Media Backup, Delta Message History Sync,
//! and Cloud Storage Manager engine.
//!
//! ## Security & Architecture Invariants
//! 1. **Strict Relay Exclusion**: Every network entry point checks `relay_enabled` and immediately
//!    returns `Err(GDriveError::RelayBlocked)` if LAN Relay is enabled. The internal HTTP client
//!    always sets `.no_proxy()`.
//! 2. **Minimal OAuth Scope**: Uses OAuth 2.0 Authorization Code + PKCE (`S256`) on an ephemeral
//!    loopback port (`127.0.0.1:0`) requesting ONLY `https://www.googleapis.com/auth/drive.file`.
//! 3. **2-Layer E2EE**: A random 256-bit `CloudMasterKey` encrypts media blobs (`TZMB` v1) and
//!    compressed history deltas (gzip + AES-256-GCM). The master key is wrapped in `manifest.enc.json`
//!    using PBKDF2-HMAC-SHA256 (600,000 iterations) derived from the user's vault passphrase.
//! 4. **O(1) RAM Streaming & Zero Size Caps**: Resumable uploads stream files in exact 4 MiB
//!    (`4 * 1024 * 1024`, multiple of 256 KiB) chunks, and downloads stream chunk-by-chunk to disk
//!    with no artificial per-file size cap or artificial cloud quota cap.

use std::fmt;
use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE, RETRY_AFTER};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::data_dir;
use crate::dpapi;
use crate::media_vault::constant_time_eq;
use crate::store::MessageStore;

pub use crate::store::{
    CloudMediaBreakdown, GDriveCloudItemDto, GDriveThreadUsageDto, HistoryDeltaBatch,
};

pub const GDRIVE_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
pub const GDRIVE_PROVIDER_NAME: &str = "gdrive";
pub const DEFAULT_AUTO_SYNC_INTERVAL_SECS: u64 = 300;
pub const OAUTH_CALLBACK_TIMEOUT_SECS: u64 = 180;
pub const MIN_VAULT_PASSPHRASE_LEN: usize = 8;

pub const VAULT_MANIFEST_VERSION: u32 = 1;
pub const VAULT_MANIFEST_KDF_NAME: &str = "pbkdf2-hmac-sha256";
pub const VAULT_MANIFEST_KDF_ITERS: u32 = 600_000;
const MIN_ALLOWED_KDF_ITERS: u32 = 100_000;
const MAX_ALLOWED_KDF_ITERS: u32 = 5_000_000;

pub const HISTORY_DELTA_WIRE_VERSION: u8 = 1;
const MAX_DECOMPRESSED_DELTA_BYTES: u64 = 128 * 1024 * 1024;
const HISTORY_DELTA_MSG_LIMIT: usize = 2000;
const MAX_HISTORY_UPLOAD_PAGES_PER_CYCLE: usize = 10;

/// Exact 4 MiB chunk size (16 * 256 KiB) required by Google Drive Resumable Upload API.
pub const RESUMABLE_CHUNK_SIZE: usize = 4 * 1024 * 1024;
const SMALL_UPLOAD_LIMIT: usize = RESUMABLE_CHUNK_SIZE;
const MAX_API_RETRIES: usize = 4;

pub const DRIVE_ROOT_FOLDER_NAME: &str = "Teza Vault";
pub const DRIVE_README_FILE_NAME: &str = "README.txt";
pub const DRIVE_MANIFEST_FILE_NAME: &str = "manifest.enc.json";
pub const DRIVE_BLOBS_FOLDER_NAME: &str = "blobs";
pub const DRIVE_HISTORY_FOLDER_NAME: &str = "history";
const DRIVE_FOLDER_MIME: &str = "application/vnd.google-apps.folder";

pub const GDRIVE_PHASE_IDLE: &str = "idle";
pub const GDRIVE_PHASE_CONNECTING: &str = "connecting";
pub const GDRIVE_PHASE_UPLOADING_MEDIA: &str = "uploading_media";
pub const GDRIVE_PHASE_SYNCING_HISTORY: &str = "syncing_history";
pub const GDRIVE_PHASE_REHYDRATING: &str = "rehydrating";
pub const GDRIVE_PHASE_PRUNING: &str = "pruning";
pub const GDRIVE_PHASE_PAUSED_RELAY: &str = "paused_relay";
pub const GDRIVE_PHASE_ERROR: &str = "error";

const DRIVE_README_TEXT: &str = "\
Teza Encrypted Vault (Google Drive Backup)\r\n\
==========================================\r\n\
\r\n\
This folder stores end-to-end encrypted backups created by Teza Desktop.\r\n\
- manifest.enc.json : Passphrase-wrapped encryption key (PBKDF2-HMAC-SHA256 600k + AES-256-GCM)\r\n\
- blobs/            : Encrypted media files (TZMB v1 64 KiB chunked AES-256-GCM)\r\n\
- history/          : Compressed, encrypted delta chat history snapshots\r\n\
\r\n\
Do not modify or rename files manually. Use Teza Settings -> Storage -> Cloud Storage Manager\r\n\
to inspect or prune cloud items safely.\r\n";

#[derive(Debug, Error)]
pub enum GDriveError {
    #[error("Google Drive sync is disabled while LAN Relay is active")]
    RelayBlocked,
    #[error("Google Drive OAuth credentials are not configured")]
    NotConfigured,
    #[error("Google Drive is not connected")]
    NotConnected,
    #[error("Google Drive E2EE vault is locked")]
    VaultLocked,
    #[error("Invalid E2EE passphrase or corrupted vault manifest")]
    InvalidPassphrase,
    #[error("Google Drive sign-in was cancelled")]
    AuthCancelled,
    #[error("Google Drive sign-in timed out")]
    AuthTimeout,
    #[error("Google Drive API rate limit exceeded")]
    RateLimited,
    #[error("Google Drive đã hết dung lượng trống (storageQuotaExceeded). Hãy giải phóng dung lượng trên Google Drive hoặc dùng Trình quản lý dung lượng Cloud để xóa bớt tệp cũ.")]
    StorageQuotaExceeded,
    #[error("Google Drive file not found")]
    NotFound,
    #[error("Integrity check failed on Google Drive payload")]
    IntegrityMismatch,
    #[error("Google Drive API error: {0}")]
    Api(String),
    #[error("{0}")]
    Other(String),
}

pub type GDriveResult<T> = std::result::Result<T, GDriveError>;

fn volatile_zeroize(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned mutable reference to a single byte inside `bytes`.
        unsafe {
            std::ptr::write_volatile(b, 0);
        }
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

fn zeroize_string(s: &mut String) {
    // SAFETY: Overwriting UTF-8 string bytes with NUL bytes keeps valid UTF-8 and clears RAM before drop.
    unsafe {
        volatile_zeroize(s.as_bytes_mut());
    }
    s.clear();
}

fn encode_hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn default_true() -> bool {
    true
}

fn default_auto_sync_interval_secs() -> u64 {
    DEFAULT_AUTO_SYNC_INTERVAL_SECS
}

/// Immediate strict Relay guard invoked at the start of every Google Drive network entry point.
pub fn ensure_relay_disabled(relay_enabled: bool) -> GDriveResult<()> {
    if relay_enabled {
        Err(GDriveError::RelayBlocked)
    } else {
        Ok(())
    }
}

/// Compile-time optional built-in OAuth client ID (`TEZA_GDRIVE_CLIENT_ID`).
pub fn builtin_oauth_client_id() -> Option<&'static str> {
    option_env!("TEZA_GDRIVE_CLIENT_ID")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Compile-time optional built-in OAuth client secret (`TEZA_GDRIVE_CLIENT_SECRET`).
pub fn builtin_oauth_client_secret() -> Option<&'static str> {
    option_env!("TEZA_GDRIVE_CLIENT_SECRET")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Compile-time optional built-in zero-secret OAuth token broker base URL (`TEZA_GDRIVE_TOKEN_BROKER_URL`).
/// When configured alongside `TEZA_GDRIVE_CLIENT_ID`, the desktop binary embeds NO `client_secret`:
/// `/token` and `/refresh` exchange requests go through the broker, which injects the secret server-side.
pub fn builtin_oauth_token_broker_url() -> Option<String> {
    option_env!("TEZA_GDRIVE_TOKEN_BROKER_URL")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|u| validate_token_broker_url(u).ok().flatten())
}

/// Validates and normalizes an OAuth Token Broker base URL.
/// Empty strings clear the broker (`Ok(None)`). Non-empty URLs must be `https://`
/// (or `http://127.0.0.1` / `http://localhost` for local testing) with no credentials, query, or fragment.
pub fn validate_token_broker_url(raw: &str) -> GDriveResult<Option<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed = Url::parse(trimmed)
        .map_err(|_| GDriveError::Api("URL Token Broker không hợp lệ".into()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| GDriveError::Api("URL Token Broker thiếu tên miền".into()))?;
    let is_loopback = host == "127.0.0.1" || host == "localhost" || host == "[::1]";
    match parsed.scheme() {
        "https" => {}
        "http" if is_loopback => {}
        _ => {
            return Err(GDriveError::Api(
                "URL Token Broker bắt buộc sử dụng HTTPS".into(),
            ));
        }
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(GDriveError::Api(
            "URL Token Broker không được chứa thông tin đăng nhập".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(GDriveError::Api(
            "URL Token Broker không được chứa query hoặc fragment".into(),
        ));
    }
    Ok(Some(trimmed.trim_end_matches('/').to_string()))
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_broker_url: Option<String>,
    pub mode: &'static str,
}

impl fmt::Debug for ResolvedOAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedOAuthConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_broker_url", &self.token_broker_url)
            .field("mode", &self.mode)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveConfigDto {
    pub client_id: String,
    pub has_client_secret: bool,
    pub has_builtin_oauth: bool,
    #[serde(default)]
    pub token_broker_url: String,
    #[serde(default)]
    pub has_builtin_token_broker: bool,
    #[serde(default)]
    pub oauth_mode: String,
    pub sync_enabled: bool,
    pub sync_media: bool,
    pub sync_history: bool,
    pub auto_sync_interval_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveStatusDto {
    pub relay_blocked: bool,
    pub oauth_configured: bool,
    pub connected: bool,
    pub account_email: Option<String>,
    pub account_name: Option<String>,
    pub vault_unlocked: bool,
    pub has_remote_manifest: bool,
    pub sync_enabled: bool,
    pub sync_media: bool,
    pub sync_history: bool,
    pub syncing: bool,
    pub phase: String,
    pub progress_done: u64,
    pub progress_total: u64,
    pub pending_media_count: u64,
    pub pending_media_bytes: u64,
    pub last_sync_ts: Option<i64>,
    pub last_history_sync_ts: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveQuotaDto {
    pub drive_limit_bytes: Option<u64>,
    pub drive_usage_bytes: u64,
    pub drive_usage_in_drive_bytes: u64,
    pub drive_usage_in_trash_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveStorageOverviewDto {
    pub quota: Option<GDriveQuotaDto>,
    pub teza_cloud_total_bytes: u64,
    pub teza_cloud_object_count: u64,
    pub images_bytes: u64,
    pub videos_bytes: u64,
    pub voice_bytes: u64,
    pub files_bytes: u64,
    pub history_bytes: u64,
    pub history_file_count: u64,
    pub by_thread: Vec<GDriveThreadUsageDto>,
    pub largest_items: Vec<GDriveCloudItemDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDrivePruneResultDto {
    pub deleted_count: u64,
    pub freed_bytes: u64,
    pub failed_count: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GDriveStoredConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret_dpapi_b64: Option<String>,
    #[serde(default)]
    pub token_broker_url: String,
    #[serde(default = "default_true")]
    pub sync_enabled: bool,
    #[serde(default = "default_true")]
    pub sync_media: bool,
    #[serde(default = "default_true")]
    pub sync_history: bool,
    #[serde(default = "default_auto_sync_interval_secs")]
    pub auto_sync_interval_secs: u64,
}

impl Default for GDriveStoredConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret_dpapi_b64: None,
            token_broker_url: String::new(),
            sync_enabled: true,
            sync_media: true,
            sync_history: true,
            auto_sync_interval_secs: DEFAULT_AUTO_SYNC_INTERVAL_SECS,
        }
    }
}

impl fmt::Debug for GDriveStoredConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GDriveStoredConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret_dpapi_b64",
                &self.client_secret_dpapi_b64.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_broker_url", &self.token_broker_url)
            .field("sync_enabled", &self.sync_enabled)
            .field("sync_media", &self.sync_media)
            .field("sync_history", &self.sync_history)
            .field("auto_sync_interval_secs", &self.auto_sync_interval_secs)
            .finish()
    }
}

impl GDriveStoredConfig {
    pub fn load_from_file(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let mut cfg = serde_json::from_str::<Self>(&raw).unwrap_or_default();
        cfg.client_id = cfg.client_id.trim().to_string();
        cfg.client_secret_dpapi_b64 = cfg
            .client_secret_dpapi_b64
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        cfg.token_broker_url = validate_token_broker_url(&cfg.token_broker_url)
            .ok()
            .flatten()
            .unwrap_or_default();
        if cfg.auto_sync_interval_secs < 30 {
            cfg.auto_sync_interval_secs = DEFAULT_AUTO_SYNC_INTERVAL_SECS;
        }
        cfg
    }

    pub fn save_to_file(&self, path: &Path) -> GDriveResult<()> {
        let mut clean = self.clone();
        clean.client_id = clean.client_id.trim().to_string();
        clean.client_secret_dpapi_b64 = clean
            .client_secret_dpapi_b64
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        clean.token_broker_url = validate_token_broker_url(&clean.token_broker_url)?
            .unwrap_or_default();
        if clean.auto_sync_interval_secs < 30 {
            clean.auto_sync_interval_secs = DEFAULT_AUTO_SYNC_INTERVAL_SECS;
        }
        let json = serde_json::to_string_pretty(&clean)
            .map_err(|e| GDriveError::Other(format!("Failed to serialize gdrive config: {e}")))?;
        data_dir::write_private_atomic(path, json.as_bytes())
            .map_err(|e| GDriveError::Other(format!("Failed to persist gdrive config: {e}")))
    }

    /// Seals a BYO client secret using Windows DPAPI (or 0600 fallback on non-Windows).
    /// Passing `Some("")` clears the stored client secret.
    pub fn set_client_secret(&mut self, plain_secret: Option<&str>) -> GDriveResult<()> {
        match plain_secret.map(str::trim) {
            None => {}
            Some("") => {
                self.client_secret_dpapi_b64 = None;
            }
            Some(secret) => {
                let sealed = dpapi::protect_secret(secret).map_err(|e| {
                    GDriveError::Other(format!("Failed to seal OAuth client secret: {e}"))
                })?;
                self.client_secret_dpapi_b64 = Some(sealed);
            }
        }
        Ok(())
    }

    /// Sets or clears a custom OAuth Token Broker base URL (must be HTTPS, or loopback for dev).
    pub fn set_token_broker_url(&mut self, raw_url: Option<&str>) -> GDriveResult<()> {
        if let Some(raw) = raw_url {
            self.token_broker_url = validate_token_broker_url(raw)?.unwrap_or_default();
        }
        Ok(())
    }

    /// Clears all custom BYO OAuth overrides so the app falls back to built-in Teza OAuth defaults.
    pub fn reset_custom_oauth(&mut self) {
        self.client_id.clear();
        self.client_secret_dpapi_b64 = None;
        self.token_broker_url.clear();
    }

    pub fn is_oauth_configured(&self) -> bool {
        !self.client_id.trim().is_empty() || builtin_oauth_client_id().is_some()
    }

    pub fn oauth_mode(&self) -> &'static str {
        if !self.client_id.trim().is_empty() {
            if validate_token_broker_url(&self.token_broker_url)
                .ok()
                .flatten()
                .is_some()
            {
                "custom_broker"
            } else {
                "custom_direct"
            }
        } else if builtin_oauth_client_id().is_some() {
            if builtin_oauth_token_broker_url().is_some() {
                "builtin_broker"
            } else {
                "builtin_direct"
            }
        } else {
            "unconfigured"
        }
    }

    /// Resolves effective OAuth config (`client_id`, optional `client_secret`, optional `token_broker_url`)
    /// preferring user BYO credentials and falling back to compile-time `TEZA_GDRIVE_*` env vars.
    pub fn resolve_oauth_config(&self) -> GDriveResult<ResolvedOAuthConfig> {
        let custom_id = self.client_id.trim();
        if !custom_id.is_empty() {
            let custom_broker = validate_token_broker_url(&self.token_broker_url)?;
            if let Some(broker_url) = custom_broker {
                return Ok(ResolvedOAuthConfig {
                    client_id: custom_id.to_string(),
                    client_secret: None,
                    token_broker_url: Some(broker_url),
                    mode: "custom_broker",
                });
            }
            let secret = match self.client_secret_dpapi_b64.as_deref() {
                Some(sealed) if !sealed.trim().is_empty() => {
                    let unsealed = dpapi::unprotect_secret(sealed.trim()).map_err(|e| {
                        GDriveError::Other(format!(
                            "Failed to unseal stored OAuth client secret: {e}"
                        ))
                    })?;
                    let trimmed = unsealed.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                }
                _ => None,
            };
            return Ok(ResolvedOAuthConfig {
                client_id: custom_id.to_string(),
                client_secret: secret,
                token_broker_url: None,
                mode: "custom_direct",
            });
        }
        if let Some(builtin_id) = builtin_oauth_client_id() {
            if let Some(broker_url) = builtin_oauth_token_broker_url() {
                return Ok(ResolvedOAuthConfig {
                    client_id: builtin_id.to_string(),
                    client_secret: None,
                    token_broker_url: Some(broker_url),
                    mode: "builtin_broker",
                });
            }
            return Ok(ResolvedOAuthConfig {
                client_id: builtin_id.to_string(),
                client_secret: builtin_oauth_client_secret().map(str::to_string),
                token_broker_url: None,
                mode: "builtin_direct",
            });
        }
        Err(GDriveError::NotConfigured)
    }

    /// Resolves effective `(client_id, optional_client_secret)` preferring user BYO credentials
    /// and falling back to compile-time `TEZA_GDRIVE_CLIENT_ID`/`TEZA_GDRIVE_CLIENT_SECRET`.
    pub fn resolve_oauth_credentials(&self) -> GDriveResult<(String, Option<String>)> {
        let resolved = self.resolve_oauth_config()?;
        Ok((resolved.client_id, resolved.client_secret))
    }

    pub fn to_dto(&self) -> GDriveConfigDto {
        GDriveConfigDto {
            client_id: self.client_id.clone(),
            has_client_secret: self
                .client_secret_dpapi_b64
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()),
            has_builtin_oauth: builtin_oauth_client_id().is_some(),
            token_broker_url: self.token_broker_url.clone(),
            has_builtin_token_broker: builtin_oauth_token_broker_url().is_some(),
            oauth_mode: self.oauth_mode().to_string(),
            sync_enabled: self.sync_enabled,
            sync_media: self.sync_media,
            sync_history: self.sync_history,
            auto_sync_interval_secs: self.auto_sync_interval_secs,
        }
    }
}

/// Loads or lazily generates a stable per-installation random device identifier for delta naming.
pub fn load_or_create_device_id(path: &Path) -> GDriveResult<String> {
    if let Ok(raw) = std::fs::read_to_string(path) {
        let clean: String = raw
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(48)
            .collect();
        if clean.len() >= 8 {
            return Ok(clean);
        }
    }
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd)
        .map_err(|e| GDriveError::Other(format!("CSPRNG failed generating device ID: {e}")))?;
    let id = format!("dev-{}", encode_hex_lower(&rnd));
    data_dir::write_private_atomic(path, id.as_bytes())
        .map_err(|e| GDriveError::Other(format!("Failed to persist device ID: {e}")))?;
    Ok(id)
}

/// DPAPI-sealed OAuth token set & cached Drive folder IDs with redacted `Debug` and zeroized `Drop`.
#[derive(Clone, Serialize, Deserialize)]
pub struct GDriveTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_epoch_secs: i64,
    #[serde(default)]
    pub account_email: Option<String>,
    #[serde(default)]
    pub account_name: Option<String>,
    #[serde(default)]
    pub root_folder_id: Option<String>,
    #[serde(default)]
    pub uid_folder_id: Option<String>,
    #[serde(default)]
    pub blobs_folder_id: Option<String>,
    #[serde(default)]
    pub history_folder_id: Option<String>,
    #[serde(default)]
    pub manifest_file_id: Option<String>,
}

impl fmt::Debug for GDriveTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GDriveTokens")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at_epoch_secs", &self.expires_at_epoch_secs)
            .field("account_email", &self.account_email)
            .field("account_name", &self.account_name)
            .field("root_folder_id", &self.root_folder_id)
            .field("uid_folder_id", &self.uid_folder_id)
            .field("blobs_folder_id", &self.blobs_folder_id)
            .field("history_folder_id", &self.history_folder_id)
            .field("manifest_file_id", &self.manifest_file_id)
            .finish()
    }
}

impl Drop for GDriveTokens {
    fn drop(&mut self) {
        zeroize_string(&mut self.access_token);
        zeroize_string(&mut self.refresh_token);
    }
}

impl GDriveTokens {
    pub fn is_access_token_fresh(&self, now_epoch_secs: i64) -> bool {
        !self.access_token.is_empty() && now_epoch_secs.saturating_add(60) < self.expires_at_epoch_secs
    }

    pub fn load_from_file(path: &Path) -> GDriveResult<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let sealed = std::fs::read_to_string(path)
            .map_err(|e| GDriveError::Other(format!("Failed to read gdrive tokens file: {e}")))?;
        let mut plain = dpapi::unprotect_secret(sealed.trim())
            .map_err(|e| GDriveError::Other(format!("Failed to unseal gdrive tokens: {e}")))?;
        let parsed = serde_json::from_str::<Self>(&plain)
            .map_err(|e| GDriveError::Other(format!("Corrupted gdrive tokens payload: {e}")));
        zeroize_string(&mut plain);
        parsed.map(Some)
    }

    pub fn save_to_file(&self, path: &Path) -> GDriveResult<()> {
        let mut json = serde_json::to_string(self)
            .map_err(|e| GDriveError::Other(format!("Failed to serialize gdrive tokens: {e}")))?;
        let sealed_res = dpapi::protect_secret(&json);
        zeroize_string(&mut json);
        let sealed = sealed_res
            .map_err(|e| GDriveError::Other(format!("Failed to seal gdrive tokens: {e}")))?;
        data_dir::write_private_atomic(path, sealed.as_bytes())
            .map_err(|e| GDriveError::Other(format!("Failed to save gdrive tokens: {e}")))
    }

    pub fn delete_file(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// 256-bit cloud vault master key with volatile zeroization on `Drop` and redacted `Debug`.
#[derive(Clone)]
pub struct CloudMasterKey([u8; 32]);

impl CloudMasterKey {
    pub fn generate() -> GDriveResult<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| GDriveError::Other(format!("OS CSPRNG failure: {e}")))?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn load_from_file(path: &Path) -> GDriveResult<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let sealed = std::fs::read_to_string(path).map_err(|e| {
            GDriveError::Other(format!("Failed to read cloud master key file: {e}"))
        })?;
        let mut plain = dpapi::unprotect_secret(sealed.trim())
            .map_err(|e| GDriveError::Other(format!("Failed to unseal cloud master key: {e}")))?;
        let decoded = B64
            .decode(plain.trim())
            .map_err(|e| GDriveError::Other(format!("Invalid cloud master key encoding: {e}")));
        zeroize_string(&mut plain);
        let mut raw = decoded?;
        if raw.len() != 32 {
            volatile_zeroize(&mut raw);
            return Err(GDriveError::Other(
                "Invalid cloud master key length (expected 32 bytes)".into(),
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw);
        volatile_zeroize(&mut raw);
        Ok(Some(Self(key)))
    }

    pub fn save_to_file(&self, path: &Path) -> GDriveResult<()> {
        let mut b64 = B64.encode(self.0);
        let sealed_res = dpapi::protect_secret(&b64);
        zeroize_string(&mut b64);
        let sealed = sealed_res
            .map_err(|e| GDriveError::Other(format!("Failed to seal cloud master key: {e}")))?;
        data_dir::write_private_atomic(path, sealed.as_bytes())
            .map_err(|e| GDriveError::Other(format!("Failed to save cloud master key: {e}")))
    }

    pub fn delete_file(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

impl fmt::Debug for CloudMasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CloudMasterKey([REDACTED])")
    }
}

impl Drop for CloudMasterKey {
    fn drop(&mut self) {
        volatile_zeroize(&mut self.0);
    }
}

/// Passphrase-wrapped 2-layer E2EE manifest stored as `Teza Vault/<uid>/manifest.enc.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultManifest {
    pub version: u32,
    pub kdf: String,
    pub kdf_iterations: u32,
    pub salt_b64: String,
    pub nonce_b64: String,
    pub wrapped_key_b64: String,
    pub key_ver: i32,
    pub created_ts: i64,
}

fn manifest_aad(uid: &str) -> Vec<u8> {
    format!("teza-gdrive-manifest-v1:{uid}").into_bytes()
}

fn history_aad(uid: &str) -> Vec<u8> {
    format!("teza-gdrive-history-v1:{uid}").into_bytes()
}

/// Wraps an existing `CloudMasterKey` into a `VaultManifest` using PBKDF2-HMAC-SHA256 (600,000 iters)
/// inside `spawn_blocking` so the async runtime is never blocked.
pub async fn wrap_master_key_manifest(
    uid: &str,
    passphrase: &str,
    master_key: &CloudMasterKey,
) -> GDriveResult<VaultManifest> {
    if passphrase.chars().count() < MIN_VAULT_PASSPHRASE_LEN {
        return Err(GDriveError::InvalidPassphrase);
    }
    let mut salt = [0u8; 32];
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut salt)
        .map_err(|e| GDriveError::Other(format!("OS CSPRNG failure: {e}")))?;
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| GDriveError::Other(format!("OS CSPRNG failure: {e}")))?;

    let mut pass_owned = passphrase.to_string();
    let mut key_copy = *master_key.as_bytes();
    let aad = manifest_aad(uid);

    let wrapped = tokio::task::spawn_blocking(move || -> GDriveResult<Vec<u8>> {
        let mut kek = [0u8; 32];
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(VAULT_MANIFEST_KDF_ITERS).expect("non-zero PBKDF2 iterations"),
            &salt,
            pass_owned.as_bytes(),
            &mut kek,
        );
        zeroize_string(&mut pass_owned);

        let cipher = Aes256Gcm::new_from_slice(&kek).expect("32-byte KEK");
        let enc_res = cipher.encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &key_copy,
                aad: &aad,
            },
        );
        volatile_zeroize(&mut kek);
        volatile_zeroize(&mut key_copy);
        enc_res.map_err(|_| GDriveError::Other("Failed to wrap cloud master key".into()))
    })
    .await
    .map_err(|e| GDriveError::Other(format!("PBKDF2 wrap task failed: {e}")))??;

    Ok(VaultManifest {
        version: VAULT_MANIFEST_VERSION,
        kdf: VAULT_MANIFEST_KDF_NAME.to_string(),
        kdf_iterations: VAULT_MANIFEST_KDF_ITERS,
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        wrapped_key_b64: B64.encode(wrapped),
        key_ver: 1,
        created_ts: chrono::Utc::now().timestamp_millis(),
    })
}

/// Generates a fresh 256-bit `CloudMasterKey` and wraps it into a `VaultManifest`.
pub async fn create_vault_manifest(
    uid: &str,
    passphrase: &str,
) -> GDriveResult<(VaultManifest, CloudMasterKey)> {
    let master_key = CloudMasterKey::generate()?;
    let manifest = wrap_master_key_manifest(uid, passphrase, &master_key).await?;
    Ok((manifest, master_key))
}

/// Unwraps `CloudMasterKey` from a remote `VaultManifest` using PBKDF2-HMAC-SHA256 in `spawn_blocking`.
/// Rejects wrong passphrases, wrong `uid` (AAD mismatch), or tampered manifest fields.
pub async fn unwrap_master_key_manifest(
    uid: &str,
    passphrase: &str,
    manifest: &VaultManifest,
) -> GDriveResult<CloudMasterKey> {
    if manifest.version != VAULT_MANIFEST_VERSION
        || manifest.kdf != VAULT_MANIFEST_KDF_NAME
        || manifest.kdf_iterations < MIN_ALLOWED_KDF_ITERS
        || manifest.kdf_iterations > MAX_ALLOWED_KDF_ITERS
    {
        return Err(GDriveError::InvalidPassphrase);
    }
    let salt = B64
        .decode(manifest.salt_b64.trim())
        .map_err(|_| GDriveError::InvalidPassphrase)?;
    if salt.len() < 16 {
        return Err(GDriveError::InvalidPassphrase);
    }
    let nonce_vec = B64
        .decode(manifest.nonce_b64.trim())
        .map_err(|_| GDriveError::InvalidPassphrase)?;
    let nonce_bytes: [u8; 12] = nonce_vec
        .try_into()
        .map_err(|_| GDriveError::InvalidPassphrase)?;
    let wrapped = B64
        .decode(manifest.wrapped_key_b64.trim())
        .map_err(|_| GDriveError::InvalidPassphrase)?;
    if wrapped.len() < 32 + 16 {
        return Err(GDriveError::InvalidPassphrase);
    }

    let iters = NonZeroU32::new(manifest.kdf_iterations).ok_or(GDriveError::InvalidPassphrase)?;
    let mut pass_owned = passphrase.to_string();
    let aad = manifest_aad(uid);

    tokio::task::spawn_blocking(move || -> GDriveResult<CloudMasterKey> {
        let mut kek = [0u8; 32];
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            iters,
            &salt,
            pass_owned.as_bytes(),
            &mut kek,
        );
        zeroize_string(&mut pass_owned);

        let cipher = Aes256Gcm::new_from_slice(&kek).expect("32-byte KEK");
        let dec_res = cipher.decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &wrapped,
                aad: &aad,
            },
        );
        volatile_zeroize(&mut kek);

        let mut plain = dec_res.map_err(|_| GDriveError::InvalidPassphrase)?;
        if plain.len() != 32 {
            volatile_zeroize(&mut plain);
            return Err(GDriveError::InvalidPassphrase);
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&plain);
        volatile_zeroize(&mut plain);
        Ok(CloudMasterKey::from_bytes(key))
    })
    .await
    .map_err(|e| GDriveError::Other(format!("PBKDF2 unwrap task failed: {e}")))?
}

/// Serializes, gzip-compresses, and AES-256-GCM encrypts a `HistoryDeltaBatch`.
/// Wire format: `version(1B) || nonce(12B) || ciphertext+tag`.
pub fn encrypt_history_delta(
    batch: &HistoryDeltaBatch,
    master_key: &CloudMasterKey,
    uid: &str,
) -> GDriveResult<Vec<u8>> {
    let json = serde_json::to_vec(batch)
        .map_err(|e| GDriveError::Other(format!("Failed to serialize history delta: {e}")))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&json)
        .map_err(|e| GDriveError::Other(format!("Failed to compress history delta: {e}")))?;
    let compressed = encoder
        .finish()
        .map_err(|e| GDriveError::Other(format!("Failed to finish gzip history delta: {e}")))?;

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|e| GDriveError::Other(format!("OS CSPRNG failure: {e}")))?;
    let cipher = Aes256Gcm::new_from_slice(master_key.as_bytes()).expect("32-byte master key");
    let aad = history_aad(uid);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &compressed,
                aad: &aad,
            },
        )
        .map_err(|_| GDriveError::Other("Failed to encrypt history delta".into()))?;

    let mut out = Vec::with_capacity(1 + 12 + ct.len());
    out.push(HISTORY_DELTA_WIRE_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypts, gunzips, and deserializes an encrypted `HistoryDeltaBatch`.
pub fn decrypt_history_delta(
    bytes: &[u8],
    master_key: &CloudMasterKey,
    uid: &str,
) -> GDriveResult<HistoryDeltaBatch> {
    if bytes.len() < 1 + 12 + 16 {
        return Err(GDriveError::IntegrityMismatch);
    }
    if bytes[0] != HISTORY_DELTA_WIRE_VERSION {
        return Err(GDriveError::IntegrityMismatch);
    }
    let nonce = Nonce::from_slice(&bytes[1..13]);
    let ct = &bytes[13..];
    let cipher = Aes256Gcm::new_from_slice(master_key.as_bytes()).expect("32-byte master key");
    let aad = history_aad(uid);
    let compressed = cipher
        .decrypt(nonce, Payload { msg: ct, aad: &aad })
        .map_err(|_| GDriveError::IntegrityMismatch)?;

    let decoder = GzDecoder::new(compressed.as_slice());
    let mut limited = decoder.take(MAX_DECOMPRESSED_DELTA_BYTES + 1);
    let mut json_bytes = Vec::new();
    limited
        .read_to_end(&mut json_bytes)
        .map_err(|_| GDriveError::IntegrityMismatch)?;
    if (json_bytes.len() as u64) > MAX_DECOMPRESSED_DELTA_BYTES {
        return Err(GDriveError::IntegrityMismatch);
    }
    serde_json::from_slice::<HistoryDeltaBatch>(&json_bytes)
        .map_err(|_| GDriveError::IntegrityMismatch)
}

/// Computes the RFC 7636 `S256` PKCE `code_challenge = BASE64URL-NOPAD(SHA256(code_verifier))`.
pub fn compute_pkce_s256_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    B64URL.encode(digest)
}

#[derive(Clone)]
pub struct PkceChallenge {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state: String,
}

impl fmt::Debug for PkceChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkceChallenge")
            .field("code_verifier", &"[REDACTED]")
            .field("code_challenge", &self.code_challenge)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl Drop for PkceChallenge {
    fn drop(&mut self) {
        zeroize_string(&mut self.code_verifier);
        zeroize_string(&mut self.state);
    }
}

pub fn generate_pkce_challenge() -> GDriveResult<PkceChallenge> {
    let mut verifier_raw = [0u8; 32];
    let mut state_raw = [0u8; 16];
    getrandom::fill(&mut verifier_raw)
        .map_err(|e| GDriveError::Other(format!("CSPRNG failed: {e}")))?;
    getrandom::fill(&mut state_raw)
        .map_err(|e| GDriveError::Other(format!("CSPRNG failed: {e}")))?;
    let code_verifier = B64URL.encode(verifier_raw);
    let state = B64URL.encode(state_raw);
    volatile_zeroize(&mut verifier_raw);
    volatile_zeroize(&mut state_raw);
    let code_challenge = compute_pkce_s256_challenge(&code_verifier);
    Ok(PkceChallenge {
        code_verifier,
        code_challenge,
        state,
    })
}

/// Builds the Google OAuth 2.0 consent URL strictly requesting `drive.file` scope with PKCE `S256`.
pub fn build_oauth_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> GDriveResult<String> {
    let cid = client_id.trim();
    if cid.is_empty() {
        return Err(GDriveError::NotConfigured);
    }
    let url = Url::parse_with_params(
        "https://accounts.google.com/o/oauth2/v2/auth",
        &[
            ("client_id", cid),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", GDRIVE_OAUTH_SCOPE),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ],
    )
    .map_err(|e| GDriveError::Other(format!("Failed to build OAuth authorize URL: {e}")))?;
    Ok(url.into())
}

/// Ephemeral loopback OAuth 2.0 PKCE receiver bound to `127.0.0.1:0`.
pub struct OAuthLoopbackFlow {
    listener: TcpListener,
    pub redirect_uri: String,
    pub auth_url: String,
    pkce: PkceChallenge,
}

impl fmt::Debug for OAuthLoopbackFlow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthLoopbackFlow")
            .field("redirect_uri", &self.redirect_uri)
            .field("pkce", &self.pkce)
            .finish()
    }
}

impl OAuthLoopbackFlow {
    pub async fn bind(relay_enabled: bool, client_id: &str) -> GDriveResult<Self> {
        ensure_relay_disabled(relay_enabled)?;
        if client_id.trim().is_empty() {
            return Err(GDriveError::NotConfigured);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| {
            GDriveError::Other(format!("Failed to bind local OAuth loopback listener: {e}"))
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| GDriveError::Other(format!("Failed to inspect loopback port: {e}")))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let pkce = generate_pkce_challenge()?;
        let auth_url =
            build_oauth_authorize_url(client_id, &redirect_uri, &pkce.code_challenge, &pkce.state)?;
        Ok(Self {
            listener,
            redirect_uri,
            auth_url,
            pkce,
        })
    }

    /// Awaits the browser loopback callback (timeout 180s) and validates `state` in constant time.
    /// Returns `(authorization_code, code_verifier, redirect_uri)`.
    pub async fn wait_for_code(
        self,
        cancel: Option<Arc<Notify>>,
    ) -> GDriveResult<(String, String, String)> {
        self.wait_for_code_with_timeout(cancel, Duration::from_secs(OAUTH_CALLBACK_TIMEOUT_SECS))
            .await
    }

    pub async fn wait_for_code_with_timeout(
        self,
        cancel: Option<Arc<Notify>>,
        timeout: Duration,
    ) -> GDriveResult<(String, String, String)> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(GDriveError::AuthTimeout);
            }

            let accept_res = if let Some(ref notify) = cancel {
                tokio::select! {
                    _ = notify.notified() => return Err(GDriveError::AuthCancelled),
                    res = tokio::time::timeout(remaining, self.listener.accept()) => res,
                }
            } else {
                tokio::time::timeout(remaining, self.listener.accept()).await
            };

            let (mut stream, _) = match accept_res {
                Ok(Ok(pair)) => pair,
                Ok(Err(e)) => {
                    return Err(GDriveError::Other(format!(
                        "OAuth loopback accept failed: {e}"
                    )))
                }
                Err(_) => return Err(GDriveError::AuthTimeout),
            };

            let mut buf = vec![0u8; 16 * 1024];
            let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) if n > 0 => n,
                _ => continue,
            };
            let req_text = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_text.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let target = parts.next().unwrap_or("");

            if method != "GET" || (!target.starts_with("/callback?") && target != "/callback") {
                let resp = "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nNot Found";
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
                continue;
            }

            let parsed = match Url::parse(&format!("http://127.0.0.1{target}")) {
                Ok(u) => u,
                Err(_) => {
                    write_oauth_html(
                        &mut stream,
                        "400 Bad Request",
                        false,
                        "Yêu cầu không hợp lệ · Invalid OAuth callback",
                        "Yêu cầu callback không hợp lệ. Bạn có thể đóng tab này và quay lại Teza. (Invalid callback request. You can close this tab and return to Teza.)",
                    )
                    .await;
                    continue;
                }
            };

            let mut got_code: Option<String> = None;
            let mut got_state: Option<String> = None;
            let mut got_error: Option<String> = None;
            for (k, v) in parsed.query_pairs() {
                match k.as_ref() {
                    "code" => got_code = Some(v.into_owned()),
                    "state" => got_state = Some(v.into_owned()),
                    "error" => got_error = Some(v.into_owned()),
                    _ => {}
                }
            }

            let state_ok = got_state
                .as_deref()
                .is_some_and(|s| constant_time_eq(s.as_bytes(), self.pkce.state.as_bytes()));
            if !state_ok {
                write_oauth_html(
                    &mut stream,
                    "400 Bad Request",
                    false,
                    "Xác thực bảo mật thất bại · Security check failed",
                    "Mã bảo vệ state không khớp. Bạn có thể đóng tab này và thử đăng nhập lại trong Teza. (OAuth state verification failed. You can close this tab and try again in Teza.)",
                )
                .await;
                return Err(GDriveError::Api("OAuth state mismatch".into()));
            }

            if let Some(err) = got_error {
                write_oauth_html(
                    &mut stream,
                    "200 OK",
                    false,
                    "Đã hủy đăng nhập · Sign-in cancelled",
                    "Bạn đã hủy kết nối Google Drive. Bạn có thể đóng tab này và quay lại Teza. (Google Drive sign-in was cancelled. You can close this tab and return to Teza.)",
                )
                .await;
                if err == "access_denied" {
                    return Err(GDriveError::AuthCancelled);
                }
                return Err(GDriveError::Api(format!("OAuth error: {err}")));
            }

            if let Some(code) = got_code.filter(|c| !c.trim().is_empty()) {
                write_oauth_html(
                    &mut stream,
                    "200 OK",
                    true,
                    "Đã kết nối Google Drive · Google Drive Connected",
                    "Đăng nhập thành công! Bạn có thể đóng tab này và quay lại Teza. (Sign-in succeeded! You can close this tab and return to Teza.)",
                )
                .await;
                return Ok((code, self.pkce.code_verifier.clone(), self.redirect_uri));
            }

            write_oauth_html(
                &mut stream,
                "400 Bad Request",
                false,
                "Thiếu mã xác thực · Missing authorization code",
                "Không nhận được mã xác thực từ Google. Bạn có thể đóng tab này và thử lại trong Teza. (No authorization code was received. You can close this tab and try again in Teza.)",
            )
            .await;
            return Err(GDriveError::Api(
                "Missing OAuth authorization code in callback".into(),
            ));
        }
    }
}

async fn write_oauth_html(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    ok: bool,
    title: &str,
    message: &str,
) {
    let badge_bg = if ok { "rgba(16,185,129,0.16)" } else { "rgba(239,68,68,0.16)" };
    let badge_color = if ok { "#34d399" } else { "#f87171" };
    let badge_icon = if ok { "&#10003;" } else { "!" };
    let body = format!(
        "<!DOCTYPE html><html lang=\"vi\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\"><title>Teza · {title}</title>\
         <style>body{{font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;background:#0b1120;color:#f8fafc;\
         display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;padding:16px;}}\
         .card{{background:#111827;border:1px solid #1f2937;border-radius:16px;padding:32px 34px;\
         max-width:460px;width:100%;text-align:center;box-shadow:0 16px 40px rgba(0,0,0,.45);}}\
         .badge{{width:48px;height:48px;border-radius:999px;margin:0 auto 16px;display:flex;align-items:center;\
         justify-content:center;font-size:24px;font-weight:700;background:{badge_bg};color:{badge_color};}}\
         .brand{{font-size:12px;letter-spacing:.08em;text-transform:uppercase;color:#60a5fa;font-weight:700;margin-bottom:8px;}}\
         h1{{margin:0 0 10px;font-size:19px;line-height:1.35;}}p{{margin:0;color:#cbd5e1;font-size:14px;line-height:1.6;}}</style>\
         </head><body><div class=\"card\"><div class=\"badge\">{badge_icon}</div><div class=\"brand\">Teza Desktop</div>\
         <h1>{title}</h1><p>{message}</p></div></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Safely escapes string literals for Google Drive `q=` query parameters (`\` and `'`, stripping controls).
pub fn escape_drive_query_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() {
            continue;
        }
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            other => out.push(other),
        }
    }
    out
}

fn sanitize_uid_folder_name(uid: &str) -> String {
    let clean: String = uid
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if clean.is_empty() {
        "default".to_string()
    } else {
        clean
    }
}

fn parse_drive_range_next_offset(range_header: &str) -> Option<u64> {
    let trimmed = range_header.trim();
    let rest = trimmed
        .strip_prefix("bytes=")
        .or_else(|| trimmed.strip_prefix("BYTES="))?;
    let (_, end_str) = rest.split_once('-')?;
    let end = end_str.trim().parse::<u64>().ok()?;
    end.checked_add(1)
}

fn is_retryable_drive_error(status: StatusCode, body: &str) -> bool {
    if status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::INTERNAL_SERVER_ERROR
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
    {
        return true;
    }
    if status == StatusCode::FORBIDDEN {
        let lower = body.to_ascii_lowercase();
        return lower.contains("ratelimitexceeded")
            || lower.contains("userratelimitexceeded")
            || lower.contains("backenderror");
    }
    false
}

fn compute_retry_delay(attempt: usize, retry_after_hdr: Option<&str>) -> Duration {
    if let Some(secs) = retry_after_hdr
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
    {
        return Duration::from_secs(secs.min(15));
    }
    let exp_ms = 500u64.saturating_mul(1u64 << attempt.min(4));
    Duration::from_millis(exp_ms.min(8_000))
}

async fn read_chunk_full(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveFileMeta {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub mime_type: String,
    pub modified_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultFolderIds {
    pub root_folder_id: String,
    pub uid_folder_id: String,
    pub blobs_folder_id: String,
    pub history_folder_id: String,
    pub manifest_file_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistorySyncReport {
    pub downloaded_deltas: usize,
    pub applied_messages: usize,
    pub uploaded_delta_id: Option<String>,
    pub new_last_history_sync_ts: i64,
}

#[derive(Debug, Clone)]
struct GDriveEndpoints {
    token_url: String,
    drive_api_base: String,
    drive_upload_base: String,
}

impl Default for GDriveEndpoints {
    fn default() -> Self {
        Self {
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            drive_api_base: "https://www.googleapis.com/drive/v3".to_string(),
            drive_upload_base: "https://www.googleapis.com/upload/drive/v3".to_string(),
        }
    }
}

/// Direct (`.no_proxy()`) Google Drive v3 + OAuth2 client with strict Relay exclusion.
#[derive(Clone, Debug)]
pub struct GDriveClient {
    http: Client,
    endpoints: GDriveEndpoints,
}

impl GDriveClient {
    pub fn new() -> GDriveResult<Self> {
        Self::with_endpoints(GDriveEndpoints::default())
    }

    fn with_endpoints(endpoints: GDriveEndpoints) -> GDriveResult<Self> {
        let http = Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .user_agent("TezaDesktop/1.0")
            .build()
            .map_err(|e| GDriveError::Other(format!("Failed to build direct Drive HTTP client: {e}")))?;
        Ok(Self { http, endpoints })
    }

    /// Exchanges an OAuth 2.0 PKCE authorization code for access + refresh tokens.
    /// When `token_broker_url` is `Some(base)`, exchanges via `{base}/token` JSON endpoint
    /// so no `client_secret` ever needs to reside on the desktop client.
    #[allow(clippy::too_many_arguments)]
    pub async fn exchange_oauth_code_with_broker(
        &self,
        relay_enabled: bool,
        client_id: &str,
        client_secret: Option<&str>,
        token_broker_url: Option<&str>,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> GDriveResult<GDriveTokens> {
        ensure_relay_disabled(relay_enabled)?;

        let resp = if let Some(broker_raw) = token_broker_url.map(str::trim).filter(|s| !s.is_empty())
        {
            let base = validate_token_broker_url(broker_raw)?
                .ok_or_else(|| GDriveError::Api("Invalid Token Broker URL".into()))?;
            let url = format!("{base}/token");
            self.http
                .post(&url)
                .json(&serde_json::json!({
                    "client_id": client_id,
                    "code": code,
                    "code_verifier": code_verifier,
                    "redirect_uri": redirect_uri,
                }))
                .send()
                .await
                .map_err(|e| {
                    GDriveError::Api(format!("OAuth Token Broker exchange request failed: {e}"))
                })?
        } else {
            let mut form: Vec<(&str, &str)> = vec![
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("code_verifier", code_verifier),
                ("redirect_uri", redirect_uri),
            ];
            if let Some(secret) = client_secret.map(str::trim).filter(|s| !s.is_empty()) {
                form.push(("client_secret", secret));
            }
            self.http
                .post(&self.endpoints.token_url)
                .form(&form)
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("OAuth token exchange request failed: {e}")))?
        };
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| GDriveError::Api(format!("Failed reading OAuth token response: {e}")))?;
        if !status.is_success() {
            return Err(GDriveError::Api(format!(
                "OAuth token exchange HTTP {}: {}",
                status.as_u16(),
                truncate_error_body(&body)
            )));
        }

        #[derive(Deserialize)]
        struct TokenResp {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: i64,
        }
        let parsed: TokenResp = serde_json::from_str(&body)
            .map_err(|e| GDriveError::Api(format!("Invalid OAuth token JSON: {e}")))?;
        let now = chrono::Utc::now().timestamp();
        let ttl = parsed.expires_in.max(60);
        Ok(GDriveTokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.unwrap_or_default(),
            expires_at_epoch_secs: now.saturating_add(ttl),
            account_email: None,
            account_name: None,
            root_folder_id: None,
            uid_folder_id: None,
            blobs_folder_id: None,
            history_folder_id: None,
            manifest_file_id: None,
        })
    }

    /// Direct OAuth 2.0 PKCE code exchange helper (no token broker).
    #[allow(clippy::too_many_arguments)]
    pub async fn exchange_oauth_code(
        &self,
        relay_enabled: bool,
        client_id: &str,
        client_secret: Option<&str>,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> GDriveResult<GDriveTokens> {
        self.exchange_oauth_code_with_broker(
            relay_enabled,
            client_id,
            client_secret,
            None,
            code,
            code_verifier,
            redirect_uri,
        )
        .await
    }

    /// Refreshes the OAuth access token if it expires within 60 seconds, optionally via Token Broker.
    /// Returns `Ok(true)` if the token was refreshed (caller should re-seal DPAPI tokens file).
    pub async fn refresh_access_token_with_broker(
        &self,
        relay_enabled: bool,
        client_id: &str,
        client_secret: Option<&str>,
        token_broker_url: Option<&str>,
        tokens: &mut GDriveTokens,
    ) -> GDriveResult<bool> {
        ensure_relay_disabled(relay_enabled)?;

        let now = chrono::Utc::now().timestamp();
        if tokens.is_access_token_fresh(now) {
            return Ok(false);
        }
        if tokens.refresh_token.trim().is_empty() {
            return Err(GDriveError::NotConnected);
        }

        let resp = if let Some(broker_raw) = token_broker_url.map(str::trim).filter(|s| !s.is_empty())
        {
            let base = validate_token_broker_url(broker_raw)?
                .ok_or_else(|| GDriveError::Api("Invalid Token Broker URL".into()))?;
            let url = format!("{base}/refresh");
            self.http
                .post(&url)
                .json(&serde_json::json!({
                    "client_id": client_id,
                    "refresh_token": tokens.refresh_token.as_str(),
                }))
                .send()
                .await
                .map_err(|e| {
                    GDriveError::Api(format!("OAuth Token Broker refresh request failed: {e}"))
                })?
        } else {
            let mut form: Vec<(&str, &str)> = vec![
                ("grant_type", "refresh_token"),
                ("refresh_token", tokens.refresh_token.as_str()),
                ("client_id", client_id),
            ];
            if let Some(secret) = client_secret.map(str::trim).filter(|s| !s.is_empty()) {
                form.push(("client_secret", secret));
            }
            self.http
                .post(&self.endpoints.token_url)
                .form(&form)
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("OAuth token refresh failed: {e}")))?
        };
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| GDriveError::Api(format!("Failed reading token refresh body: {e}")))?;
        if !status.is_success() {
            return Err(GDriveError::Api(format!(
                "OAuth token refresh HTTP {}: {}",
                status.as_u16(),
                truncate_error_body(&body)
            )));
        }

        #[derive(Deserialize)]
        struct RefreshResp {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: i64,
        }
        let parsed: RefreshResp = serde_json::from_str(&body)
            .map_err(|e| GDriveError::Api(format!("Invalid refresh token JSON: {e}")))?;
        let ttl = parsed.expires_in.max(60);
        tokens.access_token = parsed.access_token;
        tokens.expires_at_epoch_secs = chrono::Utc::now().timestamp().saturating_add(ttl);
        if let Some(new_rt) = parsed.refresh_token.filter(|s| !s.trim().is_empty()) {
            tokens.refresh_token = new_rt;
        }
        Ok(true)
    }

    /// Direct OAuth access token refresh helper (no token broker).
    pub async fn refresh_access_token(
        &self,
        relay_enabled: bool,
        client_id: &str,
        client_secret: Option<&str>,
        tokens: &mut GDriveTokens,
    ) -> GDriveResult<bool> {
        self.refresh_access_token_with_broker(
            relay_enabled,
            client_id,
            client_secret,
            None,
            tokens,
        )
        .await
    }

    /// Fetches Google Drive storage quota and connected user profile via `GET /about`.
    pub async fn fetch_drive_quota(
        &self,
        relay_enabled: bool,
        access_token: &str,
    ) -> GDriveResult<(GDriveQuotaDto, Option<String>, Option<String>)> {
        ensure_relay_disabled(relay_enabled)?;

        let url = format!(
            "{}/about?fields=storageQuota,user",
            self.endpoints.drive_api_base
        );
        let body = self
            .send_get_text_with_retry(relay_enabled, access_token, &url)
            .await?;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawQuota {
            #[serde(default)]
            limit: Option<String>,
            #[serde(default)]
            usage: Option<String>,
            #[serde(default)]
            usage_in_drive: Option<String>,
            #[serde(default)]
            usage_in_drive_trash: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawUser {
            #[serde(default)]
            email_address: Option<String>,
            #[serde(default)]
            display_name: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AboutResp {
            #[serde(default)]
            storage_quota: Option<RawQuota>,
            #[serde(default)]
            user: Option<RawUser>,
        }

        let parsed: AboutResp = serde_json::from_str(&body)
            .map_err(|e| GDriveError::Api(format!("Invalid Drive about JSON: {e}")))?;
        let q = parsed.storage_quota.unwrap_or(RawQuota {
            limit: None,
            usage: None,
            usage_in_drive: None,
            usage_in_drive_trash: None,
        });
        let quota = GDriveQuotaDto {
            drive_limit_bytes: q.limit.and_then(|s| s.parse::<u64>().ok()),
            drive_usage_bytes: q.usage.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
            drive_usage_in_drive_bytes: q
                .usage_in_drive
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0),
            drive_usage_in_trash_bytes: q
                .usage_in_drive_trash
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0),
        };
        let email = parsed.user.as_ref().and_then(|u| u.email_address.clone());
        let name = parsed.user.as_ref().and_then(|u| u.display_name.clone());
        Ok((quota, email, name))
    }

    /// Finds a single non-trashed file or folder inside `parent_id` (use `"root"` for Drive root).
    pub async fn find_file_in_folder(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime_type: Option<&str>,
    ) -> GDriveResult<Option<DriveFileMeta>> {
        ensure_relay_disabled(relay_enabled)?;

        let esc_name = escape_drive_query_literal(name);
        let esc_parent = escape_drive_query_literal(parent_id);
        let q = match mime_type {
            Some(mt) => {
                let esc_mt = escape_drive_query_literal(mt);
                format!(
                    "name = '{esc_name}' and '{esc_parent}' in parents and mimeType = '{esc_mt}' and trashed = false"
                )
            }
            None => {
                format!("name = '{esc_name}' and '{esc_parent}' in parents and trashed = false")
            }
        };

        let mut url = Url::parse(&format!("{}/files", self.endpoints.drive_api_base))
            .map_err(|e| GDriveError::Other(format!("Invalid Drive files URL: {e}")))?;
        url.query_pairs_mut()
            .append_pair("q", &q)
            .append_pair("fields", "files(id,name,size,mimeType,modifiedTime)")
            .append_pair("pageSize", "1")
            .append_pair("spaces", "drive");

        let body = self
            .send_get_text_with_retry(relay_enabled, access_token, url.as_str())
            .await?;
        let files = parse_drive_file_list(&body)?.0;
        Ok(files.into_iter().next())
    }

    /// Lists all non-trashed files inside `folder_id`, following `nextPageToken` across all pages.
    pub async fn list_folder_files(
        &self,
        relay_enabled: bool,
        access_token: &str,
        folder_id: &str,
    ) -> GDriveResult<Vec<DriveFileMeta>> {
        ensure_relay_disabled(relay_enabled)?;

        let esc_parent = escape_drive_query_literal(folder_id);
        let q = format!("'{esc_parent}' in parents and trashed = false");
        let mut out = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            ensure_relay_disabled(relay_enabled)?;
            let mut url = Url::parse(&format!("{}/files", self.endpoints.drive_api_base))
                .map_err(|e| GDriveError::Other(format!("Invalid Drive files URL: {e}")))?;
            {
                let mut pairs = url.query_pairs_mut();
                pairs
                    .append_pair("q", &q)
                    .append_pair(
                        "fields",
                        "nextPageToken,files(id,name,size,mimeType,modifiedTime)",
                    )
                    .append_pair("pageSize", "1000")
                    .append_pair("orderBy", "createdTime asc")
                    .append_pair("spaces", "drive");
                if let Some(ref tok) = page_token {
                    pairs.append_pair("pageToken", tok);
                }
            }

            let body = self
                .send_get_text_with_retry(relay_enabled, access_token, url.as_str())
                .await?;
            let (batch, next_tok) = parse_drive_file_list(&body)?;
            out.extend(batch);
            match next_tok {
                Some(tok) if !tok.is_empty() => page_token = Some(tok),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Creates a Google Drive folder inside `parent_id` (`"root"` for Drive root).
    pub async fn create_folder(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
    ) -> GDriveResult<String> {
        ensure_relay_disabled(relay_enabled)?;

        let url = format!("{}/files?fields=id", self.endpoints.drive_api_base);
        let payload = serde_json::json!({
            "name": name,
            "mimeType": DRIVE_FOLDER_MIME,
            "parents": [parent_id],
        });
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| GDriveError::Other(format!("Failed encoding folder metadata: {e}")))?;

        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .post(&url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .header(CONTENT_TYPE, "application/json; charset=UTF-8")
                .body(payload_bytes.clone())
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive create_folder failed: {e}")))?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp
                .text()
                .await
                .map_err(|e| GDriveError::Api(format!("Failed reading create_folder body: {e}")))?;

            if status.is_success() {
                return parse_file_id(&body);
            }
            if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }
        Err(GDriveError::RateLimited)
    }

    /// Idempotently finds or creates `Teza Vault/` -> `README.txt` + `<uid>/` -> `blobs/`, `history/`
    /// and probes `manifest.enc.json`. Updates folder/manifest IDs on `tokens`.
    pub async fn ensure_vault_folders(
        &self,
        relay_enabled: bool,
        access_token: &str,
        uid: &str,
        tokens: &mut GDriveTokens,
    ) -> GDriveResult<VaultFolderIds> {
        ensure_relay_disabled(relay_enabled)?;

        let root_folder_id = match self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                "root",
                DRIVE_ROOT_FOLDER_NAME,
                Some(DRIVE_FOLDER_MIME),
            )
            .await?
        {
            Some(f) => f.id,
            None => {
                self.create_folder(relay_enabled, access_token, "root", DRIVE_ROOT_FOLDER_NAME)
                    .await?
            }
        };

        // Best-effort README.txt inside Teza Vault/ — never fail sign-in or folder setup
        // (e.g. if the user's Google Drive is currently at 100% storage quota).
        if let Ok(None) = self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                &root_folder_id,
                DRIVE_README_FILE_NAME,
                None,
            )
            .await
        {
            let _ = self
                .upload_small_bytes(
                    relay_enabled,
                    access_token,
                    &root_folder_id,
                    DRIVE_README_FILE_NAME,
                    "text/plain; charset=utf-8",
                    DRIVE_README_TEXT.as_bytes(),
                )
                .await;
        }

        let uid_seg = sanitize_uid_folder_name(uid);
        let uid_folder_id = match self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                &root_folder_id,
                &uid_seg,
                Some(DRIVE_FOLDER_MIME),
            )
            .await?
        {
            Some(f) => f.id,
            None => {
                self.create_folder(relay_enabled, access_token, &root_folder_id, &uid_seg)
                    .await?
            }
        };

        let blobs_folder_id = match self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                &uid_folder_id,
                DRIVE_BLOBS_FOLDER_NAME,
                Some(DRIVE_FOLDER_MIME),
            )
            .await?
        {
            Some(f) => f.id,
            None => {
                self.create_folder(
                    relay_enabled,
                    access_token,
                    &uid_folder_id,
                    DRIVE_BLOBS_FOLDER_NAME,
                )
                .await?
            }
        };

        let history_folder_id = match self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                &uid_folder_id,
                DRIVE_HISTORY_FOLDER_NAME,
                Some(DRIVE_FOLDER_MIME),
            )
            .await?
        {
            Some(f) => f.id,
            None => {
                self.create_folder(
                    relay_enabled,
                    access_token,
                    &uid_folder_id,
                    DRIVE_HISTORY_FOLDER_NAME,
                )
                .await?
            }
        };

        let manifest_file_id = self
            .find_file_in_folder(
                relay_enabled,
                access_token,
                &uid_folder_id,
                DRIVE_MANIFEST_FILE_NAME,
                None,
            )
            .await?
            .map(|f| f.id);

        tokens.root_folder_id = Some(root_folder_id.clone());
        tokens.uid_folder_id = Some(uid_folder_id.clone());
        tokens.blobs_folder_id = Some(blobs_folder_id.clone());
        tokens.history_folder_id = Some(history_folder_id.clone());
        tokens.manifest_file_id = manifest_file_id.clone();

        Ok(VaultFolderIds {
            root_folder_id,
            uid_folder_id,
            blobs_folder_id,
            history_folder_id,
            manifest_file_id,
        })
    }

    /// Unlocks an existing remote `manifest.enc.json` or initializes & uploads a new vault manifest
    /// on first device. Returns `(CloudMasterKey, created_new)`.
    pub async fn unlock_or_init_vault(
        &self,
        relay_enabled: bool,
        access_token: &str,
        uid: &str,
        passphrase: &str,
        tokens: &mut GDriveTokens,
    ) -> GDriveResult<(CloudMasterKey, bool)> {
        ensure_relay_disabled(relay_enabled)?;
        if passphrase.chars().count() < MIN_VAULT_PASSPHRASE_LEN {
            return Err(GDriveError::InvalidPassphrase);
        }

        let folders = self
            .ensure_vault_folders(relay_enabled, access_token, uid, tokens)
            .await?;

        if let Some(manifest_id) = folders.manifest_file_id {
            let raw = self
                .download_file_bytes(relay_enabled, access_token, &manifest_id)
                .await?;
            let manifest: VaultManifest =
                serde_json::from_slice(&raw).map_err(|_| GDriveError::InvalidPassphrase)?;
            let master_key = unwrap_master_key_manifest(uid, passphrase, &manifest).await?;
            return Ok((master_key, false));
        }

        let (manifest, master_key) = create_vault_manifest(uid, passphrase).await?;
        let manifest_json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| GDriveError::Other(format!("Failed serializing vault manifest: {e}")))?;
        let file_id = self
            .upload_small_bytes(
                relay_enabled,
                access_token,
                &folders.uid_folder_id,
                DRIVE_MANIFEST_FILE_NAME,
                "application/json",
                &manifest_json,
            )
            .await?;
        tokens.manifest_file_id = Some(file_id);
        Ok((master_key, true))
    }

    /// Uploads a small in-memory payload (such as `README.txt`, `manifest.enc.json`, or a delta)
    /// using Drive multipart/related upload (or falls back to resumable bytes if > 4 MiB).
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_small_bytes(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
    ) -> GDriveResult<String> {
        ensure_relay_disabled(relay_enabled)?;

        if data.len() > SMALL_UPLOAD_LIMIT {
            return self
                .upload_bytes_resumable(relay_enabled, access_token, parent_id, name, mime, data)
                .await;
        }

        let mut rnd = [0u8; 16];
        getrandom::fill(&mut rnd)
            .map_err(|e| GDriveError::Other(format!("OS CSPRNG failure: {e}")))?;
        let boundary = format!("teza_mp_{}", encode_hex_lower(&rnd));

        let metadata_json = serde_json::to_string(&serde_json::json!({
            "name": name,
            "parents": [parent_id],
        }))
        .map_err(|e| GDriveError::Other(format!("Failed serializing metadata: {e}")))?;

        let mut body = Vec::with_capacity(metadata_json.len() + data.len() + 256);
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
        body.extend_from_slice(metadata_json.as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
        body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let url = format!(
            "{}/files?uploadType=multipart&fields=id,size",
            self.endpoints.drive_upload_base
        );
        let ct_hdr = format!("multipart/related; boundary={boundary}");

        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .post(&url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .header(CONTENT_TYPE, &ct_hdr)
                .body(body.clone())
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive multipart upload failed: {e}")))?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let resp_text = resp.text().await.map_err(|e| {
                GDriveError::Api(format!("Failed reading multipart upload body: {e}"))
            })?;

            if status.is_success() {
                return parse_file_id(&resp_text);
            }
            if is_retryable_drive_error(status, &resp_text) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &resp_text));
        }
        Err(GDriveError::RateLimited)
    }

    async fn initiate_resumable_session(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime: &str,
        total_len: u64,
    ) -> GDriveResult<String> {
        let init_url = format!(
            "{}/files?uploadType=resumable&fields=id,size",
            self.endpoints.drive_upload_base
        );
        let meta_bytes = serde_json::to_vec(&serde_json::json!({
            "name": name,
            "parents": [parent_id],
        }))
        .map_err(|e| GDriveError::Other(format!("Failed encoding upload metadata: {e}")))?;

        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .post(&init_url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .header(CONTENT_TYPE, "application/json; charset=UTF-8")
                .header("X-Upload-Content-Type", mime)
                .header("X-Upload-Content-Length", total_len.to_string())
                .body(meta_bytes.clone())
                .send()
                .await
                .map_err(|e| {
                    GDriveError::Api(format!("Drive resumable session initiation failed: {e}"))
                })?;
            let status = resp.status();
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();

            if status.is_success() {
                if let Some(loc) = location.filter(|s| !s.trim().is_empty()) {
                    return Ok(loc);
                }
                return Err(GDriveError::Api(
                    "Drive resumable init response missing Location header".into(),
                ));
            }
            if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }
        Err(GDriveError::RateLimited)
    }

    async fn upload_bytes_resumable(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime: &str,
        data: &[u8],
    ) -> GDriveResult<String> {
        let total_len = data.len() as u64;
        let session_uri = self
            .initiate_resumable_session(
                relay_enabled,
                access_token,
                parent_id,
                name,
                mime,
                total_len,
            )
            .await?;

        let mut offset: usize = 0;
        let mut retries: usize = 0;
        while offset < data.len() {
            ensure_relay_disabled(relay_enabled)?;
            let end_excl = (offset + RESUMABLE_CHUNK_SIZE).min(data.len());
            let chunk = data[offset..end_excl].to_vec();
            let end_incl = end_excl - 1;
            let content_range = format!("bytes {offset}-{end_incl}/{total_len}");

            let resp = self
                .http
                .put(&session_uri)
                .header(CONTENT_TYPE, mime)
                .header(CONTENT_RANGE, &content_range)
                .body(chunk)
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive chunk PUT failed: {e}")))?;
            let status = resp.status();
            let range_hdr = resp
                .headers()
                .get(RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();

            if status == StatusCode::OK || status == StatusCode::CREATED {
                return parse_file_id(&body);
            }
            if status == StatusCode::PERMANENT_REDIRECT {
                let next = range_hdr
                    .as_deref()
                    .and_then(parse_drive_range_next_offset)
                    .map(|v| v as usize)
                    .unwrap_or(end_excl);
                offset = next.min(data.len());
                retries = 0;
                continue;
            }
            if is_retryable_drive_error(status, &body) && retries < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(retries, retry_after.as_deref())).await;
                retries += 1;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }

        Err(GDriveError::Api(
            "Drive resumable upload ended without completion response".into(),
        ))
    }

    /// Streams any-size file to Google Drive using O(1) RAM 4 MiB chunked resumable `PUT`s
    /// (exact multiple of 256 KiB, zero per-file size cap). Returns the created Drive `file_id`.
    #[allow(clippy::too_many_arguments)]
    pub async fn upload_file_resumable(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime: &str,
        file_path: &Path,
    ) -> GDriveResult<String> {
        self.upload_file_resumable_chunked(
            relay_enabled,
            access_token,
            parent_id,
            name,
            mime,
            file_path,
            RESUMABLE_CHUNK_SIZE,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_file_resumable_chunked(
        &self,
        relay_enabled: bool,
        access_token: &str,
        parent_id: &str,
        name: &str,
        mime: &str,
        file_path: &Path,
        chunk_size: usize,
    ) -> GDriveResult<String> {
        ensure_relay_disabled(relay_enabled)?;

        let meta = tokio::fs::metadata(file_path).await.map_err(|e| {
            GDriveError::Other(format!(
                "Failed to stat upload file {}: {e}",
                file_path.display()
            ))
        })?;
        let total_len = meta.len();

        let session_uri = self
            .initiate_resumable_session(
                relay_enabled,
                access_token,
                parent_id,
                name,
                mime,
                total_len,
            )
            .await?;

        if total_len == 0 {
            for attempt in 0..=MAX_API_RETRIES {
                ensure_relay_disabled(relay_enabled)?;
                let resp = self
                    .http
                    .put(&session_uri)
                    .header(CONTENT_TYPE, mime)
                    .header(CONTENT_LENGTH, "0")
                    .body(Vec::new())
                    .send()
                    .await
                    .map_err(|e| GDriveError::Api(format!("Drive empty upload PUT failed: {e}")))?;
                let status = resp.status();
                let retry_after = resp
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body = resp.text().await.unwrap_or_default();
                if status == StatusCode::OK || status == StatusCode::CREATED {
                    return parse_file_id(&body);
                }
                if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                    tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                    continue;
                }
                return Err(map_drive_http_error(status, &body));
            }
            return Err(GDriveError::RateLimited);
        }

        let mut file = tokio::fs::File::open(file_path).await.map_err(|e| {
            GDriveError::Other(format!(
                "Failed to open upload file {}: {e}",
                file_path.display()
            ))
        })?;
        let mut buf = vec![0u8; chunk_size.max(256 * 1024)];
        let mut offset: u64 = 0;
        let mut retries: usize = 0;

        while offset < total_len {
            ensure_relay_disabled(relay_enabled)?;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| GDriveError::Other(format!("Failed seeking upload file: {e}")))?;
            let remaining = (total_len - offset) as usize;
            let to_read = remaining.min(buf.len());
            let n = read_chunk_full(&mut file, &mut buf[..to_read])
                .await
                .map_err(|e| GDriveError::Other(format!("Failed reading upload chunk: {e}")))?;
            if n == 0 {
                return Err(GDriveError::Other(
                    "Unexpected EOF while streaming file for Drive resumable upload".into(),
                ));
            }
            let end_incl = offset + (n as u64) - 1;
            let content_range = format!("bytes {offset}-{end_incl}/{total_len}");

            let resp = self
                .http
                .put(&session_uri)
                .header(CONTENT_TYPE, mime)
                .header(CONTENT_RANGE, &content_range)
                .body(buf[..n].to_vec())
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive resumable chunk PUT failed: {e}")))?;
            let status = resp.status();
            let range_hdr = resp
                .headers()
                .get(RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();

            if status == StatusCode::OK || status == StatusCode::CREATED {
                return parse_file_id(&body);
            }
            if status == StatusCode::PERMANENT_REDIRECT {
                let next = range_hdr
                    .as_deref()
                    .and_then(parse_drive_range_next_offset)
                    .unwrap_or(end_incl + 1);
                offset = next.min(total_len);
                retries = 0;
                continue;
            }
            if is_retryable_drive_error(status, &body) && retries < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(retries, retry_after.as_deref())).await;
                retries += 1;
                // Query server upload progress so we resume at the exact committed byte offset.
                if let Ok(Some(acked_next)) = self
                    .query_resumable_offset(relay_enabled, &session_uri, total_len)
                    .await
                {
                    offset = acked_next.min(total_len);
                }
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }

        Err(GDriveError::Api(
            "Drive resumable upload finished chunks without completion ID".into(),
        ))
    }

    async fn query_resumable_offset(
        &self,
        relay_enabled: bool,
        session_uri: &str,
        total_len: u64,
    ) -> GDriveResult<Option<u64>> {
        ensure_relay_disabled(relay_enabled)?;
        let resp = self
            .http
            .put(session_uri)
            .header(CONTENT_LENGTH, "0")
            .header(CONTENT_RANGE, format!("bytes */{total_len}"))
            .body(Vec::new())
            .send()
            .await
            .map_err(|e| GDriveError::Api(format!("Drive resumable offset query failed: {e}")))?;
        if resp.status() == StatusCode::PERMANENT_REDIRECT {
            let next = resp
                .headers()
                .get(RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_drive_range_next_offset)
                .unwrap_or(0);
            return Ok(Some(next));
        }
        Ok(None)
    }

    /// Streams a Google Drive file directly to `dst_path` in O(1) RAM.
    /// Maps HTTP 404 to `GDriveError::NotFound` and deletes partial `dst_path` on failure.
    pub async fn download_file_to_path(
        &self,
        relay_enabled: bool,
        access_token: &str,
        file_id: &str,
        dst_path: &Path,
    ) -> GDriveResult<u64> {
        ensure_relay_disabled(relay_enabled)?;

        let url = format!(
            "{}/files/{}?alt=media",
            self.endpoints.drive_api_base,
            encode_path_segment(file_id)
        );
        if let Some(parent) = dst_path.parent() {
            data_dir::ensure_private_dir(parent).map_err(|e| {
                GDriveError::Other(format!("Failed creating download directory: {e}"))
            })?;
        }

        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let mut resp = self
                .http
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive download request failed: {e}")))?;
            let status = resp.status();
            if status == StatusCode::NOT_FOUND {
                return Err(GDriveError::NotFound);
            }
            if !status.is_success() {
                let retry_after = resp
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body = resp.text().await.unwrap_or_default();
                if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                    tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                    continue;
                }
                return Err(map_drive_http_error(status, &body));
            }

            let mut file = tokio::fs::File::create(dst_path).await.map_err(|e| {
                GDriveError::Other(format!(
                    "Failed creating download destination {}: {e}",
                    dst_path.display()
                ))
            })?;
            let mut total: u64 = 0;
            let stream_res: GDriveResult<()> = async {
                while let Some(chunk) = resp
                    .chunk()
                    .await
                    .map_err(|e| GDriveError::Api(format!("Drive download stream error: {e}")))?
                {
                    file.write_all(&chunk).await.map_err(|e| {
                        GDriveError::Other(format!("Failed writing download chunk: {e}"))
                    })?;
                    total = total.saturating_add(chunk.len() as u64);
                }
                file.flush()
                    .await
                    .map_err(|e| GDriveError::Other(format!("Failed flushing download: {e}")))?;
                file.sync_all()
                    .await
                    .map_err(|e| GDriveError::Other(format!("Failed syncing download: {e}")))?;
                Ok(())
            }
            .await;

            if let Err(err) = stream_res {
                let _ = tokio::fs::remove_file(dst_path).await;
                return Err(err);
            }
            return Ok(total);
        }
        Err(GDriveError::RateLimited)
    }

    /// Downloads a small Drive file (e.g., `manifest.enc.json` or `.delta.enc`) into memory.
    pub async fn download_file_bytes(
        &self,
        relay_enabled: bool,
        access_token: &str,
        file_id: &str,
    ) -> GDriveResult<Vec<u8>> {
        ensure_relay_disabled(relay_enabled)?;

        let url = format!(
            "{}/files/{}?alt=media",
            self.endpoints.drive_api_base,
            encode_path_segment(file_id)
        );
        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .get(&url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive download failed: {e}")))?;
            let status = resp.status();
            if status == StatusCode::NOT_FOUND {
                return Err(GDriveError::NotFound);
            }
            if status.is_success() {
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| GDriveError::Api(format!("Failed reading Drive bytes: {e}")))?;
                return Ok(bytes.to_vec());
            }
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();
            if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }
        Err(GDriveError::RateLimited)
    }

    /// Permanently deletes a Google Drive file. Treats `404 Not Found` as already deleted (`Ok(())`).
    pub async fn delete_drive_file(
        &self,
        relay_enabled: bool,
        access_token: &str,
        file_id: &str,
    ) -> GDriveResult<()> {
        ensure_relay_disabled(relay_enabled)?;

        let url = format!(
            "{}/files/{}",
            self.endpoints.drive_api_base,
            encode_path_segment(file_id)
        );
        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .delete(&url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive delete request failed: {e}")))?;
            let status = resp.status();
            if status.is_success() || status == StatusCode::NOT_FOUND {
                return Ok(());
            }
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.text().await.unwrap_or_default();
            if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }
        Err(GDriveError::RateLimited)
    }

    /// Pulls unseen `.delta.enc` history files from Drive `history/`, decrypts & merges them
    /// idempotently into `store`, then exports & uploads any local deltas since `last_history_sync_ts`
    /// in bounded pages without ever jumping past truncated rows or local wall-clock skew.
    /// Never holds the `MessageStore` mutex across network or crypto I/O.
    #[allow(clippy::too_many_arguments)]
    pub async fn sync_history_deltas(
        &self,
        relay_enabled: bool,
        access_token: &str,
        history_folder_id: &str,
        store: &Arc<AsyncMutex<MessageStore>>,
        master_key: &CloudMasterKey,
        uid: &str,
        device_id: &str,
        last_history_sync_ts: i64,
    ) -> GDriveResult<HistorySyncReport> {
        self.sync_history_deltas_with_limit(
            relay_enabled,
            access_token,
            history_folder_id,
            store,
            master_key,
            uid,
            device_id,
            last_history_sync_ts,
            HISTORY_DELTA_MSG_LIMIT,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn sync_history_deltas_with_limit(
        &self,
        relay_enabled: bool,
        access_token: &str,
        history_folder_id: &str,
        store: &Arc<AsyncMutex<MessageStore>>,
        master_key: &CloudMasterKey,
        uid: &str,
        device_id: &str,
        last_history_sync_ts: i64,
        limit_msgs: usize,
    ) -> GDriveResult<HistorySyncReport> {
        ensure_relay_disabled(relay_enabled)?;

        let remote_files = self
            .list_folder_files(relay_enabled, access_token, history_folder_id)
            .await?;

        let mut report = HistorySyncReport {
            downloaded_deltas: 0,
            applied_messages: 0,
            uploaded_delta_id: None,
            new_last_history_sync_ts: last_history_sync_ts,
        };

        for file in remote_files {
            if !file.name.ends_with(".delta.enc") {
                continue;
            }
            let already_applied = {
                let guard = store.lock().await;
                guard
                    .is_delta_applied(&file.id)
                    .map_err(|e| GDriveError::Other(format!("SQLite delta check failed: {e}")))?
            };
            if already_applied {
                continue;
            }

            let enc_bytes = self
                .download_file_bytes(relay_enabled, access_token, &file.id)
                .await?;
            let batch = decrypt_history_delta(&enc_bytes, master_key, uid)?;
            if batch.uid != uid {
                return Err(GDriveError::IntegrityMismatch);
            }

            let now_ms = chrono::Utc::now().timestamp_millis();
            let applied = {
                let guard = store.lock().await;
                let count = guard.import_history_delta_idempotent(&batch).map_err(|e| {
                    GDriveError::Other(format!("Failed importing history delta into SQLite: {e}"))
                })?;
                guard.mark_delta_applied(&file.id, now_ms).map_err(|e| {
                    GDriveError::Other(format!("Failed marking delta applied in SQLite: {e}"))
                })?;
                count
            };
            report.downloaded_deltas += 1;
            report.applied_messages += applied;
        }

        // Export local changes since `cursor_ts` in bounded pages so DBs with >2,000 messages
        // or PC clock skew ahead of server timestamps never drop rows.
        let mut cursor_ts = last_history_sync_ts;
        let safe_dev = sanitize_uid_folder_name(device_id);
        for _page in 0..MAX_HISTORY_UPLOAD_PAGES_PER_CYCLE {
            ensure_relay_disabled(relay_enabled)?;
            let local_batch = {
                let guard = store.lock().await;
                guard
                    .export_history_delta_since(uid, device_id, cursor_ts, limit_msgs)
                    .map_err(|e| {
                        GDriveError::Other(format!("Failed exporting history delta: {e}"))
                    })?
            };
            let has_changes = !local_batch.messages.is_empty()
                || !local_batch.threads.is_empty()
                || !local_batch.cloud_media.is_empty()
                || (cursor_ts <= 0 && !local_batch.folders.is_empty());
            if !has_changes {
                break;
            }

            let from_ts = local_batch
                .messages
                .first()
                .map(|m| m.ts)
                .unwrap_or(cursor_ts.max(0));
            let (to_ts, next_watermark) =
                compute_history_batch_watermark(cursor_ts, &local_batch);

            let encrypted = encrypt_history_delta(&local_batch, master_key, uid)?;
            let file_name = format!("{safe_dev}-{from_ts}-{to_ts}.delta.enc");
            let uploaded_id = self
                .upload_small_bytes(
                    relay_enabled,
                    access_token,
                    history_folder_id,
                    &file_name,
                    "application/octet-stream",
                    &encrypted,
                )
                .await?;

            let now_ms = chrono::Utc::now().timestamp_millis();
            {
                let guard = store.lock().await;
                guard
                    .mark_delta_applied(&uploaded_id, now_ms)
                    .map_err(|e| GDriveError::Other(format!("SQLite mark_delta_applied: {e}")))?;
                guard
                    .set_gdrive_last_history_sync_ts(next_watermark)
                    .map_err(|e| GDriveError::Other(format!("SQLite history watermark: {e}")))?;
            }
            report.uploaded_delta_id = Some(uploaded_id);
            report.new_last_history_sync_ts = next_watermark;
            cursor_ts = next_watermark;

            if !local_batch.messages_truncated && !local_batch.cloud_media_truncated {
                break;
            }
        }

        Ok(report)
    }

    async fn send_get_text_with_retry(
        &self,
        relay_enabled: bool,
        access_token: &str,
        url: &str,
    ) -> GDriveResult<String> {
        for attempt in 0..=MAX_API_RETRIES {
            ensure_relay_disabled(relay_enabled)?;
            let resp = self
                .http
                .get(url)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .send()
                .await
                .map_err(|e| GDriveError::Api(format!("Drive GET request failed: {e}")))?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp
                .text()
                .await
                .map_err(|e| GDriveError::Api(format!("Failed reading Drive GET body: {e}")))?;

            if status.is_success() {
                return Ok(body);
            }
            if is_retryable_drive_error(status, &body) && attempt < MAX_API_RETRIES {
                tokio::time::sleep(compute_retry_delay(attempt, retry_after.as_deref())).await;
                continue;
            }
            return Err(map_drive_http_error(status, &body));
        }
        Err(GDriveError::RateLimited)
    }
}

fn compute_history_batch_watermark(cursor_ts: i64, batch: &HistoryDeltaBatch) -> (i64, i64) {
    let msg_cap = if batch.messages_truncated {
        batch.max_message_ts()
    } else {
        None
    };
    let cm_cap = if batch.cloud_media_truncated {
        batch.max_cloud_media_cursor_ts
    } else {
        None
    };

    if msg_cap.is_some() || cm_cap.is_some() {
        let cap_ts = match (msg_cap, cm_cap) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => unreachable!(),
        };
        let next = cap_ts.saturating_add(1).max(cursor_ts.saturating_add(1));
        return (cap_ts, next);
    }

    let max_msg_ts = batch.max_message_ts().unwrap_or(0);
    let max_thread_ts = batch
        .threads
        .iter()
        .map(|t| t.last_ts.max(t.last_read_in_ts).max(t.peer_read_ts))
        .max()
        .unwrap_or(0);
    let max_media_ts = batch
        .max_cloud_media_cursor_ts
        .or_else(|| batch.cloud_media.iter().map(|c| c.cloud_synced_ts).max())
        .unwrap_or(0);
    let max_db_ts = max_msg_ts.max(max_thread_ts).max(max_media_ts);

    if max_db_ts > 0 {
        let next = max_db_ts.saturating_add(1).max(cursor_ts);
        (max_db_ts, next)
    } else {
        // Folders-only initial batch (0 non-zero DB timestamps): never advance to PC wall-clock.
        let next = cursor_ts.max(1);
        (next, next)
    }
}

fn truncate_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 240 {
        trimmed.to_string()
    } else {
        let mut end = 240;
        while !trimmed.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &trimmed[..end])
    }
}

fn map_drive_http_error(status: StatusCode, body: &str) -> GDriveError {
    if status == StatusCode::NOT_FOUND {
        return GDriveError::NotFound;
    }
    if status == StatusCode::FORBIDDEN && body.to_ascii_lowercase().contains("storagequotaexceeded")
    {
        return GDriveError::StorageQuotaExceeded;
    }
    if is_retryable_drive_error(status, body) {
        return GDriveError::RateLimited;
    }
    GDriveError::Api(format!(
        "HTTP {}: {}",
        status.as_u16(),
        truncate_error_body(body)
    ))
}

fn parse_file_id(body: &str) -> GDriveResult<String> {
    #[derive(Deserialize)]
    struct IdResp {
        id: String,
    }
    let parsed: IdResp = serde_json::from_str(body)
        .map_err(|e| GDriveError::Api(format!("Drive response missing file id: {e}")))?;
    if parsed.id.trim().is_empty() {
        return Err(GDriveError::Api("Drive returned empty file id".into()));
    }
    Ok(parsed.id)
}

fn parse_drive_file_list(body: &str) -> GDriveResult<(Vec<DriveFileMeta>, Option<String>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawFile {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        size: Option<String>,
        #[serde(default)]
        mime_type: String,
        #[serde(default)]
        modified_time: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ListResp {
        #[serde(default)]
        files: Vec<RawFile>,
        #[serde(default)]
        next_page_token: Option<String>,
    }
    let parsed: ListResp = serde_json::from_str(body)
        .map_err(|e| GDriveError::Api(format!("Invalid Drive file list JSON: {e}")))?;
    let files = parsed
        .files
        .into_iter()
        .map(|f| DriveFileMeta {
            id: f.id,
            name: f.name,
            size: f.size.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
            mime_type: f.mime_type,
            modified_time: f.modified_time,
        })
        .collect();
    Ok((files, parsed.next_page_token))
}

fn encode_path_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatFolderDto, FolderFilter, MessageDto, ThreadDto, ThreadKind};
    use crate::data_dir::StoragePaths;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn temp_test_dir(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "teza-gdrive-test-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn strict_relay_guard_blocks_all_network_paths() {
        let client = GDriveClient::new().unwrap();
        let dir = temp_test_dir("relay-guard");
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(AsyncMutex::new(
            MessageStore::open(&paths.history_db(Some("u1"))).unwrap(),
        ));
        let master = CloudMasterKey::generate().unwrap();
        let mut tokens = GDriveTokens {
            access_token: "tok".into(),
            refresh_token: "ref".into(),
            expires_at_epoch_secs: 0,
            account_email: None,
            account_name: None,
            root_folder_id: None,
            uid_folder_id: None,
            blobs_folder_id: None,
            history_folder_id: None,
            manifest_file_id: None,
        };
        let dummy_file = dir.join("dummy.bin");
        std::fs::write(&dummy_file, b"abc").unwrap();
        let dl_file = dir.join("dl.bin");

        assert!(matches!(
            OAuthLoopbackFlow::bind(true, "cid").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .exchange_oauth_code(true, "cid", None, "code", "ver", "http://127.0.0.1/cb")
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .refresh_access_token(true, "cid", None, &mut tokens)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client.fetch_drive_quota(true, "tok").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .find_file_in_folder(true, "tok", "root", "Teza Vault", None)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client.list_folder_files(true, "tok", "folder1").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client.create_folder(true, "tok", "root", "Teza Vault").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .ensure_vault_folders(true, "tok", "u1", &mut tokens)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .unlock_or_init_vault(true, "tok", "u1", "strongpassphrase", &mut tokens)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .upload_small_bytes(true, "tok", "p1", "f.txt", "text/plain", b"hi")
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .upload_file_resumable(true, "tok", "p1", "f.enc", "application/octet-stream", &dummy_file)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .download_file_to_path(true, "tok", "file1", &dl_file)
                .await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client.download_file_bytes(true, "tok", "file1").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client.delete_drive_file(true, "tok", "file1").await,
            Err(GDriveError::RelayBlocked)
        ));
        assert!(matches!(
            client
                .sync_history_deltas(true, "tok", "hist1", &store, &master, "u1", "dev1", 0)
                .await,
            Err(GDriveError::RelayBlocked)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pbkdf2_600k_manifest_wrap_unwrap_and_tamper_rejection() {
        let (manifest, master) = create_vault_manifest("uid-999", "MySuperSecretPass!2026")
            .await
            .unwrap();
        assert_eq!(manifest.version, VAULT_MANIFEST_VERSION);
        assert_eq!(manifest.kdf, VAULT_MANIFEST_KDF_NAME);
        assert_eq!(manifest.kdf_iterations, 600_000);

        // Correct passphrase + uid succeeds
        let restored = unwrap_master_key_manifest("uid-999", "MySuperSecretPass!2026", &manifest)
            .await
            .unwrap();
        assert_eq!(restored.as_bytes(), master.as_bytes());

        // Short passphrase rejected on wrap
        assert!(matches!(
            create_vault_manifest("uid-999", "short").await,
            Err(GDriveError::InvalidPassphrase)
        ));

        // Wrong passphrase rejected
        assert!(matches!(
            unwrap_master_key_manifest("uid-999", "WrongPassphrase!2026", &manifest).await,
            Err(GDriveError::InvalidPassphrase)
        ));

        // Wrong UID (AAD mismatch) rejected
        assert!(matches!(
            unwrap_master_key_manifest("uid-other", "MySuperSecretPass!2026", &manifest).await,
            Err(GDriveError::InvalidPassphrase)
        ));

        // Tampered ciphertext rejected
        let mut tampered = manifest.clone();
        let mut raw_ct = B64.decode(&tampered.wrapped_key_b64).unwrap();
        raw_ct[0] ^= 0x55;
        tampered.wrapped_key_b64 = B64.encode(raw_ct);
        assert!(matches!(
            unwrap_master_key_manifest("uid-999", "MySuperSecretPass!2026", &tampered).await,
            Err(GDriveError::InvalidPassphrase)
        ));
    }

    #[test]
    fn encrypted_gzip_history_delta_roundtrip_and_idempotent_merge_into_fresh_db() {
        let dir = temp_test_dir("delta-e2ee");
        let mut src = MessageStore::open(&dir.join("src.db")).unwrap();

        src.upsert_thread(&ThreadDto {
            id: "t-eng".into(),
            title: "Engineering Team".into(),
            avatar: String::new(),
            kind: ThreadKind::Group,
            last_preview: "Shipped Phase 2".into(),
            last_ts: 1_700_000_500_000,
            unread_count: 0,
            is_pinned: true,
            is_muted: false,
            last_read_in_ts: 1_700_000_500_000,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        })
        .unwrap();
        src.save_chat_folder(&ChatFolderDto {
            id: "f-work".into(),
            name: "Work".into(),
            icon: "briefcase".into(),
            color: "#10b981".into(),
            thread_ids: vec!["t-eng".into()],
            excluded_thread_ids: Vec::new(),
            include_kinds: Vec::new(),
            only_unread: false,
            exclude_muted: false,
            position: 1,
            remote_label_id: None,
        })
        .unwrap();
        src.upsert_message(&MessageDto {
            msg_id: "m-101".into(),
            thread_id: "t-eng".into(),
            sender_id: "u-alice".into(),
            sender_name: "Alice".into(),
            text: "Encrypted history backup payload with Vietnamese: Chào buổi sáng!".into(),
            msg_type: "chat.text".into(),
            ts: 1_700_000_500_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: true,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        })
        .unwrap();

        let batch = src
            .export_history_delta_since("u-alice", "dev-1", 0, 500)
            .unwrap();
        let master = CloudMasterKey::generate().unwrap();
        let encrypted = encrypt_history_delta(&batch, &master, "u-alice").unwrap();
        assert_eq!(encrypted[0], HISTORY_DELTA_WIRE_VERSION);
        assert!(
            !String::from_utf8_lossy(&encrypted).contains("Shipped Phase 2"),
            "ciphertext must not leak plaintext"
        );

        // Wrong key or wrong UID or tampered byte rejected
        let wrong_key = CloudMasterKey::generate().unwrap();
        assert!(matches!(
            decrypt_history_delta(&encrypted, &wrong_key, "u-alice"),
            Err(GDriveError::IntegrityMismatch)
        ));
        assert!(matches!(
            decrypt_history_delta(&encrypted, &master, "u-bob"),
            Err(GDriveError::IntegrityMismatch)
        ));
        let mut tampered = encrypted.clone();
        *tampered.last_mut().unwrap() ^= 0x01;
        assert!(matches!(
            decrypt_history_delta(&tampered, &master, "u-alice"),
            Err(GDriveError::IntegrityMismatch)
        ));

        // Decrypt and merge into a fresh SQLite DB
        let decrypted = decrypt_history_delta(&encrypted, &master, "u-alice").unwrap();
        let dst = MessageStore::open(&dir.join("dst.db")).unwrap();
        assert_eq!(dst.import_history_delta_idempotent(&decrypted).unwrap(), 1);
        assert_eq!(dst.list_threads(FolderFilter::All).unwrap().len(), 1);
        assert_eq!(dst.list_chat_folders().unwrap().len(), 1);
        assert_eq!(
            dst.get_message("t-eng", "m-101").unwrap().unwrap().text,
            "Encrypted history backup payload with Vietnamese: Chào buổi sáng!"
        );
        // Re-applying identical delta is a clean no-op
        assert_eq!(dst.import_history_delta_idempotent(&decrypted).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_redact_debug_dpapi_roundtrip_escape_query_and_pkce_rfc7636_vector() {
        // RFC 7636 Appendix B test vector
        let rfc_verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(
            compute_pkce_s256_challenge(rfc_verifier),
            expected_challenge
        );

        // Drive query escaping prevents injection via quotes, backslashes, or newlines
        assert_eq!(
            escape_drive_query_literal("O'Brien\\Vault\r\n' or '1'='1"),
            "O\\'Brien\\\\Vault\\' or \\'1\\'=\\'1"
        );

        // Authorize URL strictly requests ONLY drive.file scope
        let auth_url = build_oauth_authorize_url(
            "my-client.apps.googleusercontent.com",
            "http://127.0.0.1:45678/callback",
            expected_challenge,
            "state-xyz",
        )
        .unwrap();
        assert!(auth_url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file"));
        assert!(auth_url.contains("code_challenge_method=S256"));
        assert!(auth_url.contains("access_type=offline"));

        // Redacted Debug for tokens, master key, and stored config
        let tokens = GDriveTokens {
            access_token: "ya29.super_secret_access".into(),
            refresh_token: "1//super_secret_refresh".into(),
            expires_at_epoch_secs: 1_900_000_000,
            account_email: Some("user@example.com".into()),
            account_name: Some("Teza User".into()),
            root_folder_id: Some("root-id".into()),
            uid_folder_id: None,
            blobs_folder_id: None,
            history_folder_id: None,
            manifest_file_id: None,
        };
        let tok_dbg = format!("{tokens:?}");
        assert!(tok_dbg.contains("[REDACTED]"));
        assert!(!tok_dbg.contains("ya29.super_secret_access"));
        assert!(!tok_dbg.contains("1//super_secret_refresh"));

        let master = CloudMasterKey::from_bytes([0xAB; 32]);
        let master_dbg = format!("{master:?}");
        assert_eq!(master_dbg, "CloudMasterKey([REDACTED])");
        assert!(!master_dbg.contains("abab"));

        let dir = temp_test_dir("dpapi-io");
        let cfg_path = dir.join("gdrive_config.json");
        let mut cfg = GDriveStoredConfig {
            client_id: "client-123.apps.googleusercontent.com".into(),
            ..Default::default()
        };
        cfg.set_client_secret(Some("GOCSPX-super-secret-value"))
            .unwrap();
        let cfg_dbg = format!("{cfg:?}");
        assert!(!cfg_dbg.contains("GOCSPX"));
        cfg.save_to_file(&cfg_path).unwrap();
        let raw_json = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!raw_json.contains("GOCSPX-super-secret-value"));

        let loaded_cfg = GDriveStoredConfig::load_from_file(&cfg_path);
        let (cid, csec) = loaded_cfg.resolve_oauth_credentials().unwrap();
        assert_eq!(cid, "client-123.apps.googleusercontent.com");
        assert_eq!(csec.as_deref(), Some("GOCSPX-super-secret-value"));

        // Tokens & master key DPAPI persistence round-trip
        let tok_path = dir.join("gdrive_tokens_u1.dpapi");
        tokens.save_to_file(&tok_path).unwrap();
        let raw_tok = std::fs::read_to_string(&tok_path).unwrap();
        assert!(!raw_tok.contains("ya29.super_secret_access"));
        let loaded_tok = GDriveTokens::load_from_file(&tok_path).unwrap().unwrap();
        assert_eq!(loaded_tok.access_token, "ya29.super_secret_access");
        assert_eq!(loaded_tok.refresh_token, "1//super_secret_refresh");

        let mk_path = dir.join("gdrive_master_key_u1.dpapi");
        master.save_to_file(&mk_path).unwrap();
        let loaded_mk = CloudMasterKey::load_from_file(&mk_path).unwrap().unwrap();
        assert_eq!(loaded_mk.as_bytes(), master.as_bytes());

        // Stable device ID creation
        let dev_path = dir.join("gdrive_device_id.txt");
        let dev1 = load_or_create_device_id(&dev_path).unwrap();
        let dev2 = load_or_create_device_id(&dev_path).unwrap();
        assert_eq!(dev1, dev2);
        assert!(dev1.starts_with("dev-"));

        // Range header parser for 308 Resume Incomplete
        assert_eq!(parse_drive_range_next_offset("bytes=0-4194303"), Some(4_194_304));
        assert_eq!(parse_drive_range_next_offset("bytes=0-0"), Some(1));
        assert_eq!(parse_drive_range_next_offset("invalid"), None);

        // 403 storageQuotaExceeded maps to dedicated non-retryable StorageQuotaExceeded error
        let quota_body = r#"{"error":{"code":403,"message":"The user's Drive storage quota has been exceeded.","errors":[{"reason":"storageQuotaExceeded"}]}}"#;
        assert!(!is_retryable_drive_error(StatusCode::FORBIDDEN, quota_body));
        assert!(matches!(
            map_drive_http_error(StatusCode::FORBIDDEN, quota_body),
            GDriveError::StorageQuotaExceeded
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn oauth_loopback_validates_state_and_returns_code() {
        let flow = OAuthLoopbackFlow::bind(false, "test-client-id")
            .await
            .unwrap();
        let parsed = Url::parse(&flow.auth_url).unwrap();
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        let cb_url = format!("{}?code=auth_code_42&state={state}", flow.redirect_uri);
        let expected_redirect = flow.redirect_uri.clone();

        let client = Client::builder().no_proxy().build().unwrap();
        let sender = tokio::spawn(async move {
            let resp = client.get(&cb_url).send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let html = resp.text().await.unwrap();
            assert!(html.contains("return to Teza"));
        });

        let (code, verifier, redirect) = flow
            .wait_for_code_with_timeout(None, Duration::from_secs(5))
            .await
            .unwrap();
        sender.await.unwrap();
        assert_eq!(code, "auth_code_42");
        assert!(!verifier.is_empty());
        assert_eq!(redirect, expected_redirect);
    }

    #[tokio::test]
    async fn resumable_chunked_upload_and_streaming_download_work_against_mock_drive() {
        use tokio::sync::Mutex;

        let dir = temp_test_dir("resumable-mock");
        // 600 KiB payload (> 2 chunks of 256 KiB!) to verify multi-chunk 308 -> 200 streaming
        let payload: Vec<u8> = (0..(600 * 1024 + 123))
            .map(|i| ((i * 37 + 13) % 251) as u8)
            .collect();
        let src_path = dir.join("big.enc");
        std::fs::write(&src_path, &payload).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let uploaded_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let uploaded_for_srv = uploaded_buf.clone();
        let expected_total = payload.len();

        let server = tokio::spawn(async move {
            let mut chunk_puts = 0usize;
            for _ in 0..8 {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                // Read HTTP headers
                let mut raw = Vec::new();
                let mut tmp = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break 0;
                    }
                    raw.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
                let first_line = head_str.lines().next().unwrap_or("");
                let content_len: usize = head_str
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let mut body = raw[header_end..].to_vec();
                while body.len() < content_len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                body.truncate(content_len);

                if first_line.starts_with("POST /upload/files") {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nLocation: http://{addr}/session/abc\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                } else if first_line.starts_with("PUT /session/abc") {
                    chunk_puts += 1;
                    let mut guard = uploaded_for_srv.lock().await;
                    guard.extend_from_slice(&body);
                    let cur_len = guard.len();
                    drop(guard);
                    if cur_len < expected_total {
                        let resp = format!(
                            "HTTP/1.1 308 Resume Incomplete\r\nRange: bytes=0-{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            cur_len - 1
                        );
                        sock.write_all(resp.as_bytes()).await.unwrap();
                    } else {
                        let resp_body = format!(
                            "{{\"id\":\"drv-chunked-1\",\"size\":\"{expected_total}\"}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        sock.write_all(resp.as_bytes()).await.unwrap();
                    }
                } else if first_line.starts_with("GET /api/files/drv-chunked-1?alt=media") {
                    let data = uploaded_for_srv.lock().await.clone();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    sock.write_all(head.as_bytes()).await.unwrap();
                    sock.write_all(&data).await.unwrap();
                    break;
                }
                let _ = sock.shutdown().await;
            }
            chunk_puts
        });

        let client = GDriveClient::with_endpoints(GDriveEndpoints {
            token_url: format!("http://{addr}/token"),
            drive_api_base: format!("http://{addr}/api"),
            drive_upload_base: format!("http://{addr}/upload"),
        })
        .unwrap();

        let file_id = client
            .upload_file_resumable_chunked(
                false,
                "test-token",
                "blobs-folder",
                "blob.enc",
                "application/octet-stream",
                &src_path,
                256 * 1024,
            )
            .await
            .unwrap();
        assert_eq!(file_id, "drv-chunked-1");

        let dl_path = dir.join("downloaded.enc");
        let dl_len = client
            .download_file_to_path(false, "test-token", &file_id, &dl_path)
            .await
            .unwrap();
        assert_eq!(dl_len, payload.len() as u64);
        assert_eq!(std::fs::read(&dl_path).unwrap(), payload);

        let chunk_puts = server.await.unwrap();
        assert_eq!(chunk_puts, 3, "600 KiB / 256 KiB should stream in 3 chunks");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sync_history_deltas_paginates_beyond_limit_with_boundary_ties_and_clock_skew() {
        use tokio::sync::Mutex;

        let dir = temp_test_dir("delta-sync-paged");
        let store_a = Arc::new(AsyncMutex::new(
            MessageStore::open(&dir.join("dev_a.db")).unwrap(),
        ));
        let store_b = Arc::new(AsyncMutex::new(
            MessageStore::open(&dir.join("dev_b.db")).unwrap(),
        ));
        let master = CloudMasterKey::generate().unwrap();

        // Populate 9 messages with timestamps far behind current PC clock (simulating severe PC clock lead)
        // and 3 siblings sharing identical boundary millisecond 1_600_000_002_000.
        let initial_msgs = [
            ("t1", "m1", 1_600_000_001_000i64),
            ("t1", "m2", 1_600_000_001_500),
            ("t1", "m3", 1_600_000_002_000),
            ("t1", "m4", 1_600_000_002_000),
            ("t2", "m5", 1_600_000_002_000),
            ("t1", "m6", 1_600_000_003_000),
            ("t1", "m7", 1_600_000_004_000),
            ("t1", "m8", 1_600_000_005_000),
            ("t1", "m9", 1_600_000_006_000),
        ];
        {
            let mut guard = store_a.lock().await;
            for (tid, mid, ts) in initial_msgs {
                guard
                    .upsert_message(&MessageDto {
                        msg_id: mid.into(),
                        thread_id: tid.into(),
                        sender_id: "u1".into(),
                        sender_name: "Alice".into(),
                        text: format!("payload {tid}/{mid}"),
                        msg_type: "chat.text".into(),
                        ts,
                        is_outgoing: false,
                        is_recalled: false,
                        is_pinned: false,
                        quote: None,
                        reactions: Vec::new(),
                        attachment_url: None,
                        attachment_name: None,
                    })
                    .unwrap();
            }
        }

        // Mock Drive server supporting list, multipart upload, and alt=media download
        type MockFilesStore = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let files_store: MockFilesStore = Arc::new(Mutex::new(Vec::new()));
        let srv_files = files_store.clone();
        let stop_srv = Arc::new(Notify::new());
        let stop_srv_clone = stop_srv.clone();

        let server = tokio::spawn(async move {
            let mut next_id = 1usize;
            loop {
                let accepted = tokio::select! {
                    _ = stop_srv_clone.notified() => break,
                    res = listener.accept() => res,
                };
                let Ok((mut sock, _)) = accepted else {
                    break;
                };
                let mut raw = Vec::new();
                let mut tmp = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break 0;
                    }
                    raw.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                if header_end == 0 {
                    continue;
                }
                let head_str = String::from_utf8_lossy(&raw[..header_end]).to_string();
                let first_line = head_str.lines().next().unwrap_or("");
                let content_len: usize = head_str
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let mut body = raw[header_end..].to_vec();
                while body.len() < content_len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                body.truncate(content_len);

                if first_line.starts_with("GET /api/files?") {
                    let files = srv_files.lock().await;
                    let arr: Vec<serde_json::Value> = files
                        .iter()
                        .map(|(id, name, data)| {
                            serde_json::json!({
                                "id": id,
                                "name": name,
                                "size": data.len().to_string(),
                                "mimeType": "application/octet-stream"
                            })
                        })
                        .collect();
                    let json = serde_json::json!({ "files": arr }).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                } else if first_line.starts_with("POST /upload/files") {
                    // Extract boundary, metadata name, and binary second part from multipart/related
                    let boundary = head_str
                        .lines()
                        .find_map(|l| l.split("boundary=").nth(1))
                        .unwrap()
                        .trim()
                        .to_string();
                    let delim = format!("--{boundary}").into_bytes();
                    let mut parts: Vec<&[u8]> = Vec::new();
                    let mut pos = 0;
                    while let Some(rel) = body[pos..].windows(delim.len()).position(|w| w == delim) {
                        let start = pos + rel + delim.len();
                        if body[start..].starts_with(b"--") {
                            break;
                        }
                        let part_start = if body[start..].starts_with(b"\r\n") {
                            start + 2
                        } else {
                            start
                        };
                        if let Some(next_rel) = body[part_start..]
                            .windows(delim.len())
                            .position(|w| w == delim)
                        {
                            let end = (part_start + next_rel).saturating_sub(2);
                            parts.push(&body[part_start..end]);
                            pos = part_start + next_rel;
                        } else {
                            break;
                        }
                    }
                    let meta_part = parts[0];
                    let meta_split = meta_part.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                    let meta_json: serde_json::Value =
                        serde_json::from_slice(&meta_part[meta_split + 4..]).unwrap();
                    let name = meta_json["name"].as_str().unwrap().to_string();
                    let data_part = parts[1];
                    let data_split = data_part.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                    let data = data_part[data_split + 4..].to_vec();

                    let fid = format!("drv-delta-{next_id}");
                    next_id += 1;
                    let sz = data.len();
                    srv_files.lock().await.push((fid.clone(), name, data));
                    let resp_body = format!("{{\"id\":\"{fid}\",\"size\":\"{sz}\"}}");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                } else if let Some(rest) = first_line.strip_prefix("GET /api/files/") {
                    let fid = rest.split('?').next().unwrap_or("");
                    let maybe_data = srv_files
                        .lock()
                        .await
                        .iter()
                        .find(|(id, _, _)| id == fid)
                        .map(|(_, _, d)| d.clone());
                    if let Some(data) = maybe_data {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            data.len()
                        );
                        sock.write_all(head.as_bytes()).await.unwrap();
                        sock.write_all(&data).await.unwrap();
                    } else {
                        sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                    }
                }
                let _ = sock.shutdown().await;
            }
        });

        let client = GDriveClient::with_endpoints(GDriveEndpoints {
            token_url: format!("http://{addr}/token"),
            drive_api_base: format!("http://{addr}/api"),
            drive_upload_base: format!("http://{addr}/upload"),
        })
        .unwrap();

        // Cycle 1 on Device A with tiny page limit=3 -> must upload multiple pages and stop at 1_600_000_006_001 (NOT PC now!)
        let rep1 = client
            .sync_history_deltas_with_limit(
                false,
                "tok",
                "hist-folder",
                &store_a,
                &master,
                "u1",
                "dev-A",
                0,
                3,
            )
            .await
            .unwrap();
        assert_eq!(rep1.new_last_history_sync_ts, 1_600_000_006_001);
        assert!(files_store.lock().await.len() >= 2);

        // New message arrives with server ts STILL far behind PC wall-clock -> must NOT be skipped!
        {
            let mut guard = store_a.lock().await;
            guard
                .upsert_message(&MessageDto {
                    msg_id: "m10".into(),
                    thread_id: "t2".into(),
                    sender_id: "u2".into(),
                    sender_name: "Bob".into(),
                    text: "arrived under clock skew".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_600_000_009_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                })
                .unwrap();
        }
        let rep2 = client
            .sync_history_deltas_with_limit(
                false,
                "tok",
                "hist-folder",
                &store_a,
                &master,
                "u1",
                "dev-A",
                rep1.new_last_history_sync_ts,
                3,
            )
            .await
            .unwrap();
        assert_eq!(rep2.new_last_history_sync_ts, 1_600_000_009_001);
        assert!(rep2.uploaded_delta_id.is_some());

        // Device B pulls all uploaded delta pages into a fresh DB -> ALL 10 messages restored!
        let rep_b = client
            .sync_history_deltas_with_limit(
                false,
                "tok",
                "hist-folder",
                &store_b,
                &master,
                "u1",
                "dev-B",
                0,
                2000,
            )
            .await
            .unwrap();
        assert_eq!(rep_b.applied_messages, 10);
        {
            let guard = store_b.lock().await;
            for (tid, mid, _) in initial_msgs {
                assert!(guard.get_message(tid, mid).unwrap().is_some());
            }
            assert_eq!(
                guard.get_message("t2", "m10").unwrap().unwrap().text,
                "arrived under clock skew"
            );
        }

        stop_srv.notify_one();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_broker_url_validation_and_oauth_mode_resolution() {
        assert_eq!(validate_token_broker_url("").unwrap(), None);
        assert_eq!(validate_token_broker_url("   ").unwrap(), None);
        assert_eq!(
            validate_token_broker_url("https://oauth.teza.example.com///").unwrap(),
            Some("https://oauth.teza.example.com".to_string())
        );
        assert_eq!(
            validate_token_broker_url("http://127.0.0.1:8787/").unwrap(),
            Some("http://127.0.0.1:8787".to_string())
        );
        // Reject insecure remote HTTP, userinfo, query, fragment
        assert!(validate_token_broker_url("http://evil.example.com").is_err());
        assert!(validate_token_broker_url("https://user:pass@oauth.example.com").is_err());
        assert!(validate_token_broker_url("https://oauth.example.com?x=1").is_err());
        assert!(validate_token_broker_url("https://oauth.example.com#frag").is_err());

        let mut cfg = GDriveStoredConfig {
            client_id: "cid-custom.apps.googleusercontent.com".into(),
            ..Default::default()
        };
        cfg.set_client_secret(Some("GOCSPX-secret")).unwrap();
        assert_eq!(cfg.oauth_mode(), "custom_direct");
        let resolved_direct = cfg.resolve_oauth_config().unwrap();
        assert_eq!(resolved_direct.mode, "custom_direct");
        assert_eq!(resolved_direct.client_secret.as_deref(), Some("GOCSPX-secret"));
        assert!(resolved_direct.token_broker_url.is_none());

        // Setting a token broker switches to zero-secret broker mode (client_secret omitted!)
        cfg.set_token_broker_url(Some("https://broker.teza.example.com/"))
            .unwrap();
        assert_eq!(cfg.oauth_mode(), "custom_broker");
        let resolved_broker = cfg.resolve_oauth_config().unwrap();
        assert_eq!(resolved_broker.mode, "custom_broker");
        assert!(resolved_broker.client_secret.is_none());
        assert_eq!(
            resolved_broker.token_broker_url.as_deref(),
            Some("https://broker.teza.example.com")
        );
        assert!(!format!("{resolved_broker:?}").contains("GOCSPX"));

        // Reset custom OAuth clears client_id, secret, and broker URL
        cfg.reset_custom_oauth();
        assert!(cfg.client_id.is_empty());
        assert!(cfg.client_secret_dpapi_b64.is_none());
        assert!(cfg.token_broker_url.is_empty());
    }

    #[tokio::test]
    async fn oauth_token_broker_exchanges_and_refreshes_without_client_secret() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_bodies: Arc<tokio::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen_bodies);

        let srv = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let first_line = req.lines().next().unwrap_or("").to_string();
                let body = req
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                let path = first_line.split_whitespace().nth(1).unwrap_or("").to_string();
                seen_clone.lock().await.push((path.clone(), body));
                let json = if path == "/token" {
                    r#"{"access_token":"ya29.broker_access_1","refresh_token":"1//broker_refresh","expires_in":3600}"#
                } else {
                    r#"{"access_token":"ya29.broker_access_2","expires_in":3600}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                stream.write_all(resp.as_bytes()).await.unwrap();
                let _ = stream.shutdown().await;
            }
        });

        let client = GDriveClient::new().unwrap();
        let broker_base = format!("http://{addr}");

        // Strict Relay guard applies to Token Broker too
        let blocked = client
            .exchange_oauth_code_with_broker(
                true,
                "cid-1",
                Some("SHOULD_NOT_BE_SENT"),
                Some(&broker_base),
                "code-1",
                "verifier-1234567890123456789012345678901234567890",
                "http://127.0.0.1:43210/callback",
            )
            .await;
        assert!(matches!(blocked, Err(GDriveError::RelayBlocked)));

        let mut tokens = client
            .exchange_oauth_code_with_broker(
                false,
                "cid-1",
                Some("SHOULD_NOT_BE_SENT"),
                Some(&broker_base),
                "code-1",
                "verifier-1234567890123456789012345678901234567890",
                "http://127.0.0.1:43210/callback",
            )
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "ya29.broker_access_1");
        assert_eq!(tokens.refresh_token, "1//broker_refresh");

        // Force expiry and refresh via broker
        tokens.expires_at_epoch_secs = 0;
        let refreshed = client
            .refresh_access_token_with_broker(
                false,
                "cid-1",
                Some("SHOULD_NOT_BE_SENT"),
                Some(&broker_base),
                &mut tokens,
            )
            .await
            .unwrap();
        assert!(refreshed);
        assert_eq!(tokens.access_token, "ya29.broker_access_2");

        srv.await.unwrap();
        let captured = seen_bodies.lock().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].0, "/token");
        assert!(captured[0].1.contains("\"code\":\"code-1\""));
        assert!(!captured[0].1.contains("SHOULD_NOT_BE_SENT"));
        assert_eq!(captured[1].0, "/refresh");
        assert!(captured[1].1.contains("\"refresh_token\":\"1//broker_refresh\""));
        assert!(!captured[1].1.contains("SHOULD_NOT_BE_SENT"));
    }
}
