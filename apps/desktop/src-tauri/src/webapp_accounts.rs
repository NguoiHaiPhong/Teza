//! Persistent, DPAPI-encrypted saved accounts and safe autofill helpers for embedded Web Apps.
//!
//! Passwords are sealed via Windows DPAPI (`zalo_core::dpapi`) before persistence, written
//! atomically with `zalo_core::data_dir::write_private_atomic`, never exposed in IPC DTOs,
//! and redacted in all `Debug` implementations.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use zalo_core::data_dir;
use zalo_core::dpapi;

const SCHEMA_VERSION: u32 = 1;
const MAX_WEBAPP_ACCOUNTS: usize = 200;
const MAX_APP_ID_LEN: usize = 256;
const MAX_LABEL_LEN: usize = 256;
const MAX_USERNAME_LEN: usize = 512;
const MAX_PASSWORD_LEN: usize = 2048;
const MAX_WEB_URL_LEN: usize = 4096;

static NEXT_ACCOUNT_SEQ: AtomicU64 = AtomicU64::new(1);

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Public DTO returned to the frontend over IPC. Never contains plaintext or sealed passwords.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebAppAccountDto {
    pub id: String,
    pub app_id: String,
    pub origin: String,
    pub label: String,
    pub username: String,
    pub has_password: bool,
    pub auto_fill: bool,
    pub updated_at_ms: i64,
}

/// Input payload accepted from the frontend to create or update a saved Web App account.
#[derive(Clone, Deserialize)]
pub struct WebAppAccountInput {
    #[serde(default)]
    pub id: Option<String>,
    pub app_id: String,
    pub url: String,
    #[serde(default)]
    pub label: String,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub clear_password: Option<bool>,
    #[serde(default)]
    pub auto_fill: bool,
}

impl fmt::Debug for WebAppAccountInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebAppAccountInput")
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("url", &self.url)
            .field("label", &self.label)
            .field("username", &self.username)
            .field(
                "password",
                &self
                    .password
                    .as_ref()
                    .map(|p| if p.is_empty() { "" } else { "[REDACTED]" }),
            )
            .field("clear_password", &self.clear_password)
            .field("auto_fill", &self.auto_fill)
            .finish()
    }
}

/// Decrypted account credentials kept strictly in Rust memory for immediate autofill execution.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedWebAppCredentials {
    pub id: String,
    pub app_id: String,
    pub origin: String,
    pub username: String,
    pub password: String,
}

impl ResolvedWebAppCredentials {
    fn zeroize_password(&mut self) {
        // SAFETY: Overwriting every byte of a UTF-8 `String` with 0x00 preserves valid UTF-8
        // before `String`'s own `Drop` deallocates the buffer.
        unsafe {
            for b in self.password.as_bytes_mut() {
                std::ptr::write_volatile(b, 0);
            }
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

impl Drop for ResolvedWebAppCredentials {
    fn drop(&mut self) {
        self.zeroize_password();
    }
}

impl fmt::Debug for ResolvedWebAppCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedWebAppCredentials")
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("origin", &self.origin)
            .field("username", &self.username)
            .field(
                "password",
                &if self.password.is_empty() {
                    ""
                } else {
                    "[REDACTED]"
                },
            )
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredWebAppAccount {
    id: String,
    app_id: String,
    origin: String,
    label: String,
    username: String,
    #[serde(default)]
    sealed_password: String,
    #[serde(default)]
    auto_fill: bool,
    #[serde(default)]
    updated_at_ms: i64,
}

impl fmt::Debug for StoredWebAppAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredWebAppAccount")
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("origin", &self.origin)
            .field("label", &self.label)
            .field("username", &self.username)
            .field("has_password", &!self.sealed_password.is_empty())
            .field("auto_fill", &self.auto_fill)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

impl StoredWebAppAccount {
    fn to_dto(&self) -> WebAppAccountDto {
        WebAppAccountDto {
            id: self.id.clone(),
            app_id: self.app_id.clone(),
            origin: self.origin.clone(),
            label: self.label.clone(),
            username: self.username.clone(),
            has_password: !self.sealed_password.is_empty(),
            auto_fill: self.auto_fill,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredWebAppAccountsFile {
    #[serde(default = "default_schema_version")]
    version: u32,
    #[serde(default)]
    accounts: Vec<StoredWebAppAccount>,
}

impl Default for StoredWebAppAccountsFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            accounts: Vec::new(),
        }
    }
}

/// Persistent store of saved Web App accounts backed by an atomically written private JSON file.
pub struct WebAppAccountStore {
    path: PathBuf,
    accounts: Vec<StoredWebAppAccount>,
}

impl fmt::Debug for WebAppAccountStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebAppAccountStore")
            .field("path", &self.path)
            .field("count", &self.accounts.len())
            .finish()
    }
}

fn is_local_or_private_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || (a == 100 && (64..=127).contains(&b))
}

fn is_loopback_host(host_lower_no_dot: &str) -> bool {
    if host_lower_no_dot == "localhost" || host_lower_no_dot.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host_lower_no_dot.parse::<std::net::Ipv4Addr>() {
        return ip.is_loopback() || ip.is_unspecified();
    }
    let ipv6_candidate = host_lower_no_dot
        .trim_start_matches('[')
        .trim_end_matches(']');
    if let Ok(ip) = ipv6_candidate.parse::<std::net::Ipv6Addr>() {
        return ip.is_loopback()
            || ip.is_unspecified()
            || ip
                .to_ipv4_mapped()
                .is_some_and(|v4| v4.is_loopback() || v4.is_unspecified());
    }
    false
}

/// Reject Tauri internal/IPC/asset hosts, the Vite dev origin (`localhost:1420` / `127.0.0.1:1420`),
/// and any non-`http`/`https` scheme from ever loading inside embedded child WebViews.
pub fn is_forbidden_internal_webview_url(url: &tauri::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    let Some(raw_host) = url.host_str().filter(|h| !h.is_empty()) else {
        return true;
    };
    let host = raw_host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host == "tauri.localhost"
        || host.ends_with(".tauri.localhost")
        || host == "ipc.localhost"
        || host.ends_with(".ipc.localhost")
        || host == "asset.localhost"
        || host.ends_with(".asset.localhost")
    {
        return true;
    }
    if url.port_or_known_default() == Some(1420) && is_loopback_host(&host) {
        return true;
    }
    false
}

fn is_local_or_private_host(host: &str) -> bool {
    let lower = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".lan")
        || lower.ends_with(".internal")
    {
        return true;
    }
    if let Ok(ip) = lower.parse::<std::net::Ipv4Addr>() {
        return is_local_or_private_ipv4(ip);
    }
    let ipv6_candidate = lower.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = ipv6_candidate.parse::<std::net::Ipv6Addr>() {
        if let Some(v4) = ip.to_ipv4_mapped() {
            return is_local_or_private_ipv4(v4);
        }
        let seg0 = ip.segments()[0];
        return ip.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80;
    }
    false
}

fn has_explicit_scheme(raw: &str) -> bool {
    let head_end = raw.find(['/', '?', '#']).unwrap_or(raw.len());
    let head = &raw[..head_end];
    let Some((scheme, rest_in_head)) = head.split_once(':') else {
        return false;
    };
    if scheme.is_empty() {
        return false;
    }
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return false;
    }
    if raw[scheme.len() + 1..].starts_with("//") {
        return true;
    }
    rest_in_head.is_empty() || !rest_in_head.chars().all(|c| c.is_ascii_digit())
}

/// Validate a user-provided web URL, allowing only `http`/`https`, rejecting control characters,
/// credentials/userinfo, oversized inputs, and Tauri internal hosts, and defaulting bare LAN/private
/// hosts to `http://` (and public hosts to `https://`).
pub fn validate_web_url(raw: &str) -> Result<tauri::Url, String> {
    if raw.chars().any(char::is_control) {
        return Err("URL không hợp lệ: chứa ký tự điều khiển".into());
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL không hợp lệ: rỗng".into());
    }
    if trimmed.len() > MAX_WEB_URL_LEN {
        return Err("URL vượt quá độ dài tối đa cho phép".into());
    }
    let with_scheme = if has_explicit_scheme(trimmed) {
        trimmed.to_string()
    } else {
        let probe = format!("http://{trimmed}");
        let prefer_http = tauri::Url::parse(&probe)
            .ok()
            .and_then(|u| u.host_str().map(is_local_or_private_host))
            .unwrap_or(false);
        if prefer_http {
            probe
        } else {
            format!("https://{trimmed}")
        }
    };
    let parsed =
        tauri::Url::parse(&with_scheme).map_err(|e| format!("URL không hợp lệ: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Chỉ hỗ trợ giao thức http/https (nhận: {other})")),
    }
    if parsed.host_str().map(str::is_empty).unwrap_or(true) {
        return Err("URL phải có tên miền hợp lệ".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL không được chứa thông tin đăng nhập (user:pass@)".into());
    }
    if is_forbidden_internal_webview_url(&parsed) {
        return Err("Không cho phép mở địa chỉ nội bộ của ứng dụng".into());
    }
    Ok(parsed)
}

/// Extract canonical `scheme://host[:port]` origin from a parsed `tauri::Url`.
/// Default ports (`:80` for `http`, `:443` for `https`) are omitted and hostnames are normalized.
pub fn canonical_origin_from_url(url: &tauri::Url) -> Result<String, String> {
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return Err(format!("Chỉ hỗ trợ giao thức http/https (nhận: {scheme})"));
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "URL phải có tên miền hợp lệ".to_string())?;
    let origin = url.origin().ascii_serialization();
    if origin.is_empty() || origin == "null" {
        return Ok(match url.port() {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        });
    }
    Ok(origin)
}

/// Validate a raw URL or bare host and return its canonical `scheme://host[:port]` origin.
pub fn extract_canonical_origin(raw: &str) -> Result<String, String> {
    let parsed = validate_web_url(raw)?;
    canonical_origin_from_url(&parsed)
}

/// Verify that the webview's current URL matches the saved account's canonical origin.
pub fn verify_origin_matches(expected_origin: &str, current_url: &tauri::Url) -> Result<(), String> {
    let expected = extract_canonical_origin(expected_origin)?;
    let actual = canonical_origin_from_url(current_url).map_err(|_| {
        format!(
            "Không thể điền tài khoản vì trang hiện tại ({}) không thuộc giao thức http/https hợp lệ",
            current_url.scheme()
        )
    })?;
    if actual != expected {
        return Err(format!(
            "Không thể điền tài khoản vì trang hiện tại ({actual}) khác nguồn gốc đã lưu ({expected})"
        ));
    }
    Ok(())
}

fn seal_password(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    dpapi::protect_secret(plain).map_err(|e| format!("Không thể mã hóa mật khẩu: {e}"))
}

fn unseal_password(sealed: &str) -> Result<String, String> {
    if sealed.trim().is_empty() {
        return Ok(String::new());
    }
    dpapi::unprotect_secret(sealed).map_err(|e| format!("Không thể giải mã mật khẩu đã lưu: {e}"))
}

fn append_os_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn backup_corrupt_store_file(path: &std::path::Path, reason: &str) {
    let ts = now_epoch_ms().max(1);
    let mut backup_path = append_os_suffix(path, &format!(".corrupt.{ts}"));
    if backup_path.exists() {
        let seq = NEXT_ACCOUNT_SEQ.fetch_add(1, Ordering::Relaxed);
        backup_path = append_os_suffix(path, &format!(".corrupt.{ts}.{seq}"));
    }
    match std::fs::rename(path, &backup_path) {
        Ok(()) => {
            eprintln!(
                "[teza][webapp_accounts] Failed to parse {} ({reason}); backed up corrupt file to {} and starting empty",
                path.display(),
                backup_path.display()
            );
        }
        Err(rename_err) => match std::fs::copy(path, &backup_path) {
            Ok(_) => {
                eprintln!(
                    "[teza][webapp_accounts] Failed to parse {} ({reason}); rename failed ({rename_err}), copied corrupt file to {} and starting empty",
                    path.display(),
                    backup_path.display()
                );
            }
            Err(copy_err) => {
                eprintln!(
                    "[teza][webapp_accounts] Failed to parse {} ({reason}); failed to back up to {} (rename={rename_err}; copy={copy_err})",
                    path.display(),
                    backup_path.display()
                );
            }
        },
    }
}

impl WebAppAccountStore {
    /// Load saved accounts from `path`, backing up corrupt files before falling back to empty.
    pub fn load(path: PathBuf) -> Self {
        let accounts = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredWebAppAccountsFile>(&bytes) {
                Ok(file) => file.accounts,
                Err(parse_err) => {
                    backup_corrupt_store_file(&path, &parse_err.to_string());
                    Vec::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                eprintln!(
                    "[teza][webapp_accounts] Failed to read {}: {err}; starting empty",
                    path.display()
                );
                Vec::new()
            }
        };
        Self { path, accounts }
    }

    fn persist_accounts(&self, accounts: &[StoredWebAppAccount]) -> Result<(), String> {
        let payload = StoredWebAppAccountsFile {
            version: SCHEMA_VERSION,
            accounts: accounts.to_vec(),
        };
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("Không thể tuần tự hóa danh sách tài khoản: {e}"))?;
        data_dir::write_private_atomic(&self.path, json.as_bytes())
            .map_err(|e| format!("Không thể lưu tệp tài khoản Web App: {e}"))
    }

    fn generate_id(&self, now_ms: i64) -> String {
        loop {
            let seq = NEXT_ACCOUNT_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("wacc_{now_ms}_{seq}");
            if !self.accounts.iter().any(|a| a.id == candidate) {
                return candidate;
            }
        }
    }

    /// List saved accounts filtered by optional `app_id` and/or `origin`, sorted by `updated_at_ms` desc.
    pub fn list(&self, app_id: Option<&str>, origin: Option<&str>) -> Vec<WebAppAccountDto> {
        let app_filter = app_id.map(str::trim).filter(|s| !s.is_empty());
        let origin_filter = origin.map(str::trim).filter(|s| !s.is_empty()).map(|raw| {
            extract_canonical_origin(raw).unwrap_or_else(|_| raw.to_ascii_lowercase())
        });

        let mut rows: Vec<WebAppAccountDto> = self
            .accounts
            .iter()
            .filter(|acc| {
                if let Some(aid) = app_filter {
                    if acc.app_id != aid {
                        return false;
                    }
                }
                if let Some(ref orig) = origin_filter {
                    if acc.origin != *orig {
                        return false;
                    }
                }
                true
            })
            .map(StoredWebAppAccount::to_dto)
            .collect();

        rows.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        rows
    }

    /// Create or update a saved Web App account and persist atomically to disk.
    pub fn save(&mut self, input: WebAppAccountInput) -> Result<WebAppAccountDto, String> {
        self.save_with_timestamp(input, now_epoch_ms())
    }

    fn save_with_timestamp(
        &mut self,
        input: WebAppAccountInput,
        now_ms: i64,
    ) -> Result<WebAppAccountDto, String> {
        let app_id = input.app_id.trim().to_string();
        if app_id.is_empty() {
            return Err("Thiếu mã Web App".into());
        }
        if app_id.chars().count() > MAX_APP_ID_LEN {
            return Err("Mã Web App quá dài".into());
        }

        let origin = extract_canonical_origin(&input.url)?;

        let username = input.username.trim().to_string();
        if username.is_empty() {
            return Err("Vui lòng nhập tên đăng nhập hoặc email".into());
        }
        if username.chars().count() > MAX_USERNAME_LEN {
            return Err("Tên đăng nhập quá dài".into());
        }

        let trimmed_label = input.label.trim();
        let label = if trimmed_label.is_empty() {
            username.clone()
        } else {
            trimmed_label.to_string()
        };
        if label.chars().count() > MAX_LABEL_LEN {
            return Err("Tên gợi nhớ quá dài".into());
        }

        if let Some(ref pw) = input.password {
            if pw.chars().count() > MAX_PASSWORD_LEN {
                return Err("Mật khẩu vượt quá độ dài cho phép".into());
            }
        }

        let clear_password = input.clear_password == Some(true);
        let new_password = input.password.as_deref().filter(|p| !p.is_empty());

        let existing_id = input
            .id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let mut staged = self.accounts.clone();

        let updated_id = if let Some(ref id) = existing_id {
            let Some(existing_idx) = staged.iter().position(|a| &a.id == id) else {
                return Err("Không tìm thấy tài khoản cần cập nhật".into());
            };
            let origin_changed = staged[existing_idx].origin != origin;
            if origin_changed
                && !staged[existing_idx].sealed_password.is_empty()
                && !clear_password
                && new_password.is_none()
            {
                return Err(
                    "Không thể giữ mật khẩu cũ khi đổi địa chỉ nguồn gốc (origin). Vui lòng nhập mật khẩu mới hoặc chọn xóa mật khẩu."
                        .into(),
                );
            }
            let next_sealed = if clear_password {
                String::new()
            } else if let Some(pw) = new_password {
                seal_password(pw)?
            } else {
                staged[existing_idx].sealed_password.clone()
            };

            let acc = &mut staged[existing_idx];
            acc.app_id = app_id.clone();
            acc.origin = origin.clone();
            acc.label = label;
            acc.username = username;
            acc.sealed_password = next_sealed;
            acc.auto_fill = input.auto_fill;
            acc.updated_at_ms = now_ms;
            id.clone()
        } else {
            if staged.len() >= MAX_WEBAPP_ACCOUNTS {
                return Err(format!(
                    "Đã đạt giới hạn tối đa {MAX_WEBAPP_ACCOUNTS} tài khoản Web App đã lưu. Vui lòng xóa bớt tài khoản cũ trước khi thêm mới."
                ));
            }
            let sealed_password = if clear_password {
                String::new()
            } else if let Some(pw) = new_password {
                seal_password(pw)?
            } else {
                String::new()
            };
            let id = self.generate_id(now_ms);
            staged.push(StoredWebAppAccount {
                id: id.clone(),
                app_id: app_id.clone(),
                origin: origin.clone(),
                label,
                username,
                sealed_password,
                auto_fill: input.auto_fill,
                updated_at_ms: now_ms,
            });
            id
        };

        // Keep auto_fill deterministic: at most one auto_fill account per (app_id, origin).
        if input.auto_fill {
            for acc in &mut staged {
                if acc.id != updated_id && acc.app_id == app_id && acc.origin == origin {
                    acc.auto_fill = false;
                }
            }
        }

        // Transactional persist: write atomically to disk FIRST, commit in-memory only on Ok(()).
        self.persist_accounts(&staged)?;
        self.accounts = staged;

        self.accounts
            .iter()
            .find(|a| a.id == updated_id)
            .map(StoredWebAppAccount::to_dto)
            .ok_or_else(|| "Không tìm thấy tài khoản vừa lưu".to_string())
    }

    /// Delete a saved account by `id`.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err("Thiếu mã tài khoản cần xóa".into());
        }
        let mut staged = self.accounts.clone();
        staged.retain(|a| a.id != trimmed);
        if staged.len() != self.accounts.len() {
            self.persist_accounts(&staged)?;
            self.accounts = staged;
        }
        Ok(())
    }

    /// Look up and unseal credentials for a specific account `id`.
    pub fn resolve_credentials(&self, account_id: &str) -> Result<ResolvedWebAppCredentials, String> {
        let trimmed = account_id.trim();
        let acc = self
            .accounts
            .iter()
            .find(|a| a.id == trimmed)
            .ok_or_else(|| "Không tìm thấy tài khoản đã lưu".to_string())?;
        let password = unseal_password(&acc.sealed_password)?;
        Ok(ResolvedWebAppCredentials {
            id: acc.id.clone(),
            app_id: acc.app_id.clone(),
            origin: acc.origin.clone(),
            username: acc.username.clone(),
            password,
        })
    }

    /// Find and unseal the `auto_fill == true` account matching `(app_id, origin)`, if any.
    pub fn resolve_autofill_for_page(
        &self,
        app_id: &str,
        origin: &str,
    ) -> Result<Option<ResolvedWebAppCredentials>, String> {
        let aid = app_id.trim();
        let Some(acc) = self
            .accounts
            .iter()
            .filter(|a| a.auto_fill && a.app_id == aid && a.origin == origin)
            .max_by_key(|a| a.updated_at_ms)
        else {
            return Ok(None);
        };
        let password = unseal_password(&acc.sealed_password)?;
        Ok(Some(ResolvedWebAppCredentials {
            id: acc.id.clone(),
            app_id: acc.app_id.clone(),
            origin: acc.origin.clone(),
            username: acc.username.clone(),
            password,
        }))
    }
}

fn js_string_literal(s: &str) -> Result<String, String> {
    let json = serde_json::to_string(s)
        .map_err(|e| format!("Không thể mã hóa chuỗi autofill: {e}"))?;
    Ok(json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

struct AutofillScriptOptions<'a> {
    submit: bool,
    only_if_empty: bool,
    require_login_signal: bool,
    expected_origin: Option<&'a str>,
    max_attempts: u32,
    retry_delay_ms: u32,
}

fn build_autofill_script_inner(
    username: &str,
    password: &str,
    opts: AutofillScriptOptions<'_>,
) -> Result<String, String> {
    let username_js = js_string_literal(username)?;
    let password_js = js_string_literal(password)?;
    let expected_origin_js = js_string_literal(opts.expected_origin.unwrap_or(""))?;
    let submit_js = if opts.submit { "true" } else { "false" };
    let only_if_empty_js = if opts.only_if_empty { "true" } else { "false" };
    let require_login_signal_js = if opts.require_login_signal {
        "true"
    } else {
        "false"
    };
    let max_attempts = opts.max_attempts;
    let retry_delay_ms = opts.retry_delay_ms;

    Ok(format!(
        r#"(() => {{
  const USERNAME = {username_js};
  const PASSWORD = {password_js};
  const SHOULD_SUBMIT = {submit_js};
  const ONLY_IF_EMPTY = {only_if_empty_js};
  const REQUIRE_LOGIN_SIGNAL = {require_login_signal_js};
  const EXPECTED_ORIGIN = {expected_origin_js};
  const MAX_ATTEMPTS = {max_attempts};
  const RETRY_DELAY_MS = {retry_delay_ms};

  function isAllowedOrigin() {{
    if (!EXPECTED_ORIGIN) return false;
    try {{
      return Boolean(window.location && window.location.origin === EXPECTED_ORIGIN);
    }} catch (_) {{
      return false;
    }}
  }}

  function isSameOriginUrl(rawUrl, doc) {{
    if (!EXPECTED_ORIGIN) return false;
    const base =
      (doc && doc.baseURI) || (window.location && window.location.href) || "";
    try {{
      const resolved = new URL(rawUrl, base);
      return (
        (resolved.protocol === "http:" || resolved.protocol === "https:") &&
        resolved.origin === EXPECTED_ORIGIN
      );
    }} catch (_) {{
      return false;
    }}
  }}

  function isSafeFormAction(form, submitBtn) {{
    const doc = (form && form.ownerDocument) || document;
    if (submitBtn && typeof submitBtn.getAttribute === "function") {{
      const btnAction = submitBtn.getAttribute("formaction");
      if (btnAction !== null && String(btnAction).trim() !== "") {{
        if (!isSameOriginUrl(btnAction, doc)) return false;
      }}
    }}
    if (form && typeof form.getAttribute === "function") {{
      const rawAction = form.getAttribute("action");
      if (rawAction !== null && String(rawAction).trim() !== "") {{
        if (!isSameOriginUrl(rawAction, doc)) return false;
      }} else if (typeof form.action === "string" && form.action.trim() !== "") {{
        if (!isSameOriginUrl(form.action, doc)) return false;
      }}
    }}
    return true;
  }}

  function hasAutocompleteToken(el, token) {{
    const raw = String((el && el.getAttribute && el.getAttribute("autocomplete")) || "")
      .trim()
      .toLowerCase();
    if (!raw) return false;
    return raw.split(/\s+/).includes(token);
  }}

  function isNonLoginPasswordField(el) {{
    return (
      hasAutocompleteToken(el, "new-password") ||
      hasAutocompleteToken(el, "one-time-code")
    );
  }}

  function collectInputs(root, out, seen, depth) {{
    if (!root || depth > 8 || seen.has(root)) return;
    seen.add(root);
    let nodes;
    try {{
      nodes = root.querySelectorAll("*");
    }} catch (_) {{
      return;
    }}
    for (let i = 0; i < nodes.length; i++) {{
      const el = nodes[i];
      const tag = String(el.tagName || "").toUpperCase();
      if (tag === "INPUT") {{
        out.push(el);
      }}
      if (el.shadowRoot) {{
        collectInputs(el.shadowRoot, out, seen, depth + 1);
      }}
      if (tag === "IFRAME" || tag === "FRAME") {{
        try {{
          if (!isVisible(el)) continue;
          const frameWin = el.contentWindow;
          const frameLoc = frameWin && frameWin.location;
          if (
            !EXPECTED_ORIGIN ||
            !frameLoc ||
            (frameLoc.protocol !== "http:" && frameLoc.protocol !== "https:") ||
            frameLoc.origin !== EXPECTED_ORIGIN
          ) {{
            continue;
          }}
          const doc = el.contentDocument;
          if (doc) collectInputs(doc, out, seen, depth + 1);
        }} catch (_) {{}}
      }}
    }}
  }}

  function isVisible(el) {{
    if (!el) return false;
    const doc = el.ownerDocument || document;
    const win = (doc && doc.defaultView) || window;
    let style;
    try {{
      style = win.getComputedStyle(el);
    }} catch (_) {{
      return false;
    }}
    if (!style) return false;
    const opacity = parseFloat(style.opacity || "1");
    if (
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.visibility === "collapse" ||
      Number.isNaN(opacity) ||
      opacity < 0.1
    ) {{
      return false;
    }}
    const rect = el.getBoundingClientRect();
    if (!rect || rect.width < 4 || rect.height < 4) {{
      return false;
    }}
    const vw =
      win.innerWidth || (doc.documentElement && doc.documentElement.clientWidth) || 0;
    const vh =
      win.innerHeight || (doc.documentElement && doc.documentElement.clientHeight) || 0;
    if (vw > 0 && vh > 0) {{
      if (rect.right <= 0 || rect.bottom <= 0 || rect.left >= vw || rect.top >= vh) {{
        return false;
      }}
      const visW = Math.min(rect.right, vw) - Math.max(rect.left, 0);
      const visH = Math.min(rect.bottom, vh) - Math.max(rect.top, 0);
      if (visW < 4 || visH < 4) {{
        return false;
      }}
    }} else if (rect.right <= 0 || rect.bottom <= 0) {{
      return false;
    }}
    return true;
  }}

  function isUsableInput(el) {{
    if (!el || el.disabled || el.readOnly) return false;
    const t = String(el.type || "text").toLowerCase();
    if (
      t === "hidden" ||
      t === "submit" ||
      t === "button" ||
      t === "checkbox" ||
      t === "radio" ||
      t === "file" ||
      t === "image" ||
      t === "reset" ||
      t === "range" ||
      t === "color" ||
      t === "date" ||
      t === "time" ||
      t === "datetime-local" ||
      t === "month" ||
      t === "week"
    ) {{
      return false;
    }}
    return isVisible(el);
  }}

  function isUsernameType(el) {{
    const t = String(el.type || "text").toLowerCase();
    return t === "email" || t === "text" || t === "tel" || t === "";
  }}

  function usernameScore(el) {{
    if (!isUsernameType(el)) return -1000;
    if (hasAutocompleteToken(el, "one-time-code")) return -1000;
    const t = String(el.type || "text").toLowerCase();
    const ac = String(el.getAttribute("autocomplete") || "").toLowerCase();
    const meta = [
      el.name || "",
      el.id || "",
      el.placeholder || "",
      el.getAttribute("aria-label") || "",
      ac,
    ]
      .join(" ")
      .toLowerCase();

    let score = 0;
    if (ac.includes("username") || ac.includes("email")) score += 100;
    if (t === "email") score += 60;
    if (/(user|login|account|email|mail|phone|mobile|tel|uid|emp|staff|member|tài\s*khoản|đăng\s*nhập|số\s*điện\s*thoại)/i.test(meta)) {{
      score += 40;
    }}
    if (/(search|filter|captcha|otp|totp|2fa|mfa|verify|tìm\s*kiếm)/i.test(meta)) {{
      score -= 100;
    }}
    return score;
  }}

  function isLoginButtonText(btn) {{
    if (!btn) return false;
    const txt = String(
      btn.innerText || btn.textContent || btn.value || btn.getAttribute("aria-label") || ""
    )
      .replace(/\s+/g, " ")
      .trim()
      .toLowerCase();
    if (!txt || txt.length > 48) return false;
    if (
      /(cancel|close|hủy|huỷ|đóng|show|hide|hiện|ẩn|forgot|quên|reset|register|sign\s*up|đăng\s*ký|delete|xóa|xoá|back|quay\s*lại)/i.test(
        txt
      )
    ) {{
      return false;
    }}
    return /(log\s*in|sign\s*in|login|signin|đăng\s*nhập)/i.test(txt);
  }}

  function hasStrictLoginSignal(el) {{
    if (!el) return false;
    if (hasAutocompleteToken(el, "username")) return true;
    const form = el.form || (el.closest ? el.closest("form") : null);
    if (form) {{
      const formMeta = [
        form.id || "",
        form.name || "",
        form.getAttribute("action") || "",
        typeof form.className === "string" ? form.className : "",
      ]
        .join(" ")
        .toLowerCase();
      if (
        /(login|log-in|log_in|signin|sign-in|sign_in|auth|sso|dang-nhap|dangnhap)/i.test(formMeta)
      ) {{
        return true;
      }}
      const btns = Array.from(
        form.querySelectorAll('button[type="submit"], input[type="submit"], button, [role="button"]')
      );
      if (btns.some((b) => !b.disabled && isVisible(b) && isLoginButtonText(b))) {{
        return true;
      }}
    }}
    const path = String((window.location && window.location.pathname) || "").toLowerCase();
    if (/(^|\/)(login|signin|sign-in|log-in|auth|sso|dang-nhap)(\/|$)/i.test(path)) {{
      return usernameScore(el) >= 40;
    }}
    return false;
  }}

  function setNativeValue(el, val) {{
    const win = (el.ownerDocument && el.ownerDocument.defaultView) || window;
    const proto = win.HTMLInputElement && win.HTMLInputElement.prototype;
    const desc = proto ? Object.getOwnPropertyDescriptor(proto, "value") : null;
    const prev = el.value;
    try {{
      el.focus();
    }} catch (_) {{}}
    if (desc && typeof desc.set === "function") {{
      desc.set.call(el, val);
    }} else {{
      el.value = val;
    }}
    const tracker = el._valueTracker;
    if (tracker && typeof tracker.setValue === "function") {{
      tracker.setValue(prev);
    }}
    el.dispatchEvent(new win.Event("input", {{ bubbles: true, composed: true }}));
    el.dispatchEvent(new win.Event("change", {{ bubbles: true, composed: true }}));
  }}

  function triggerSubmit(targetEl) {{
    if (!targetEl || !isAllowedOrigin()) return;
    const doc = targetEl.ownerDocument || document;
    const win = (doc && doc.defaultView) || window;
    if (!win || !win.location || win.location.origin !== EXPECTED_ORIGIN) return;

    const form = targetEl.form || (targetEl.closest ? targetEl.closest("form") : null);
    if (form) {{
      if (!isSafeFormAction(form, null)) return;
      const explicitSubmitBtns = Array.from(
        form.querySelectorAll('button[type="submit"], input[type="submit"]')
      );
      const submitBtn = explicitSubmitBtns.find(
        (b) => !b.disabled && isVisible(b) && isSafeFormAction(form, b)
      );
      if (submitBtn) {{
        if (typeof form.requestSubmit === "function") {{
          try {{
            form.requestSubmit(submitBtn);
            return;
          }} catch (_) {{}}
        }}
        submitBtn.click();
        return;
      }}
      const loginBtns = Array.from(
        form.querySelectorAll('button, input[type="button"], [role="button"]')
      );
      const loginBtn = loginBtns.find(
        (b) => !b.disabled && isVisible(b) && isLoginButtonText(b) && isSafeFormAction(form, b)
      );
      if (loginBtn) {{
        loginBtn.click();
        return;
      }}
      if (typeof form.requestSubmit === "function") {{
        try {{
          form.requestSubmit();
          return;
        }} catch (_) {{}}
      }}
      const ev = new win.Event("submit", {{ bubbles: true, cancelable: true }});
      if (form.dispatchEvent(ev)) {{
        try {{
          form.submit();
        }} catch (_) {{}}
      }}
      return;
    }}

    let container = targetEl.parentElement;
    for (let depth = 0; container && depth < 5; depth++) {{
      const btns = Array.from(
        container.querySelectorAll(
          'button[type="submit"], input[type="submit"], button, input[type="button"], [role="button"]'
        )
      );
      const match = btns.find((b) => {{
        if (b.disabled || !isVisible(b)) return false;
        const tag = String(b.tagName || "").toUpperCase();
        const typeAttr = String((b.getAttribute && b.getAttribute("type")) || "").toLowerCase();
        if ((tag === "BUTTON" || tag === "INPUT") && typeAttr === "submit") {{
          return isSafeFormAction(null, b);
        }}
        return isLoginButtonText(b) && isSafeFormAction(null, b);
      }});
      if (match) {{
        match.click();
        return;
      }}
      container = container.parentElement;
    }}

    try {{
      targetEl.focus();
    }} catch (_) {{}}
    for (const type of ["keydown", "keypress", "keyup"]) {{
      targetEl.dispatchEvent(
        new win.KeyboardEvent(type, {{
          key: "Enter",
          code: "Enter",
          keyCode: 13,
          which: 13,
          bubbles: true,
          cancelable: true,
          composed: true,
        }})
      );
    }}
  }}

  function attempt(remaining) {{
    if (!isAllowedOrigin()) {{
      return;
    }}
    const allInputs = [];
    collectInputs(document, allInputs, new Set(), 0);
    const usable = allInputs.filter(isUsableInput);

    const pwCandidates = usable.filter(
      (el) => String(el.type || "").toLowerCase() === "password"
    );
    const safePwCandidates = pwCandidates.filter((el) => !isNonLoginPasswordField(el));
    const pwInput =
      safePwCandidates.find((el) => hasAutocompleteToken(el, "current-password")) ||
      safePwCandidates[0] ||
      null;

    let userInput = null;
    if (pwInput) {{
      const pwIndex = usable.indexOf(pwInput);
      const pwForm = pwInput.form || (pwInput.closest ? pwInput.closest("form") : null);
      const sameDoc = (el) => el.ownerDocument === pwInput.ownerDocument;
      const sameForm = (el) => {{
        if (!pwForm) return true;
        const f = el.form || (el.closest ? el.closest("form") : null);
        return f === pwForm;
      }};

      const pickBest = (list, preferClosest) => {{
        let best = null;
        let bestScore = -999;
        for (let i = 0; i < list.length; i++) {{
          const el = list[i];
          const s = usernameScore(el);
          if (s < 0) continue;
          if (best === null || s > bestScore || (preferClosest && s === bestScore)) {{
            best = el;
            bestScore = s;
          }}
        }}
        return best;
      }};

      const precedingSameForm = usable
        .slice(0, pwIndex)
        .filter((el) => isUsernameType(el) && sameDoc(el) && sameForm(el));
      userInput = pickBest(precedingSameForm, true);

      if (!userInput && pwForm) {{
        const siblingSameForm = usable.filter(
          (el) => el !== pwInput && isUsernameType(el) && sameDoc(el) && sameForm(el)
        );
        userInput = pickBest(siblingSameForm, false);
      }}

      if (!userInput) {{
        const precedingAny = usable
          .slice(0, pwIndex)
          .filter((el) => isUsernameType(el) && sameDoc(el));
        userInput = pickBest(precedingAny, true);
      }}
    }} else {{
      const candidates = usable.filter((el) => isUsernameType(el));
      const requireStrict = REQUIRE_LOGIN_SIGNAL || SHOULD_SUBMIT;
      let best = null;
      let bestScore = requireStrict ? 40 : 1;
      for (let i = 0; i < candidates.length; i++) {{
        const el = candidates[i];
        if (requireStrict && !hasStrictLoginSignal(el)) continue;
        const s = usernameScore(el);
        if (s >= bestScore) {{
          best = el;
          bestScore = s;
        }}
      }}
      userInput = best;
    }}

    if (!pwInput && !userInput) {{
      if (remaining > 1) {{
        setTimeout(() => attempt(remaining - 1), RETRY_DELAY_MS);
      }}
      return;
    }}

    if (!isAllowedOrigin()) {{
      return;
    }}

    let filledAny = false;
    if (userInput && USERNAME) {{
      if (!ONLY_IF_EMPTY || !userInput.value) {{
        setNativeValue(userInput, USERNAME);
        filledAny = true;
      }}
    }}
    if (pwInput && PASSWORD) {{
      if (!ONLY_IF_EMPTY || !pwInput.value) {{
        setNativeValue(pwInput, PASSWORD);
        filledAny = true;
      }}
    }}

    if (SHOULD_SUBMIT && filledAny) {{
      const submitTarget = pwInput || userInput;
      setTimeout(() => {{
        if (!isAllowedOrigin()) return;
        triggerSubmit(submitTarget);
      }}, 60);
    }}
  }}

  attempt(MAX_ATTEMPTS);
}})();"#
    ))
}

/// Build a safe, self-contained JS script for explicit user-triggered autofill (`webapp_account_fill`).
pub fn build_autofill_script(
    username: &str,
    password: &str,
    expected_origin: &str,
    submit: bool,
) -> Result<String, String> {
    let canonical_origin = extract_canonical_origin(expected_origin)?;
    build_autofill_script_inner(
        username,
        password,
        AutofillScriptOptions {
            submit,
            only_if_empty: false,
            require_login_signal: false,
            expected_origin: Some(&canonical_origin),
            max_attempts: 4,
            retry_delay_ms: 200,
        },
    )
}

/// Build a conservative JS autofill script for automatic page-load fill:
/// never submits automatically, preserves non-empty fields, requires explicit login signals if no
/// password field is present, checks `window.location.origin`, and retries briefly for SPA mounts.
pub fn build_page_load_autofill_script(
    username: &str,
    password: &str,
    expected_origin: &str,
) -> Result<String, String> {
    let canonical_origin = extract_canonical_origin(expected_origin)?;
    build_autofill_script_inner(
        username,
        password,
        AutofillScriptOptions {
            submit: false,
            only_if_empty: true,
            require_login_signal: true,
            expected_origin: Some(&canonical_origin),
            max_attempts: 4,
            retry_delay_ms: 300,
        },
    )
}

/// Enable native WebView2 password autosave and general autofill on a Windows child webview.
#[cfg(windows)]
pub fn enable_webview2_password_autosave<R: tauri::Runtime>(wv: &tauri::Webview<R>) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings4;
    use windows_core::Interface;

    let _ = wv.with_webview(|platform_wv| unsafe {
        let controller = platform_wv.controller();
        if let Ok(core_wv) = controller.CoreWebView2() {
            if let Ok(settings) = core_wv.Settings() {
                if let Ok(settings4) = settings.cast::<ICoreWebView2Settings4>() {
                    let _ = settings4.SetIsPasswordAutosaveEnabled(true);
                    let _ = settings4.SetIsGeneralAutofillEnabled(true);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store_path(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = NEXT_ACCOUNT_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("teza_webapp_accounts_{tag}_{stamp}_{seq}"))
            .join("webapp_accounts.json")
    }

    fn cleanup_store_path(path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn dpapi_seal_unseal_roundtrip_and_never_plaintext_on_disk() {
        let path = temp_store_path("dpapi");
        let secret = "S3cr3t!Passw0rd#Teza_2026";
        {
            let mut store = WebAppAccountStore::load(path.clone());
            let dto = store
                .save(WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "10.7.12.233:5093/login".into(),
                    label: "ERP Admin".into(),
                    username: "admin@corp.local".into(),
                    password: Some(secret.into()),
                    clear_password: None,
                    auto_fill: true,
                })
                .unwrap();
            assert_eq!(dto.origin, "http://10.7.12.233:5093");
            assert!(dto.has_password);
            assert!(dto.auto_fill);
        }

        let disk_raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !disk_raw.contains(secret),
            "plaintext password must NEVER appear on disk"
        );
        #[cfg(target_os = "windows")]
        assert!(
            disk_raw.contains("dpapi:v1:"),
            "Windows storage must seal password with DPAPI prefix"
        );

        let reloaded = WebAppAccountStore::load(path.clone());
        let list = reloaded.list(Some("erp"), None);
        assert_eq!(list.len(), 1);
        assert!(list[0].has_password);

        let creds = reloaded.resolve_credentials(&list[0].id).unwrap();
        assert_eq!(creds.username, "admin@corp.local");
        assert_eq!(creds.password, secret);

        cleanup_store_path(&path);
    }

    #[test]
    fn debug_formatting_never_leaks_passwords() {
        let secret = "UltraSecretPassword_987654!";
        let input = WebAppAccountInput {
            id: None,
            app_id: "crm".into(),
            url: "https://crm.example.com/signin".into(),
            label: "CRM".into(),
            username: "alice".into(),
            password: Some(secret.into()),
            clear_password: None,
            auto_fill: false,
        };
        let input_dbg = format!("{input:?}");
        assert!(!input_dbg.contains(secret));
        assert!(input_dbg.contains("[REDACTED]"));

        let path = temp_store_path("debug");
        let mut store = WebAppAccountStore::load(path.clone());
        let dto = store.save(input).unwrap();
        let creds = store.resolve_credentials(&dto.id).unwrap();

        let dto_dbg = format!("{dto:?}");
        let creds_dbg = format!("{creds:?}");
        let store_dbg = format!("{store:?}");
        let stored_dbg = format!("{:?}", store.accounts[0]);

        assert!(!dto_dbg.contains(secret));
        assert!(!creds_dbg.contains(secret));
        assert!(creds_dbg.contains("[REDACTED]"));
        assert!(!store_dbg.contains(secret));
        assert!(!stored_dbg.contains(secret));
        assert!(!stored_dbg.contains(&store.accounts[0].sealed_password));

        cleanup_store_path(&path);
    }

    #[test]
    fn crud_preserve_password_on_edit_and_single_autofill_per_origin() {
        let path = temp_store_path("crud");
        let mut store = WebAppAccountStore::load(path.clone());

        let acc1 = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "http://10.7.12.233:5093/login".into(),
                    label: "".into(),
                    username: "user1".into(),
                    password: Some("pass_one".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                1000,
            )
            .unwrap();
        assert_eq!(acc1.label, "user1");
        assert!(acc1.has_password);
        assert!(acc1.auto_fill);

        // Saving a second auto_fill account on same (app_id, origin) clears the first
        let acc2 = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "http://10.7.12.233:5093/other-page".into(),
                    label: "Backup".into(),
                    username: "user2".into(),
                    password: Some("pass_two".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                2000,
            )
            .unwrap();
        assert!(acc2.auto_fill);

        // Another app on same origin keeps its own independent auto_fill
        let acc_other_app = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp-reports".into(),
                    url: "http://10.7.12.233:5093/reports".into(),
                    label: "Reports".into(),
                    username: "reporter".into(),
                    password: Some("pass_three".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                1500,
            )
            .unwrap();
        assert!(acc_other_app.auto_fill);

        let erp_list = store.list(Some("erp"), None);
        assert_eq!(erp_list.len(), 2);
        // Sorted by updated_at_ms desc
        assert_eq!(erp_list[0].id, acc2.id);
        assert!(erp_list[0].auto_fill);
        assert_eq!(erp_list[1].id, acc1.id);
        assert!(!erp_list[1].auto_fill);

        // Updating acc1 with password=None preserves existing password
        let acc1_edited = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc1.id.clone()),
                    app_id: "erp".into(),
                    url: "http://10.7.12.233:5093/".into(),
                    label: "Primary Renamed".into(),
                    username: "user1_new".into(),
                    password: None,
                    clear_password: None,
                    auto_fill: false,
                },
                3000,
            )
            .unwrap();
        assert!(acc1_edited.has_password);
        assert_eq!(acc1_edited.label, "Primary Renamed");
        assert_eq!(store.resolve_credentials(&acc1.id).unwrap().password, "pass_one");

        // Updating with empty password and clear_password != Some(true) also preserves password
        let acc1_edited2 = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc1.id.clone()),
                    app_id: "erp".into(),
                    url: "http://10.7.12.233:5093/".into(),
                    label: "Primary Renamed 2".into(),
                    username: "user1_new".into(),
                    password: Some(String::new()),
                    clear_password: Some(false),
                    auto_fill: false,
                },
                3500,
            )
            .unwrap();
        assert!(acc1_edited2.has_password);
        assert_eq!(store.resolve_credentials(&acc1.id).unwrap().password, "pass_one");

        // Clearing password explicitly wipes it
        let acc1_cleared = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc1.id.clone()),
                    app_id: "erp".into(),
                    url: "http://10.7.12.233:5093/".into(),
                    label: "Primary Cleared".into(),
                    username: "user1_new".into(),
                    password: None,
                    clear_password: Some(true),
                    auto_fill: false,
                },
                4000,
            )
            .unwrap();
        assert!(!acc1_cleared.has_password);
        assert_eq!(store.resolve_credentials(&acc1.id).unwrap().password, "");

        // resolve_autofill_for_page finds acc2 for erp and acc_other_app for erp-reports
        assert_eq!(
            store
                .resolve_autofill_for_page("erp", "http://10.7.12.233:5093")
                .unwrap()
                .unwrap()
                .id,
            acc2.id
        );
        assert_eq!(
            store
                .resolve_autofill_for_page("erp-reports", "http://10.7.12.233:5093")
                .unwrap()
                .unwrap()
                .id,
            acc_other_app.id
        );

        // Origin filter normalizes full URLs
        let by_origin = store.list(None, Some("http://10.7.12.233:5093/some/path"));
        assert_eq!(by_origin.len(), 3);

        // Delete removes account cleanly
        store.delete(&acc1.id).unwrap();
        assert_eq!(store.list(Some("erp"), None).len(), 1);
        assert!(store.resolve_credentials(&acc1.id).is_err());

        cleanup_store_path(&path);
    }

    #[test]
    fn canonical_origin_and_origin_matching_security_checks() {
        assert_eq!(
            extract_canonical_origin("https://Example.COM:443/login?a=1#frag").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            extract_canonical_origin("http://10.7.12.233:80/index.html").unwrap(),
            "http://10.7.12.233"
        );
        assert_eq!(
            extract_canonical_origin("10.7.12.233:5093/summary.html").unwrap(),
            "http://10.7.12.233:5093"
        );
        assert_eq!(
            extract_canonical_origin("notion.so/login").unwrap(),
            "https://notion.so"
        );
        assert_eq!(
            extract_canonical_origin("[::1]:8080/api").unwrap(),
            "http://[::1]:8080"
        );
        assert!(extract_canonical_origin("javascript:alert(1)").is_err());
        assert!(extract_canonical_origin("file:///C:/Windows/win.ini").is_err());

        let same_origin_url = tauri::Url::parse("https://app.example.com/dashboard?tab=2").unwrap();
        assert!(verify_origin_matches("https://app.example.com", &same_origin_url).is_ok());

        let diff_scheme = tauri::Url::parse("http://app.example.com/login").unwrap();
        assert!(verify_origin_matches("https://app.example.com", &diff_scheme).is_err());

        let diff_port = tauri::Url::parse("https://app.example.com:8443/login").unwrap();
        assert!(verify_origin_matches("https://app.example.com", &diff_port).is_err());

        let diff_host = tauri::Url::parse("https://evil.example.com/login").unwrap();
        let err = verify_origin_matches("https://app.example.com", &diff_host).unwrap_err();
        assert!(err.contains("https://evil.example.com"));
        assert!(err.contains("https://app.example.com"));
    }

    #[test]
    fn autofill_script_safely_escapes_quotes_newlines_and_script_tags() {
        let tricky_user = "user\"'\\`;\n\r\t</script><img src=x onerror=alert(1)>";
        let tricky_pass = "p@ss\"'\\\n\u{2028}\u{2029}</SCRIPT>alert('xss')";

        let script =
            build_autofill_script(tricky_user, tricky_pass, "https://app.example.com/login", true)
                .unwrap();
        assert!(!script.contains("</script>"));
        assert!(!script.contains("</SCRIPT>"));
        assert!(!script.contains('\u{2028}'));
        assert!(!script.contains('\u{2029}'));
        assert!(script.contains("const SHOULD_SUBMIT = true;"));
        assert!(script.contains("const EXPECTED_ORIGIN = \"https://app.example.com\";"));
        assert!(script.contains("const MAX_ATTEMPTS = 4;"));
        assert!(script.contains("const RETRY_DELAY_MS = 200;"));
        assert!(script.contains("HTMLInputElement.prototype"));
        assert!(script.contains("dispatchEvent"));
        assert!(script.contains("isSafeFormAction"));
        assert!(script.contains("isAllowedOrigin"));
        assert!(script.contains("frameLoc.origin !== EXPECTED_ORIGIN"));
        assert!(!script.contains("button:not([type])"));
        assert!(!script.contains("pwCandidates[0]"));
        assert!(script.contains("\\u003c/script\\u003e"));
        assert!(script.contains("\\u003c/SCRIPT\\u003e"));

        let page_load_script =
            build_page_load_autofill_script("alice", "pw", "https://app.example.com").unwrap();
        assert!(page_load_script.contains("const SHOULD_SUBMIT = false;"));
        assert!(page_load_script.contains("const ONLY_IF_EMPTY = true;"));
        assert!(page_load_script.contains("const REQUIRE_LOGIN_SIGNAL = true;"));
        assert!(page_load_script.contains("const EXPECTED_ORIGIN = \"https://app.example.com\";"));
    }

    #[test]
    fn validate_web_url_hardening_and_internal_webview_denylist() {
        // Forbidden Tauri internal / dev origins
        for forbidden in [
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
            "http://sub.tauri.localhost/",
            "http://tauri.localhost./",
            "http://ipc.localhost/invoke",
            "http://asset.localhost/logo.png",
            "http://localhost:1420/",
            "https://localhost:1420/app",
            "http://127.0.0.1:1420/",
            "http://[::1]:1420/",
            "http://[::ffff:127.0.0.1]:1420/",
            "tauri://localhost",
            "file:///C:/Windows/win.ini",
            "javascript:alert(1)",
            "about:blank",
        ] {
            let parsed = tauri::Url::parse(forbidden).unwrap();
            assert!(
                is_forbidden_internal_webview_url(&parsed),
                "expected forbidden: {forbidden}"
            );
            assert!(
                validate_web_url(forbidden).is_err(),
                "validate_web_url must reject: {forbidden}"
            );
        }

        assert!(validate_web_url("tauri.localhost").is_err());
        assert!(validate_web_url("ipc.localhost").is_err());
        assert!(validate_web_url("asset.localhost").is_err());
        assert!(validate_web_url("localhost:1420").is_err());
        assert!(validate_web_url("127.0.0.1:1420").is_err());

        // Control chars, max length, and userinfo
        assert!(validate_web_url("https://example.com\n/login").is_err());
        assert!(validate_web_url("https://ex\tample.com").is_err());
        assert!(validate_web_url("https://example.com\r").is_err());
        assert!(validate_web_url("https://example.com/\x00a").is_err());
        let too_long = format!("https://example.com/{}", "a".repeat(4100));
        assert!(validate_web_url(&too_long).is_err());
        assert!(validate_web_url("https://user:pass@example.com/login").is_err());
        assert!(validate_web_url("https://user@example.com/login").is_err());
        assert!(validate_web_url("user@example.com/login").is_err());

        // Bare URL with `://` in query string
        assert_eq!(
            validate_web_url("example.com/login?next=https://other.com")
                .unwrap()
                .as_str(),
            "https://example.com/login?next=https://other.com"
        );
        assert_eq!(
            validate_web_url("10.7.12.233:5093/login?next=https://other.com")
                .unwrap()
                .as_str(),
            "http://10.7.12.233:5093/login?next=https://other.com"
        );

        // Trailing dot and IPv4-mapped IPv6 LAN detection
        assert_eq!(
            validate_web_url("localhost.:3000/app").unwrap().scheme(),
            "http"
        );
        assert_eq!(
            validate_web_url("nas.local./files").unwrap().scheme(),
            "http"
        );
        assert_eq!(
            validate_web_url("[::ffff:10.7.12.233]:5093/login")
                .unwrap()
                .scheme(),
            "http"
        );
        assert_eq!(
            validate_web_url("[::ffff:8.8.8.8]:8443/login")
                .unwrap()
                .scheme(),
            "https"
        );
    }

    #[test]
    fn origin_change_refuses_carrying_over_existing_password_and_persists_transactionally() {
        let path = temp_store_path("origin_change");
        let mut store = WebAppAccountStore::load(path.clone());

        let acc = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "https://erp-a.example.com/login".into(),
                    label: "ERP A".into(),
                    username: "alice".into(),
                    password: Some("secret-a".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                1000,
            )
            .unwrap();

        // Changing origin without a new password or clear_password must fail and leave account intact
        let err = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc.id.clone()),
                    app_id: "erp".into(),
                    url: "https://erp-b.example.com/login".into(),
                    label: "ERP B".into(),
                    username: "alice".into(),
                    password: None,
                    clear_password: None,
                    auto_fill: true,
                },
                2000,
            )
            .unwrap_err();
        assert!(err.contains("origin"));
        let creds_still = store.resolve_credentials(&acc.id).unwrap();
        assert_eq!(creds_still.origin, "https://erp-a.example.com");
        assert_eq!(creds_still.password, "secret-a");

        // Changing origin WITH clear_password=true succeeds and clears password
        let cleared = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc.id.clone()),
                    app_id: "erp".into(),
                    url: "https://erp-b.example.com/login".into(),
                    label: "ERP B".into(),
                    username: "alice".into(),
                    password: None,
                    clear_password: Some(true),
                    auto_fill: true,
                },
                2500,
            )
            .unwrap();
        assert_eq!(cleared.origin, "https://erp-b.example.com");
        assert!(!cleared.has_password);

        // Changing origin WITH an explicit new password succeeds
        let rekeyed = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(acc.id.clone()),
                    app_id: "erp".into(),
                    url: "https://erp-c.example.com/login".into(),
                    label: "ERP C".into(),
                    username: "alice".into(),
                    password: Some("secret-c".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                3000,
            )
            .unwrap();
        assert_eq!(rekeyed.origin, "https://erp-c.example.com");
        assert!(rekeyed.has_password);
        assert_eq!(store.resolve_credentials(&acc.id).unwrap().password, "secret-c");

        // Transactional persistence: point store.path to an existing directory so write fails
        let unwritable_dir = path.parent().unwrap().join("blocked_dir");
        std::fs::create_dir_all(&unwritable_dir).unwrap();
        store.path = unwritable_dir;

        // Failed save must NOT mutate in-memory state or clear existing auto_fill
        assert!(store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "https://erp-c.example.com/other".into(),
                    label: "Should Fail".into(),
                    username: "bob".into(),
                    password: Some("pw".into()),
                    clear_password: None,
                    auto_fill: true,
                },
                4000,
            )
            .is_err());
        let rows = store.list(Some("erp"), None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, acc.id);
        assert!(rows[0].auto_fill);

        // Failed delete must NOT drop the account from memory
        assert!(store.delete(&acc.id).is_err());
        assert_eq!(store.list(Some("erp"), None).len(), 1);

        cleanup_store_path(&path);
    }

    #[test]
    fn validation_preserves_password_whitespace_and_handles_corrupt_store() {
        let path = temp_store_path("validation");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(&path, b"{not-valid-json").unwrap();

        let mut store = WebAppAccountStore::load(path.clone());
        assert!(store.list(None, None).is_empty());

        // Reject invalid inputs
        assert!(store
            .save(WebAppAccountInput {
                id: None,
                app_id: "   ".into(),
                url: "https://example.com".into(),
                label: "Test".into(),
                username: "alice".into(),
                password: None,
                clear_password: None,
                auto_fill: false,
            })
            .is_err());
        assert!(store
            .save(WebAppAccountInput {
                id: None,
                app_id: "erp".into(),
                url: "https://example.com".into(),
                label: "Test".into(),
                username: "   ".into(),
                password: None,
                clear_password: None,
                auto_fill: false,
            })
            .is_err());
        assert!(store
            .save(WebAppAccountInput {
                id: Some("missing-id".into()),
                app_id: "erp".into(),
                url: "https://example.com".into(),
                label: "Test".into(),
                username: "alice".into(),
                password: None,
                clear_password: None,
                auto_fill: false,
            })
            .is_err());

        // Password whitespace must be preserved verbatim (not trimmed)
        let spaced_pw = "  s3cr3t pass  ";
        let dto = store
            .save(WebAppAccountInput {
                id: None,
                app_id: "erp".into(),
                url: "https://example.com/login".into(),
                label: "Spaced".into(),
                username: "  alice  ".into(),
                password: Some(spaced_pw.into()),
                clear_password: None,
                auto_fill: false,
            })
            .unwrap();
        let creds = store.resolve_credentials(&dto.id).unwrap();
        assert_eq!(creds.username, "alice");
        assert_eq!(creds.password, spaced_pw);

        cleanup_store_path(&path);
    }

    #[test]
    fn enforces_max_webapp_accounts_cap_while_allowing_edits_and_deletes() {
        let path = temp_store_path("max_cap");
        let mut store = WebAppAccountStore::load(path.clone());
        let mut first_id = String::new();

        for i in 0..MAX_WEBAPP_ACCOUNTS {
            let dto = store
                .save_with_timestamp(
                    WebAppAccountInput {
                        id: None,
                        app_id: "erp".into(),
                        url: "https://erp.example.com/login".into(),
                        label: format!("User {i}"),
                        username: format!("user{i}@example.com"),
                        password: None,
                        clear_password: None,
                        auto_fill: false,
                    },
                    1000 + i as i64,
                )
                .unwrap();
            if i == 0 {
                first_id = dto.id;
            }
        }
        assert_eq!(store.list(None, None).len(), MAX_WEBAPP_ACCOUNTS);

        // Inserting one more account beyond MAX_WEBAPP_ACCOUNTS must fail cleanly
        let err = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "https://erp.example.com/login".into(),
                    label: "Overflow".into(),
                    username: "overflow@example.com".into(),
                    password: None,
                    clear_password: None,
                    auto_fill: false,
                },
                5000,
            )
            .unwrap_err();
        assert!(
            err.contains(&MAX_WEBAPP_ACCOUNTS.to_string()),
            "expected cap count in error, got: {err}"
        );
        assert_eq!(store.list(None, None).len(), MAX_WEBAPP_ACCOUNTS);

        // Updating an existing account while at capacity must still succeed
        let updated = store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: Some(first_id.clone()),
                    app_id: "erp".into(),
                    url: "https://erp.example.com/login".into(),
                    label: "User 0 Renamed".into(),
                    username: "user0@example.com".into(),
                    password: None,
                    clear_password: None,
                    auto_fill: false,
                },
                6000,
            )
            .unwrap();
        assert_eq!(updated.label, "User 0 Renamed");
        assert_eq!(store.list(None, None).len(), MAX_WEBAPP_ACCOUNTS);

        // Deleting one frees a slot for a new insert
        store.delete(&first_id).unwrap();
        assert_eq!(store.list(None, None).len(), MAX_WEBAPP_ACCOUNTS - 1);
        store
            .save_with_timestamp(
                WebAppAccountInput {
                    id: None,
                    app_id: "erp".into(),
                    url: "https://erp.example.com/login".into(),
                    label: "Replacement".into(),
                    username: "replacement@example.com".into(),
                    password: None,
                    clear_password: None,
                    auto_fill: false,
                },
                7000,
            )
            .unwrap();
        assert_eq!(store.list(None, None).len(), MAX_WEBAPP_ACCOUNTS);

        cleanup_store_path(&path);
    }

    #[test]
    fn corrupt_store_is_backed_up_and_preserved_across_subsequent_save() {
        let path = temp_store_path("corrupt_backup");
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();

        // Missing file must not create any .corrupt backup
        let missing_store = WebAppAccountStore::load(path.clone());
        assert!(missing_store.list(None, None).is_empty());
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);

        // Corrupt JSON must be backed up to `<path>.corrupt.<epoch_ms>`
        let corrupt_bytes = b"{\"version\":1,\"accounts\":[{\"id\":\"broken";
        std::fs::write(&path, corrupt_bytes).unwrap();
        let mut store = WebAppAccountStore::load(path.clone());
        assert!(store.list(None, None).is_empty());

        let backups: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("webapp_accounts.json.corrupt."))
            })
            .collect();
        assert_eq!(backups.len(), 1, "expected 1 corrupt backup, found {backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), corrupt_bytes);

        // Subsequent save creates a fresh store without overwriting the corrupt backup
        store
            .save(WebAppAccountInput {
                id: None,
                app_id: "erp".into(),
                url: "https://erp.example.com/login".into(),
                label: "Fresh".into(),
                username: "fresh@example.com".into(),
                password: None,
                clear_password: None,
                auto_fill: false,
            })
            .unwrap();
        assert_eq!(std::fs::read(&backups[0]).unwrap(), corrupt_bytes);
        assert_eq!(WebAppAccountStore::load(path.clone()).list(None, None).len(), 1);

        // Invalid UTF-8 binary corruption must also be backed up
        let invalid_utf8 = [0xFFu8, 0xFE, 0x00, 0x80, 0xC3, 0x28];
        std::fs::write(&path, invalid_utf8).unwrap();
        let reloaded = WebAppAccountStore::load(path.clone());
        assert!(reloaded.list(None, None).is_empty());
        let backups_after: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("webapp_accounts.json.corrupt."))
            })
            .collect();
        assert_eq!(backups_after.len(), 2);
        assert!(backups_after
            .iter()
            .any(|p| std::fs::read(p).ok().as_deref() == Some(&invalid_utf8[..])));

        cleanup_store_path(&path);
    }

    #[test]
    fn resolved_credentials_drop_zeroizes_password_bytes() {
        assert!(std::mem::needs_drop::<ResolvedWebAppCredentials>());

        let mut creds = ResolvedWebAppCredentials {
            id: "wacc_1".into(),
            app_id: "erp".into(),
            origin: "https://erp.example.com".into(),
            username: "alice".into(),
            password: String::from("TopSecretPassword!2026"),
        };
        let original_len = creds.password.len();
        creds.zeroize_password();
        assert_eq!(creds.password.len(), original_len);
        assert!(
            creds.password.as_bytes().iter().all(|&b| b == 0),
            "expected all password bytes to be zeroized"
        );
        // Dropping after zeroize (and dropping an un-zeroized instance) must succeed cleanly
        drop(creds);

        let fresh = ResolvedWebAppCredentials {
            id: "wacc_2".into(),
            app_id: "erp".into(),
            origin: "https://erp.example.com".into(),
            username: "bob".into(),
            password: String::from("AnotherSecret"),
        };
        drop(fresh);
    }
}
