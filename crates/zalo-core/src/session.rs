//! HTTP session management, DPAPI-sealed persistence, and signed/encrypted request path.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, COOKIE, ORIGIN, REFERER};
use reqwest::multipart::{Form, Part};
use reqwest::{redirect, Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::crypto;
use crate::data_dir;
use crate::dpapi;
use crate::endpoints;
use crate::param_cipher::ParamCipher;
use crate::relay::RelayConfig;

pub const ZPW_VER: &str = "694";
pub const ZPW_TYPE: &str = "30";

/// Returns `true` when `error_code` indicates an expired or invalid `zpw_sek` session key.
pub fn is_session_expired_code(code: i64) -> bool {
    matches!(code, 600 | -102 | 102)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Valid,
    Expired { code: i64 },
    Unreachable,
}

/// Returns `true` only for official domains (`*.zalo.me`, `*.zaloapp.com`).
pub fn is_allowed_zalo_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    h == "zalo.me"
        || h.ends_with(".zalo.me")
        || h == "zaloapp.com"
        || h.ends_with(".zaloapp.com")
}

/// Official Zalo / VNG media & CDN host suffixes (kept synchronized with `litechat-relay`).
pub const ALLOWED_MEDIA_HOST_SUFFIXES: &[&str] = &[
    "zalo.me",
    "zaloapp.com",
    "zdn.vn",
    "zadn.vn",
    "zalo.vn",
    "zalo.com",
    "zalo.cloud",
    "zbox.vn",
    "zalo.video",
    "zapps.me",
    "zaloplatforms.com",
    "zing.vn",
    "dlfl.vn",
    "dlfl.me",
    "flchat.vn",
    "flchat.me",
    "dlmd.vn",
    "dlmd.me",
    "mdchat.vn",
    "mdchat.me",
];

fn host_matches_suffix(normalized_host: &str, suffix: &str) -> bool {
    normalized_host == suffix
        || normalized_host
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.'))
}

/// Returns `true` for trusted official Zalo/VNG media & CDN hosts.
pub fn is_allowed_media_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    ALLOWED_MEDIA_HOST_SUFFIXES
        .iter()
        .any(|suffix| host_matches_suffix(&h, suffix))
}

fn sanitize_url_origin_for_log(url: &reqwest::Url) -> String {
    match url.host_str() {
        Some(host) if !host.is_empty() => match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        },
        _ => format!("{}:", url.scheme()),
    }
}

fn scrub_urls_in_text(input: &str) -> String {
    const SCHEMES: &[&str] = &[
        "https://",
        "http://",
        "socks5h://",
        "socks5://",
        "socks4a://",
        "socks4://",
        "wss://",
        "ws://",
    ];
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        let next_match = SCHEMES
            .iter()
            .filter_map(|scheme| lower.find(scheme))
            .min();
        let Some(idx) = next_match else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let end = after
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | ',' | ';')
            })
            .unwrap_or(after.len());
        let token = &after[..end];
        if let Ok(parsed) = reqwest::Url::parse(token) {
            out.push_str(&sanitize_url_origin_for_log(&parsed));
        } else {
            out.push_str("[redacted-url]");
        }
        rest = &after[end..];
    }
    out
}

fn scrub_sensitive_text(raw: &str, relay: &RelayConfig) -> String {
    let mut cleaned = scrub_urls_in_text(raw);
    let user = relay.username.trim();
    if !user.is_empty() && !relay.password.is_empty() {
        let pair = format!("{user}:{}", relay.password);
        cleaned = cleaned.replace(&pair, "[REDACTED]");
        let encoded_pair = format!("{}:{}", urlencode(user), urlencode(&relay.password));
        if encoded_pair != pair {
            cleaned = cleaned.replace(&encoded_pair, "[REDACTED]");
        }
    }
    if relay.password.len() >= 4 {
        cleaned = cleaned.replace(&relay.password, "[REDACTED]");
        let encoded_pw = urlencode(&relay.password);
        if encoded_pw != relay.password {
            cleaned = cleaned.replace(&encoded_pw, "[REDACTED]");
        }
    }
    cleaned
}

fn collect_sanitized_error_chain(err: &dyn std::error::Error, relay: &RelayConfig) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<&dyn std::error::Error> = Some(err);
    let mut depth = 0usize;
    while let Some(node) = current {
        if depth >= 16 {
            break;
        }
        let raw = node.to_string();
        let trimmed = raw.trim();
        let skip_outer_url_wrapper = depth == 0
            && node.source().is_some()
            && trimmed
                .to_ascii_lowercase()
                .starts_with("error sending request for url");
        if !skip_outer_url_wrapper && !trimmed.is_empty() {
            let cleaned = scrub_sensitive_text(trimmed, relay);
            if parts.last().is_none_or(|prev| prev != &cleaned) {
                parts.push(cleaned);
            }
        }
        current = node.source();
        depth += 1;
    }
    if parts.is_empty() {
        let fallback = scrub_sensitive_text(err.to_string().trim(), relay);
        if fallback.is_empty() {
            "unknown network error".to_string()
        } else {
            fallback
        }
    } else {
        parts.join(": ")
    }
}

fn is_relay_auth_error(chain_lower: &str) -> bool {
    chain_lower.contains("407")
        || chain_lower.contains("proxy authorization required")
        || chain_lower.contains("proxy authentication required")
        || chain_lower.contains("credentials not accepted")
        || chain_lower.contains("does not support user/pass")
        || chain_lower.contains("authentication failed")
}

fn is_relay_policy_block(chain_lower: &str) -> bool {
    if is_relay_auth_error(chain_lower) {
        return false;
    }
    chain_lower.contains("not allowed by ruleset")
        || chain_lower.contains("connection not allowed")
        || chain_lower.contains("0x02")
        || chain_lower.contains("403")
        || chain_lower.contains("forbidden")
        || chain_lower.contains("unsuccessful")
}

fn should_retry_media_send_error(relay: &RelayConfig, err: &reqwest::Error) -> bool {
    if err.is_builder() {
        return false;
    }
    let chain_lower = collect_sanitized_error_chain(err, relay).to_ascii_lowercase();
    if is_relay_policy_block(&chain_lower) || is_relay_auth_error(&chain_lower) {
        return false;
    }
    if relay.enabled {
        return true;
    }
    err.is_connect()
        || chain_lower.contains("reset")
        || chain_lower.contains("eof")
        || chain_lower.contains("closed")
        || chain_lower.contains("incomplete message")
        || chain_lower.contains("broken pipe")
        || chain_lower.contains("aborted")
}

fn format_media_network_error(
    url: &reqwest::Url,
    relay: &RelayConfig,
    err: &dyn std::error::Error,
) -> String {
    let origin = sanitize_url_origin_for_log(url);
    let chain = collect_sanitized_error_chain(err, relay);
    let chain_lower = chain.to_ascii_lowercase();
    if relay.enabled && is_relay_policy_block(&chain_lower) {
        let relay_host = relay.host.trim();
        let relay_port = relay.port;
        let host_only = url.host_str().unwrap_or(&origin);
        format!(
            "Máy chủ Relay ({relay_host}:{relay_port}) từ chối kết nối tới {origin} ({chain}). Hãy cập nhật litechat-relay lên bản mới nhất hoặc thêm --extra-host {host_only}"
        )
    } else {
        format!("Không tải được từ {origin} ({chain})")
    }
}

/// Canonicalize a candidate media URL to HTTPS and validate against `is_allowed_media_host`.
///
/// - Trims whitespace, unescapes `\/` -> `/`, and expands protocol-relative `//...` to `https://...`.
/// - Accepts `https` (default/443) and upgrades `http` (default/80) when the host is in
///   `ALLOWED_MEDIA_HOST_SUFFIXES`.
/// - Rejects userinfo, non-standard ports, and untrusted hosts, returning a privacy-safe
///   origin-only diagnostic (`scheme://host[:port]` or generic parse reason — never paths/queries).
pub fn normalize_allowed_media_url(raw: &str) -> std::result::Result<reqwest::Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("URL trống".to_string());
    }
    let unescaped: std::borrow::Cow<'_, str> = if trimmed.contains(r"\/") {
        std::borrow::Cow::Owned(trimmed.replace(r"\/", "/"))
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    let candidate: std::borrow::Cow<'_, str> = if unescaped.starts_with("//") {
        std::borrow::Cow::Owned(format!("https:{unescaped}"))
    } else {
        unescaped
    };
    let Ok(mut url) = reqwest::Url::parse(&candidate) else {
        return Err("URL không hợp lệ".to_string());
    };
    let origin = sanitize_url_origin_for_log(&url);
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{origin} (chứa thông tin xác thực)"));
    }
    match url.scheme() {
        "https" => {
            if let Some(port) = url.port() {
                if port != 443 {
                    return Err(origin);
                }
            }
            if !url.host_str().is_some_and(is_allowed_media_host) {
                return Err(origin);
            }
            Ok(url)
        }
        "http" => {
            if let Some(port) = url.port() {
                if port != 80 {
                    return Err(origin);
                }
            }
            if !url.host_str().is_some_and(is_allowed_media_host) {
                return Err(origin);
            }
            let _ = url.set_port(None);
            if url.set_scheme("https").is_err() {
                return Err(origin);
            }
            Ok(url)
        }
        _ => Err(origin),
    }
}

fn is_allowed_url_with(raw: &str, expected_scheme: &str, host_check: fn(&str) -> bool) -> bool {
    let Ok(url) = reqwest::Url::parse(raw.trim()) else {
        return false;
    };
    if url.scheme() != expected_scheme {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if let Some(port) = url.port() {
        if port != 443 {
            return false;
        }
    }
    url.host_str().is_some_and(host_check)
}

pub fn is_allowed_zalo_http_url(raw: &str) -> bool {
    is_allowed_url_with(raw, "https", is_allowed_zalo_host)
}

pub fn is_allowed_media_url(raw: &str) -> bool {
    is_allowed_url_with(raw, "https", is_allowed_media_host)
}

pub fn is_allowed_zalo_ws_url(raw: &str) -> bool {
    is_allowed_url_with(raw, "wss", is_allowed_zalo_host)
}

/// Decode a `data:<mime>;base64,<payload>` URL with a strict size limit.
pub fn decode_data_url_bytes(raw: &str, max_bytes: usize) -> Result<(Vec<u8>, String)> {
    let trimmed = raw.trim();
    let rest = if trimmed.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
        &trimmed[5..]
    } else {
        anyhow::bail!("URL data không hợp lệ");
    };
    let (meta, payload) = rest
        .split_once(',')
        .context("Định dạng data URL không hợp lệ")?;
    if !meta.to_ascii_lowercase().ends_with(";base64") {
        anyhow::bail!("Chỉ hỗ trợ data URL dạng base64");
    }
    let mime = meta
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();
    let b64_payload = payload.trim();
    let max_b64_len = max_bytes.div_ceil(3).saturating_mul(4);
    if b64_payload.len() > max_b64_len {
        anyhow::bail!("Dữ liệu vượt quá giới hạn cho phép");
    }
    let bytes = B64
        .decode(b64_payload)
        .context("Không giải mã được dữ liệu base64")?;
    if bytes.len() > max_bytes {
        anyhow::bail!("Dữ liệu vượt quá giới hạn cho phép");
    }
    Ok((bytes, mime))
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Phiên đăng nhập đã hết hạn trên máy chủ (mã {code})")]
    Expired { code: i64 },
    #[error("Lỗi kết nối mạng: {0}")]
    Network(String),
    #[error("Máy chủ trả về HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("Lỗi giao thức ({code}): {message}")]
    Protocol { code: i64, message: String },
}

#[derive(Debug, Error)]
pub enum MediaDownloadError {
    #[error("Liên kết media đã hết hạn hoặc không còn tồn tại trên CDN (HTTP {status})")]
    CdnExpired { status: u16 },
    #[error("Tải tệp thất bại (HTTP {status})")]
    HttpStatus { status: u16 },
    #[error("Không hỗ trợ tải tệp SVG vì lý do bảo mật")]
    ForbiddenSvg,
    #[error("Từ chối tải tệp từ URL không nằm trong danh sách CDN tin cậy ({origin})")]
    UntrustedUrl { origin: String },
}

impl MediaDownloadError {
    pub fn is_cdn_expired(&self) -> bool {
        matches!(self, Self::CdnExpired { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Domains {
    pub auth: String,
    pub wpa_chat: String,
    pub chat: String,
    pub conversation: String,
    #[serde(default = "default_lp_chat")]
    pub lp_chat: String,
    #[serde(default = "default_lp_control")]
    pub lp_control: String,
    pub friend: String,
    pub profile: String,
    pub group: String,
    pub file: String,
    #[serde(default = "default_cloud_msg")]
    pub cloud_msg: String,
    #[serde(default = "default_reaction")]
    pub reaction: String,
    pub sticker: String,
    #[serde(default = "default_voice_call")]
    pub voice_call: String,
    pub ws: Vec<String>,
    #[serde(skip)]
    pub service_map_candidates: BTreeMap<String, Vec<String>>,
}

fn default_lp_chat() -> String {
    endpoints::MSG_CHAT.into()
}

fn default_lp_control() -> String {
    endpoints::CONVERS_CTL.into()
}

fn default_cloud_msg() -> String {
    endpoints::service("group_cloud_message")
        .unwrap_or(endpoints::CLOUD_MSG_FALLBACK)
        .into()
}

fn default_reaction() -> String {
    endpoints::service("reaction")
        .unwrap_or(endpoints::REACTION_FALLBACK)
        .into()
}

fn default_voice_call() -> String {
    endpoints::service("voice_call")
        .unwrap_or(endpoints::VOICE_CALL_FALLBACK)
        .into()
}

impl Default for Domains {
    fn default() -> Self {
        Self {
            auth: endpoints::AUTH.into(),
            wpa_chat: endpoints::WPA_CHAT.into(),
            chat: endpoints::service("chat").unwrap_or(endpoints::MSG_CHAT).into(),
            conversation: endpoints::service("conversation")
                .unwrap_or(endpoints::CONVERS_CTL)
                .into(),
            lp_chat: default_lp_chat(),
            lp_control: default_lp_control(),
            friend: endpoints::service("friend").unwrap_or_default().into(),
            profile: endpoints::service("profile").unwrap_or_default().into(),
            group: endpoints::service("group").unwrap_or_default().into(),
            file: endpoints::service("file").unwrap_or_default().into(),
            cloud_msg: default_cloud_msg(),
            reaction: default_reaction(),
            sticker: endpoints::service("sticker").unwrap_or_default().into(),
            voice_call: default_voice_call(),
            ws: (1..=8).map(|n| format!("wss://ws{n}-msg.chat.zalo.me")).collect(),
            service_map_candidates: BTreeMap::new(),
        }
    }
}

fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    if is_allowed_zalo_http_url(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

fn all_urls(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => normalize_host(s).into_iter().collect(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(Value::as_str)
            .filter_map(normalize_host)
            .collect(),
        _ => Vec::new(),
    }
}

impl Domains {
    /// Enforce HTTPS + official Zalo host allowlist across all fields and candidates.
    pub fn sanitize(&mut self) {
        let def = Domains::default();
        for (slot, fallback) in [
            (&mut self.auth, &def.auth),
            (&mut self.wpa_chat, &def.wpa_chat),
            (&mut self.chat, &def.chat),
            (&mut self.conversation, &def.conversation),
            (&mut self.lp_chat, &def.lp_chat),
            (&mut self.lp_control, &def.lp_control),
            (&mut self.friend, &def.friend),
            (&mut self.profile, &def.profile),
            (&mut self.group, &def.group),
            (&mut self.file, &def.file),
            (&mut self.cloud_msg, &def.cloud_msg),
            (&mut self.reaction, &def.reaction),
            (&mut self.sticker, &def.sticker),
            (&mut self.voice_call, &def.voice_call),
        ] {
            if !is_allowed_zalo_http_url(slot) {
                *slot = fallback.clone();
            }
        }
        self.ws.retain(|u| is_allowed_zalo_ws_url(u));
        if self.ws.is_empty() {
            self.ws = def.ws;
        }
        for list in self.service_map_candidates.values_mut() {
            list.retain(|u| is_allowed_zalo_http_url(u));
        }
    }

    /// Restore authoritative `*.chat.zalo.me` hosts from `endpoints::SERVICE_MAP_V3`.
    pub fn heal_from_verified(&mut self) {
        if let Some(v) = endpoints::service("chat") {
            self.chat = v.into();
        }
        if let Some(v) = endpoints::service("conversation") {
            self.conversation = v.into();
        }
        if let Some(v) = endpoints::service("friend") {
            self.friend = v.into();
        }
        if let Some(v) = endpoints::service("profile") {
            self.profile = v.into();
        }
        if let Some(v) = endpoints::service("group") {
            self.group = v.into();
        }
        if let Some(v) = endpoints::service("file") {
            self.file = v.into();
        }
        if let Some(v) = endpoints::service("group_cloud_message") {
            self.cloud_msg = v.into();
        }
        if let Some(v) = endpoints::service("reaction") {
            self.reaction = v.into();
        }
        if let Some(v) = endpoints::service("sticker") {
            self.sticker = v.into();
        }
        if let Some(v) = endpoints::service("voice_call") {
            self.voice_call = v.into();
        }
        self.sanitize();
    }

    pub fn apply_service_map(&mut self, map: &Value) {
        for (key, slot) in [
            ("chat", &mut self.chat),
            ("friend", &mut self.friend),
            ("conversation", &mut self.conversation),
            ("profile", &mut self.profile),
            ("group", &mut self.group),
            ("file", &mut self.file),
            ("group_cloud_message", &mut self.cloud_msg),
            ("reaction", &mut self.reaction),
            ("sticker", &mut self.sticker),
            ("voice_call", &mut self.voice_call),
        ] {
            let urls = map.get(key).map(all_urls).unwrap_or_default();
            if !urls.is_empty() {
                self.service_map_candidates.insert(key.into(), urls.clone());
                if slot.is_empty() || endpoints::service(key).is_none() {
                    *slot = urls[0].clone();
                }
            }
        }
        self.sanitize();
    }

    pub fn candidates(&self, endpoint: &str) -> Vec<String> {
        let mut out = Vec::new();
        let primary = match endpoint {
            "chat" => self.chat.as_str(),
            "friend" => self.friend.as_str(),
            "conversation" => self.conversation.as_str(),
            "profile" => self.profile.as_str(),
            "group" => self.group.as_str(),
            "file" => self.file.as_str(),
            "group_cloud_message" => self.cloud_msg.as_str(),
            "reaction" => self.reaction.as_str(),
            "sticker" => self.sticker.as_str(),
            "voice_call" => self.voice_call.as_str(),
            _ => "",
        };
        if !primary.is_empty() && is_allowed_zalo_http_url(primary) {
            out.push(primary.to_string());
        }
        if let Some(v) = endpoints::service(endpoint) {
            if is_allowed_zalo_http_url(v) && !out.iter().any(|x| x == v) {
                out.push(v.to_string());
            }
        }
        if let Some(extra) = self.service_map_candidates.get(endpoint) {
            for u in extra {
                if is_allowed_zalo_http_url(u) && !out.iter().any(|x| x == u) {
                    out.push(u.clone());
                }
            }
        }
        if endpoint == "friend"
            && !self.profile.is_empty()
            && is_allowed_zalo_http_url(&self.profile)
            && !out.contains(&self.profile)
        {
            out.push(self.profile.clone());
        }
        out
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AccountProfile {
    pub uid: String,
    pub display_name: String,
    pub avatar: String,
    #[serde(default)]
    pub send2me_id: String,
}

impl fmt::Debug for AccountProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountProfile")
            .field("uid", &self.uid)
            .field("display_name", &self.display_name)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    imei: String,
    computer_name: String,
    #[serde(default)]
    first_launch_time: i64,
    /// DPAPI-sealed `zpw_sek` cookie token.
    sealed_zpw_sek: String,
    /// DPAPI-sealed `zpw_enk` key (base64 inside DPAPI envelope).
    sealed_secret_key: String,
    domains: Domains,
    profile: AccountProfile,
}

pub struct Session {
    pub imei: String,
    pub computer_name: String,
    pub first_launch_time: i64,
    pub zpw_sek: Option<String>,
    pub secret_key: Option<Vec<u8>>,
    pub domains: Domains,
    pub profile: AccountProfile,
    pub relay: RelayConfig,
    pub jar: Arc<Jar>,
    pub http: Client,
    pub media_http: Client,
}

/// Redacted `Debug` implementation: never prints `zpw_sek`, `secret_key`, or relay password.
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("uid", &self.profile.uid)
            .field("has_zpw_sek", &self.zpw_sek.is_some())
            .field("has_secret_key", &self.secret_key.is_some())
            .field("relay_enabled", &self.relay.enabled)
            .finish()
    }
}

impl Session {
    pub fn new() -> Result<Self> {
        Self::new_with_relay(RelayConfig::default())
    }

    pub fn new_with_relay(relay: RelayConfig) -> Result<Self> {
        let jar = Arc::new(Jar::default());
        let http = build_http_client(jar.clone(), &relay)?;
        let media_http = build_media_http_client(&relay)?;
        let imei = generate_imei()?;
        let computer_name = detect_computer_name();
        let now_ms = chrono::Utc::now().timestamp_millis();
        Ok(Self {
            imei,
            computer_name,
            first_launch_time: now_ms,
            zpw_sek: None,
            secret_key: None,
            domains: Domains::default(),
            profile: AccountProfile::default(),
            relay,
            jar,
            http,
            media_http,
        })
    }

    pub fn set_relay(&mut self, relay: RelayConfig) -> Result<()> {
        let next_http = build_http_client(self.jar.clone(), &relay)?;
        let next_media = build_media_http_client(&relay)?;
        self.relay = relay;
        self.http = next_http;
        self.media_http = next_media;
        Ok(())
    }

    pub fn set_zpw_sek(&mut self, sek: String) {
        let trimmed = sek.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        seed_zpw_sek_cookies(&self.jar, &trimmed);
        self.zpw_sek = Some(trimmed);
    }

    pub fn set_zpw_enk(&mut self, zpw_enk: &str) {
        let key = crypto::parse_secret_key(zpw_enk);
        if key.len() == 16 || key.len() == 32 {
            self.secret_key = Some(key);
        }
    }

    pub fn common_params(&self) -> String {
        format!("zpw_ver={ZPW_VER}&zpw_type={ZPW_TYPE}")
    }

    pub fn session_key_bytes(&self) -> &[u8] {
        self.secret_key.as_deref().unwrap_or(&[])
    }

    pub fn effective_zpw_sek(&self) -> Option<String> {
        if let Some(sek) = self.zpw_sek.as_deref().filter(|s| !s.is_empty() && *s != "EXPIRED") {
            return Some(sek.to_string());
        }
        for raw_url in [
            "https://qr-id.zaloapp.com",
            "https://wpa.zaloapp.com",
            "https://chat.zalo.me",
            "https://wpa.chat.zalo.me",
        ] {
            let Ok(url) = reqwest::Url::parse(raw_url) else {
                continue;
            };
            let Some(val) = self.jar.cookies(&url) else {
                continue;
            };
            let Ok(cookie_str) = val.to_str() else {
                continue;
            };
            for part in cookie_str.split(';') {
                if let Some(token) = part.trim().strip_prefix("zpw_sek=") {
                    let clean = token.trim();
                    if !clean.is_empty() && clean != "EXPIRED" {
                        return Some(clean.to_string());
                    }
                }
            }
        }
        None
    }

    pub fn is_authenticated(&self) -> bool {
        self.zpw_sek.is_some() && self.secret_key.as_ref().is_some_and(|k| !k.is_empty())
    }

    pub fn clone_shallow(&self) -> Self {
        Self {
            imei: self.imei.clone(),
            computer_name: self.computer_name.clone(),
            first_launch_time: self.first_launch_time,
            zpw_sek: self.zpw_sek.clone(),
            secret_key: self.secret_key.clone(),
            domains: self.domains.clone(),
            profile: self.profile.clone(),
            relay: self.relay.clone(),
            jar: self.jar.clone(),
            http: self.http.clone(),
            media_http: self.media_http.clone(),
        }
    }

    pub fn encoded_params(&self, v: &Value) -> Result<String> {
        let key = self
            .secret_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Phiên chưa có secret key (zpw_enk)"))?;
        let b64 = crypto::encode_aes(key, &v.to_string())?;
        Ok(urlencode(&b64))
    }

    pub fn plain_params(v: &Value) -> String {
        format!("params={}", urlencode(&v.to_string()))
    }

    pub fn apply_server_info(&mut self, data: &Value) {
        if let Some(urls) = data.get("zpw_chts_new").map(all_urls) {
            if let Some(first) = urls.into_iter().next() {
                self.domains.lp_chat = first;
            }
        }
        if let Some(urls) = data.get("zpw_ctrs_new").map(all_urls) {
            if let Some(first) = urls.into_iter().next() {
                self.domains.lp_control = first;
            }
        }
        if let Some(arr) = data.get("zpw_ws").and_then(Value::as_array) {
            let list: Vec<String> = arr
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| is_allowed_zalo_ws_url(s))
                .collect();
            if !list.is_empty() {
                self.domains.ws = list;
            }
        }
        if let Some(map) = data
            .get("zpw_service_map_v3")
            .or_else(|| data.get("zpw_service_map_new"))
        {
            self.domains.apply_service_map(map);
        }
        self.domains.sanitize();
    }

    pub fn apply_login_config(&mut self, data: &Value) {
        if let Some(enk) = data.get("zpw_enk").and_then(Value::as_str) {
            self.set_zpw_enk(enk);
        }
        if let Some(sek) = data
            .get("zpw_sek")
            .or_else(|| data.get("account").and_then(|a| a.get("zpw_sek")))
            .and_then(Value::as_str)
        {
            if !sek.is_empty() {
                self.set_zpw_sek(sek.to_string());
            }
        }
        let uid_val = data
            .get("uid")
            .or_else(|| data.get("userId"))
            .or_else(|| {
                data.get("account")
                    .and_then(|a| a.get("userId").or_else(|| a.get("uid")))
            })
            .or_else(|| {
                data.get("profile")
                    .and_then(|p| p.get("userId").or_else(|| p.get("uid")))
            })
            .or_else(|| {
                data.get("info")
                    .and_then(|i| i.get("uid").or_else(|| i.get("userId")))
            });
        if let Some(uid) = value_as_nonempty_str(uid_val) {
            self.profile.uid = uid;
        }
        let name_val = data
            .get("account")
            .and_then(|a| {
                a.get("display_name")
                    .or_else(|| a.get("displayName"))
                    .or_else(|| a.get("zaloName"))
            })
            .or_else(|| data.get("display_name"))
            .or_else(|| data.get("displayName"))
            .or_else(|| data.get("zalo_name"))
            .or_else(|| {
                data.get("profile")
                    .and_then(|p| p.get("displayName").or_else(|| p.get("zaloName")))
            })
            .or_else(|| {
                data.get("settings")
                    .and_then(|st| st.get("profile"))
                    .and_then(|p| p.get("name"))
            });
        if let Some(name) = value_as_nonempty_str(name_val) {
            self.profile.display_name = name;
        }
        let avatar_val = data
            .get("avatar")
            .or_else(|| data.get("account").and_then(|a| a.get("avatar")));
        if let Some(avatar) = value_as_nonempty_str(avatar_val) {
            self.profile.avatar = crate::chat::parser::normalize_avatar_url(&avatar);
        }
        if let Some(send2me) = value_as_nonempty_str(data.get("send2me_id")) {
            self.profile.send2me_id = send2me;
        }
        self.apply_server_info(data);
        self.domains.heal_from_verified();
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let sealed_sek = match &self.zpw_sek {
            Some(sek) => dpapi::protect_secret(sek)?,
            None => String::new(),
        };
        let sealed_key = match &self.secret_key {
            Some(k) => dpapi::protect_secret(&B64.encode(k))?,
            None => String::new(),
        };
        let saved = SavedSession {
            imei: self.imei.clone(),
            computer_name: self.computer_name.clone(),
            first_launch_time: self.first_launch_time,
            sealed_zpw_sek: sealed_sek,
            sealed_secret_key: sealed_key,
            domains: self.domains.clone(),
            profile: self.profile.clone(),
        };
        let json = serde_json::to_string_pretty(&saved)?;
        data_dir::write_private_atomic(path, json.as_bytes())
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        Self::load_from_file_with_relay(path, RelayConfig::default())
    }

    pub fn load_from_file_with_relay(path: &Path, relay: RelayConfig) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Không đọc được {}", path.display()))?;
        let saved: SavedSession = serde_json::from_str(&text)
            .context("Định dạng session.json không hợp lệ")?;

        let jar = Arc::new(Jar::default());
        let http = build_http_client(jar.clone(), &relay)?;
        let media_http = build_media_http_client(&relay)?;
        let mut domains = saved.domains;
        domains.heal_from_verified();

        let zpw_sek = if saved.sealed_zpw_sek.is_empty() {
            None
        } else {
            let s = dpapi::unprotect_secret(&saved.sealed_zpw_sek)?;
            if s.is_empty() {
                None
            } else {
                seed_zpw_sek_cookies(&jar, &s);
                Some(s)
            }
        };

        let secret_key = if saved.sealed_secret_key.is_empty() {
            None
        } else {
            let b64 = dpapi::unprotect_secret(&saved.sealed_secret_key)?;
            if b64.is_empty() {
                None
            } else {
                Some(B64.decode(b64.trim())?)
            }
        };

        let mut profile = saved.profile;
        profile.avatar = crate::chat::parser::normalize_avatar_url(&profile.avatar);

        Ok(Self {
            imei: saved.imei,
            computer_name: saved.computer_name,
            first_launch_time: saved.first_launch_time,
            zpw_sek,
            secret_key,
            domains,
            profile,
            relay,
            jar,
            http,
            media_http,
        })
    }

    /// Perform an authenticated GET with `ParamCipher v2` + `signkey`, distinguishing
    /// genuine session expiration (`600` / `-102`) from transient network/HTTP errors.
    pub async fn get_encrypted(
        &self,
        base_url: &str,
        endpoint_path: &str,
        tag: &str,
        payload: &Value,
    ) -> Result<Value, SessionError> {
        let cipher = ParamCipher::new(ZPW_TYPE, &self.imei, self.first_launch_time)
            .map_err(|e| SessionError::Protocol {
                code: -1,
                message: e.to_string(),
            })?;
        let body = cipher
            .build_body(ZPW_TYPE, ZPW_VER, payload, tag)
            .map_err(|e| SessionError::Protocol {
                code: -1,
                message: e.to_string(),
            })?;

        let mut pairs = Vec::new();
        if let Some(obj) = body.as_object() {
            for (k, v) in obj {
                let s = match v {
                    Value::String(st) => st.clone(),
                    other => other.to_string(),
                };
                pairs.push(format!("{k}={}", urlencode(&s)));
            }
        }
        pairs.sort();
        let url = format!("{base_url}{endpoint_path}?{}", pairs.join("&"));
        if !is_allowed_zalo_http_url(&url) {
            return Err(SessionError::Protocol {
                code: -1,
                message: "Từ chối gửi yêu cầu tới host không hợp lệ".into(),
            });
        }

        let parsed = reqwest::Url::parse(&url).map_err(|e| SessionError::Protocol {
            code: -1,
            message: format!("URL không hợp lệ: {e}"),
        })?;
        let resp = self
            .attach_zalo_auth_headers(self.http.get(parsed.clone()), &parsed)
            .send()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;

        if status != 200 {
            return Err(SessionError::Http {
                status,
                body: truncate(&text, 200),
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| SessionError::Protocol {
            code: -1,
            message: format!("Invalid JSON: {e}"),
        })?;
        let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        if is_session_expired_code(code) {
            return Err(SessionError::Expired { code });
        }
        if code != 0 {
            let msg = v
                .get("error_message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            return Err(SessionError::Protocol { code, message: msg });
        }
        Ok(v)
    }

    pub async fn get_json(&self, url: &str) -> Result<Value> {
        self.send_raw_with_timeout(Method::GET, url, None, None).await
    }

    pub async fn get_json_with_timeout(
        &self,
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        self.send_raw_with_timeout(Method::GET, url, None, Some(timeout))
            .await
    }

    pub async fn post_form(&self, url: &str, body: String) -> Result<Value> {
        self.send_raw_with_timeout(Method::POST, url, Some(body), None)
            .await
    }

    pub async fn post_multipart(
        &self,
        url: &str,
        field_name: &str,
        file_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        if !is_allowed_zalo_http_url(url) {
            return Err(SessionError::Protocol {
                code: -1,
                message: "Từ chối upload tới host không hợp lệ".into(),
            }
            .into());
        }
        let parsed = reqwest::Url::parse(url).map_err(|e| SessionError::Protocol {
            code: -1,
            message: format!("URL không hợp lệ: {e}"),
        })?;
        let part = Part::bytes(bytes.to_vec())
            .file_name(file_name.to_string())
            .mime_str(mime)
            .context("Invalid MIME type")?;
        let form = Form::new().part(field_name.to_string(), part);
        let req = self.http.post(parsed.clone()).multipart(form);
        let resp = self
            .attach_zalo_auth_headers(req, &parsed)
            .send()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;
        Self::decode_http_response(resp).await
    }

    pub async fn get_endpoint(&self, endpoint: &str, path_and_query: &str) -> Result<Value> {
        let candidates = self.domains.candidates(endpoint);
        let mut last_err: Option<anyhow::Error> = None;
        for host in candidates {
            let url = format!("{}{}", host.trim_end_matches('/'), path_and_query);
            match self.get_json(&url).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    if let Some(SessionError::Expired { .. }) = e.downcast_ref::<SessionError>() {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Không có host khả dụng cho {endpoint}")))
    }

    fn attach_zalo_auth_headers(
        &self,
        mut req: reqwest::RequestBuilder,
        parsed: &reqwest::Url,
    ) -> reqwest::RequestBuilder {
        if let Some(sek) = self.effective_zpw_sek() {
            if let Ok(hv) = HeaderValue::from_str(&sek) {
                req = req.header("zpw_sek", hv);
            }
            let existing = self
                .jar
                .cookies(parsed)
                .and_then(|v| v.to_str().ok().map(str::to_string))
                .unwrap_or_default();
            let jar_has_sek = existing.contains("zpw_sek=");
            let jar_has_zlang = existing.contains("_zlang=");
            if !jar_has_sek {
                req = req.header(COOKIE, format!("zpw_sek={sek}; _zlang=vi"));
            } else if !jar_has_zlang {
                self.jar.add_cookie_str("_zlang=vi; Path=/", parsed);
            }
        }
        req
    }

    async fn send_raw_with_timeout(
        &self,
        method: Method,
        url: &str,
        form_body: Option<String>,
        timeout_override: Option<std::time::Duration>,
    ) -> Result<Value> {
        if !is_allowed_zalo_http_url(url) {
            return Err(SessionError::Protocol {
                code: -1,
                message: "Từ chối gửi cookie/yêu cầu tới host không hợp lệ".into(),
            }
            .into());
        }
        let parsed = reqwest::Url::parse(url).map_err(|e| SessionError::Protocol {
            code: -1,
            message: format!("URL không hợp lệ: {e}"),
        })?;
        let mut req =
            self.attach_zalo_auth_headers(self.http.request(method, parsed.clone()), &parsed);
        if let Some(timeout) = timeout_override {
            req = req.timeout(timeout);
        }
        if let Some(body) = form_body {
            req = req
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(body);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                SessionError::Network(format!("timeout: {e}"))
            } else {
                SessionError::Network(e.to_string())
            }
        })?;
        Self::decode_http_response(resp).await
    }

    pub(crate) fn decode_http_body(status: u16, text: &str) -> Result<Value> {
        if status != 200 {
            return Err(SessionError::Http {
                status,
                body: truncate(text, 200),
            }
            .into());
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(serde_json::json!({ "error_code": 0, "data": Value::Null }));
        }
        let v: Value = match serde_json::from_str(trimmed) {
            Ok(val) => val,
            Err(e) => {
                let looks_like_b64 = trimmed.len() >= 16
                    && trimmed.len().is_multiple_of(4)
                    && trimmed
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
                    && B64.decode(trimmed).is_ok();
                if looks_like_b64 {
                    serde_json::json!({ "error_code": 0, "data": trimmed })
                } else {
                    return Err(SessionError::Protocol {
                        code: -1,
                        message: format!("Invalid JSON: {e}"),
                    }
                    .into());
                }
            }
        };
        let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        if is_session_expired_code(code) {
            return Err(SessionError::Expired { code }.into());
        }
        Ok(v)
    }

    async fn decode_http_response(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;
        Self::decode_http_body(status, &text)
    }

    /// Verify with Zalo's `/api/social/profile/me-v2` whether the stored `zpw_sek` and `secret_key`
    /// are still accepted by the server. Network/timeout errors map to `Unreachable` so offline
    /// browsing of local history remains available.
    pub async fn check_session_status(&self) -> SessionStatus {
        self.check_session_inner().await.0
    }

    /// Verify session via `/api/social/profile/me-v2` and refresh `self.profile`
    /// (`uid`, `display_name`, `avatar`) so `my_uid` is guaranteed accurate.
    pub async fn verify_and_refresh_profile(&mut self) -> SessionStatus {
        let (status, profile_node) = self.check_session_inner().await;
        if status == SessionStatus::Valid {
            if let Some(node) = profile_node {
                let data = node.get("data").unwrap_or(&node);
                let p = data.get("profile").unwrap_or(data);
                if let Some(uid) = value_as_nonempty_str(
                    p.get("userId")
                        .or_else(|| p.get("uid"))
                        .or_else(|| data.get("userId"))
                        .or_else(|| data.get("uid")),
                ) {
                    self.profile.uid = uid;
                }
                if let Some(name) = value_as_nonempty_str(
                    p.get("displayName")
                        .or_else(|| p.get("zaloName"))
                        .or_else(|| p.get("display_name")),
                ) {
                    self.profile.display_name = name;
                }
                if let Some(avt) = value_as_nonempty_str(p.get("avatar").or_else(|| p.get("avt")))
                {
                    self.profile.avatar = crate::chat::parser::normalize_avatar_url(&avt);
                }
            }
        }
        status
    }

    async fn check_session_inner(&self) -> (SessionStatus, Option<Value>) {
        if !self.is_authenticated() || self.effective_zpw_sek().is_none() {
            return (SessionStatus::Expired { code: 102 }, None);
        }
        let body = serde_json::json!({
            "avatar_size": 160,
            "imei": self.imei,
        });
        let Ok(enc) = self.encoded_params(&body) else {
            return (SessionStatus::Expired { code: 102 }, None);
        };
        let profile_host = if is_allowed_zalo_http_url(&self.domains.profile) {
            self.domains.profile.trim_end_matches('/')
        } else {
            endpoints::service("profile").unwrap_or("https://tt-profile-wpa.chat.zalo.me")
        };
        let url = format!(
            "{profile_host}/api/social/profile/me-v2?{}&params={enc}&os=8",
            self.common_params()
        );
        let resp = match tokio::time::timeout(std::time::Duration::from_secs(6), self.get_json(&url)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                if let Some(SessionError::Expired { code }) = e.downcast_ref::<SessionError>() {
                    return (SessionStatus::Expired { code: *code }, None);
                }
                return (SessionStatus::Unreachable, None);
            }
            Err(_) => return (SessionStatus::Unreachable, None),
        };
        let dec = self.maybe_decrypt(resp, self.session_key_bytes());
        match dec.get("error_code").and_then(Value::as_i64) {
            Some(0) => (SessionStatus::Valid, Some(dec)),
            Some(code) if is_session_expired_code(code) => (SessionStatus::Expired { code }, None),
            _ => (SessionStatus::Unreachable, None),
        }
    }

    pub fn maybe_decrypt(&self, v: Value, key: &[u8]) -> Value {
        let Some(data_b64) = v.get("data").and_then(Value::as_str) else {
            return v;
        };
        let trimmed = data_b64.trim();
        if trimmed.is_empty() || trimmed.starts_with('{') || trimmed.starts_with('[') || key.is_empty() {
            return v;
        }
        match crypto::decode_aes(key, trimmed) {
            Ok(plain) => serde_json::from_str::<Value>(&plain).unwrap_or(v),
            Err(_) => v,
        }
    }

    pub async fn download_media_bytes(&self, url: &str, max_bytes: usize) -> Result<(Vec<u8>, String)> {
        let media = self.download_media_with_meta(url, max_bytes).await?;
        Ok((media.bytes, media.mime))
    }

    pub async fn download_media_with_meta(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<DownloadedMedia> {
        self.download_media_with_meta_progress(url, max_bytes, |_, _| {})
            .await
    }

    async fn send_media_get(&self, url: &str) -> Result<reqwest::Response> {
        self.send_media_get_with_validator(url, normalize_allowed_media_url)
            .await
    }

    async fn send_media_get_with_validator<F>(
        &self,
        url: &str,
        validate_url: F,
    ) -> Result<reqwest::Response>
    where
        F: Fn(&str) -> std::result::Result<reqwest::Url, String>,
    {
        let mut current_url = match validate_url(url) {
            Ok(u) => u,
            Err(origin) => {
                eprintln!("[litechat][media] blocked untrusted media URL: {origin}");
                return Err(MediaDownloadError::UntrustedUrl { origin }.into());
            }
        };

        const MAX_MANUAL_REDIRECT_HOPS: usize = 5;
        let mut hops = 0usize;
        loop {
            let mut retried = false;
            let r = loop {
                match self
                    .media_http
                    .get(current_url.clone())
                    .header(REFERER, "https://chat.zalo.me/")
                    .header(ORIGIN, "https://chat.zalo.me")
                    .send()
                    .await
                {
                    Ok(resp) => break resp,
                    Err(e) => {
                        if !retried && should_retry_media_send_error(&self.relay, &e) {
                            retried = true;
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            continue;
                        }
                        return Err(SessionError::Network(format_media_network_error(
                            &current_url,
                            &self.relay,
                            &e,
                        ))
                        .into());
                    }
                }
            };

            if r.status().is_redirection() {
                let Some(loc) = r
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return Ok(r);
                };
                let resolved_raw = match r.url().join(loc.trim()) {
                    Ok(u) => u.to_string(),
                    Err(_) => loc.to_string(),
                };
                if hops >= MAX_MANUAL_REDIRECT_HOPS {
                    let target_origin = reqwest::Url::parse(&resolved_raw)
                        .map(|u| sanitize_url_origin_for_log(&u))
                        .unwrap_or_else(|_| "URL chuyển hướng không hợp lệ".to_string());
                    eprintln!("[litechat][media] too many redirects -> {target_origin}");
                    anyhow::bail!("Tải tệp thất bại: quá nhiều chuyển hướng ({target_origin})");
                }
                match validate_url(&resolved_raw) {
                    Ok(next_url) => {
                        current_url = next_url;
                        hops += 1;
                        continue;
                    }
                    Err(origin) => {
                        eprintln!(
                            "[litechat][media] blocked redirect to untrusted URL: {origin}"
                        );
                        anyhow::bail!(
                            "Từ chối tải tệp từ URL chuyển hướng không nằm trong danh sách CDN tin cậy ({origin})"
                        );
                    }
                }
            }
            return Ok(r);
        }
    }

    pub async fn download_media_with_meta_progress<F>(
        &self,
        url: &str,
        max_bytes: usize,
        mut on_progress: F,
    ) -> Result<DownloadedMedia>
    where
        F: FnMut(u64, Option<u64>),
    {
        let mut resp = self.send_media_get(url).await?;

        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("Tải tệp thất bại (HTTP {})", status.as_u16());
        }
        let total_bytes = resp.content_length();
        if let Some(len) = total_bytes {
            if len as usize > max_bytes {
                anyhow::bail!("Kích thước tệp vượt quá giới hạn cho phép");
            }
        }

        let resp_url = resp.url().clone();
        let final_url = resp_url.to_string();
        let content_disposition_filename = resp
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
            .and_then(parse_content_disposition_filename);

        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();

        on_progress(0, total_bytes);
        let mut buf = Vec::new();
        let mut last_emit = std::time::Instant::now();
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            SessionError::Network(format_media_network_error(&resp_url, &self.relay, &e))
        })? {
            if buf.len().saturating_add(chunk.len()) > max_bytes {
                anyhow::bail!("Kích thước tệp vượt quá giới hạn cho phép");
            }
            buf.extend_from_slice(&chunk);
            if last_emit.elapsed() >= std::time::Duration::from_millis(80) {
                on_progress(buf.len() as u64, total_bytes);
                last_emit = std::time::Instant::now();
            }
        }
        on_progress(buf.len() as u64, total_bytes.or(Some(buf.len() as u64)));
        Ok(DownloadedMedia {
            bytes: buf,
            mime,
            final_url,
            content_disposition_filename,
        })
    }

    /// Streams a remote media URL directly into `tmp_path` in O(1) RAM with NO per-file size cap,
    /// computing SHA-256 incrementally, rejecting SVG MIME types, and classifying HTTP 403/404/410
    /// as [`MediaDownloadError::CdnExpired`].
    pub async fn download_media_to_temp_file<F>(
        &self,
        url: &str,
        tmp_path: &Path,
        on_progress: F,
    ) -> Result<StreamedMediaMeta>
    where
        F: FnMut(u64, Option<u64>),
    {
        let resp = self.send_media_get(url).await?;
        stream_response_to_temp_file(resp, tmp_path, &self.relay, on_progress).await
    }

    pub async fn download_media_data_url(&self, url: &str, max_bytes: usize) -> Result<String> {
        let (bytes, mime) = self.download_media_bytes(url, max_bytes).await?;
        let ct = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if ct.contains("svg") {
            anyhow::bail!("Không hỗ trợ hiển thị tệp SVG qua data URL");
        }
        let final_mime = infer_media_data_url_mime(url, &mime);
        Ok(format!("data:{final_mime};base64,{}", B64.encode(&bytes)))
    }
}

#[derive(Debug, Clone)]
pub struct DownloadedMedia {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub final_url: String,
    pub content_disposition_filename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedMediaMeta {
    pub mime: String,
    pub file_name: Option<String>,
    pub size_bytes: u64,
    pub sha256_hex: String,
    pub status_code: u16,
}

pub fn is_forbidden_svg_mime(mime: &str) -> bool {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .contains("svg")
}

fn is_valid_safe_mime(ct: &str) -> bool {
    let Some((kind, subtype)) = ct.split_once('/') else {
        return false;
    };
    if kind.is_empty() || subtype.is_empty() {
        return false;
    }
    let valid_part = |p: &str| {
        p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'+' | b'_'))
    };
    if !valid_part(kind) || !valid_part(subtype) {
        return false;
    }
    if subtype.contains("svg") {
        return false;
    }
    true
}

pub fn infer_streamed_media_mime(
    url: &str,
    file_name: Option<&str>,
    content_type: &str,
) -> String {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if is_valid_safe_mime(&ct)
        && ct != "application/octet-stream"
        && ct != "binary/octet-stream"
    {
        return ct;
    }
    let hint = file_name
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| {
            url.split(['?', '#'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        });
    let from_ext = |s: &str| -> Option<&'static str> {
        if s.ends_with(".png") {
            Some("image/png")
        } else if s.ends_with(".jpg") || s.ends_with(".jpeg") {
            Some("image/jpeg")
        } else if s.ends_with(".webp") {
            Some("image/webp")
        } else if s.ends_with(".gif") {
            Some("image/gif")
        } else if s.ends_with(".bmp") {
            Some("image/bmp")
        } else if s.ends_with(".avif") {
            Some("image/avif")
        } else if s.ends_with(".mp4") || s.ends_with(".m4v") {
            Some("video/mp4")
        } else if s.ends_with(".webm") {
            Some("video/webm")
        } else if s.ends_with(".mov") {
            Some("video/quicktime")
        } else if s.ends_with(".mkv") {
            Some("video/x-matroska")
        } else if s.ends_with(".mp3") {
            Some("audio/mpeg")
        } else if s.ends_with(".aac") {
            Some("audio/aac")
        } else if s.ends_with(".m4a") {
            Some("audio/mp4")
        } else if s.ends_with(".ogg") || s.ends_with(".opus") {
            Some("audio/ogg")
        } else if s.ends_with(".wav") {
            Some("audio/wav")
        } else if s.ends_with(".pdf") {
            Some("application/pdf")
        } else if s.ends_with(".zip") {
            Some("application/zip")
        } else if s.ends_with(".txt") {
            Some("text/plain")
        } else {
            None
        }
    };
    if let Some(m) = from_ext(&hint) {
        return m.to_string();
    }
    if file_name.is_some() {
        let url_path = url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(m) = from_ext(&url_path) {
            return m.to_string();
        }
    }
    "application/octet-stream".to_string()
}

pub(crate) async fn stream_response_to_temp_file<F>(
    mut resp: reqwest::Response,
    tmp_path: &Path,
    relay: &RelayConfig,
    mut on_progress: F,
) -> Result<StreamedMediaMeta>
where
    F: FnMut(u64, Option<u64>),
{
    let status_code = resp.status().as_u16();
    if matches!(status_code, 403 | 404 | 410) {
        return Err(MediaDownloadError::CdnExpired {
            status: status_code,
        }
        .into());
    }
    if !resp.status().is_success() {
        return Err(MediaDownloadError::HttpStatus {
            status: status_code,
        }
        .into());
    }

    let raw_ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if is_forbidden_svg_mime(&raw_ct) {
        return Err(MediaDownloadError::ForbiddenSvg.into());
    }

    let file_name = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
        .and_then(parse_content_disposition_filename);

    let resp_url = resp.url().clone();
    let mime = infer_streamed_media_mime(resp_url.as_str(), file_name.as_deref(), &raw_ct);
    if is_forbidden_svg_mime(&mime) {
        return Err(MediaDownloadError::ForbiddenSvg.into());
    }

    let total_bytes = resp.content_length();
    if let Some(parent) = tmp_path.parent() {
        data_dir::ensure_private_dir(parent)?;
    }

    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let file = opts
        .open(tmp_path)
        .await
        .with_context(|| format!("Không tạo được tệp tạm {}", tmp_path.display()))?;
    let mut writer = tokio::io::BufWriter::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;

    on_progress(0, total_bytes);
    let mut last_emit = std::time::Instant::now();

    let stream_res: Result<()> = async {
        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            SessionError::Network(format_media_network_error(&resp_url, relay, &e))
        })? {
            hasher.update(&chunk);
            writer
                .write_all(&chunk)
                .await
                .with_context(|| format!("Không ghi được tệp tạm {}", tmp_path.display()))?;
            written = written.saturating_add(chunk.len() as u64);
            if last_emit.elapsed() >= std::time::Duration::from_millis(80) {
                on_progress(written, total_bytes);
                last_emit = std::time::Instant::now();
            }
        }
        writer
            .flush()
            .await
            .with_context(|| format!("Không đồng bộ được tệp tạm {}", tmp_path.display()))?;
        Ok(())
    }
    .await;

    drop(writer);
    if let Err(err) = stream_res {
        let _ = tokio::fs::remove_file(tmp_path).await;
        return Err(err);
    }

    on_progress(written, total_bytes.or(Some(written)));
    let digest = hasher.finalize();
    let sha256_hex = digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    });

    Ok(StreamedMediaMeta {
        mime,
        file_name,
        size_bytes: written,
        sha256_hex,
        status_code,
    })
}

fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = (bytes[i + 1] as char).to_digit(16);
            let h2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (h1, h2) {
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

fn clean_disposition_basename(raw: &str) -> Option<String> {
    let base = raw
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        None
    } else {
        Some(base.to_string())
    }
}

fn split_header_params_respecting_quotes(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes && b == b'\\' {
            escaped = true;
            continue;
        }
        if b == b'"' {
            in_quotes = !in_quotes;
            continue;
        }
        if b == b';' && !in_quotes {
            out.push(&s[start..idx]);
            start = idx + 1;
        }
    }
    if start <= s.len() {
        out.push(&s[start..]);
    }
    out
}

pub fn parse_content_disposition_filename(header_val: &str) -> Option<String> {
    let mut ext_star: Option<String> = None;
    let mut plain: Option<String> = None;
    for part in split_header_params_respecting_quotes(header_val)
        .into_iter()
        .skip(1)
    {
        let Some((raw_key, raw_val)) = part.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let val = raw_val.trim();
        if key == "filename*" {
            let unquoted = val.trim_matches('"').trim();
            let encoded = if let Some((_charset, after_first)) = unquoted.split_once('\'') {
                if let Some((_lang, value)) = after_first.split_once('\'') {
                    value
                } else {
                    unquoted
                }
            } else {
                unquoted
            };
            let decoded = percent_decode_utf8(encoded);
            if let Some(clean) = clean_disposition_basename(&decoded) {
                ext_star = Some(clean);
            }
        } else if key == "filename" && plain.is_none() {
            let unquoted = if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                val[1..val.len() - 1].replace("\\\"", "\"")
            } else {
                val.trim_matches('"').to_string()
            };
            if let Some(clean) = clean_disposition_basename(&unquoted) {
                plain = Some(clean);
            }
        }
    }
    ext_star.or(plain)
}

fn is_safe_media_data_url_mime(ct: &str) -> bool {
    let Some((kind, subtype)) = ct.split_once('/') else {
        return false;
    };
    if !matches!(kind, "image" | "audio" | "video") || subtype.is_empty() {
        return false;
    }
    if !subtype
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'+' | b'_'))
    {
        return false;
    }
    if subtype.contains("svg") || subtype.contains("xml") || subtype.contains("html") {
        return false;
    }
    true
}

pub fn infer_media_data_url_mime(url: &str, content_type: &str) -> String {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if is_safe_media_data_url_mime(&ct) {
        return ct;
    }
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if path.ends_with(".mp4") || path.ends_with(".m4v") {
        return "video/mp4".to_string();
    }
    if path.ends_with(".webm") {
        return "video/webm".to_string();
    }
    if path.ends_with(".mov") {
        return "video/quicktime".to_string();
    }
    if path.ends_with(".mp3") {
        return "audio/mpeg".to_string();
    }
    if path.ends_with(".aac") {
        return "audio/aac".to_string();
    }
    if path.ends_with(".m4a") {
        return "audio/mp4".to_string();
    }
    if path.ends_with(".ogg") || path.ends_with(".opus") {
        return "audio/ogg".to_string();
    }
    if path.ends_with(".wav") {
        return "audio/wav".to_string();
    }
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        return "image/jpeg".to_string();
    }
    if path.ends_with(".webp") {
        return "image/webp".to_string();
    }
    if path.ends_with(".gif") {
        return "image/gif".to_string();
    }
    "image/png".to_string()
}

fn value_as_nonempty_str(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

const ZALO_CLIENT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ZaloPC/26.10.11 Chrome/122.0.0.0 Electron/29.0.0 Safari/537.36";

fn build_http_client(jar: Arc<Jar>, relay: &RelayConfig) -> Result<Client> {
    let mut headers = HeaderMap::new();
    headers.insert(ORIGIN, HeaderValue::from_static("https://chat.zalo.me"));
    headers.insert(REFERER, HeaderValue::from_static("https://chat.zalo.me/"));
    let redirect_policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.stop()
        } else if is_allowed_zalo_http_url(attempt.url().as_str()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let builder = Client::builder()
        .cookie_provider(jar)
        .default_headers(headers)
        .redirect(redirect_policy)
        .user_agent(ZALO_CLIENT_USER_AGENT)
        .timeout(std::time::Duration::from_secs(35));
    relay
        .apply_to_client_builder(builder)?
        .build()
        .context("Không khởi tạo được HTTP client")
}

fn build_media_http_client(relay: &RelayConfig) -> Result<Client> {
    let mut builder = Client::builder()
        .redirect(redirect::Policy::none())
        .user_agent(ZALO_CLIENT_USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60));
    if relay.enabled {
        builder = builder.http1_only().pool_max_idle_per_host(0);
    }
    relay
        .apply_to_client_builder(builder)?
        .build()
        .context("Không khởi tạo được HTTP client tải tệp")
}

pub fn seed_zpw_sek_cookies(jar: &Jar, sek: &str) {
    if sek.is_empty() || sek == "EXPIRED" {
        return;
    }
    let domain_seeds = [
        ("https://wpa.zaloapp.com", "zaloapp.com"),
        ("https://qr-id.zaloapp.com", "zaloapp.com"),
        ("https://chat.zalo.me", "chat.zalo.me"),
        ("https://chat.zalo.me", "zalo.me"),
    ];
    for (base_url, domain) in domain_seeds {
        if let Ok(url) = reqwest::Url::parse(base_url) {
            jar.add_cookie_str(&format!("zpw_sek={sek}; Domain={domain}; Path=/"), &url);
            jar.add_cookie_str(&format!("zpw_sek={sek}; Domain=.{domain}; Path=/"), &url);
            jar.add_cookie_str(&format!("_zlang=vi; Domain={domain}; Path=/"), &url);
            jar.add_cookie_str(&format!("_zlang=vi; Domain=.{domain}; Path=/"), &url);
        }
    }
    for host in [
        "https://chat.zalo.me",
        "https://wpa.zaloapp.com",
        "https://qr-id.zaloapp.com",
        "https://wpa.chat.zalo.me",
        "https://tt-chat3-wpa.api.zaloapp.com",
        "https://tt-group-cm.api.zaloapp.com",
        "https://reaction.api.zaloapp.com",
    ] {
        if let Ok(url) = reqwest::Url::parse(host) {
            jar.add_cookie_str(&format!("zpw_sek={sek}; Path=/"), &url);
            jar.add_cookie_str("_zlang=vi; Path=/", &url);
        }
    }
    for (_, host) in endpoints::SERVICE_MAP_V3 {
        if let Ok(url) = reqwest::Url::parse(host) {
            jar.add_cookie_str(&format!("zpw_sek={sek}; Path=/"), &url);
            jar.add_cookie_str("_zlang=vi; Path=/", &url);
        }
    }
}

fn generate_imei() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("OS CSPRNG failure")?;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

fn detect_computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "LiteChat-Desktop".into())
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Build sorted URL-encoded query parameters with `signkey`.
pub fn signed_query(params: &Map<String, Value>, tag: &str) -> String {
    let sig = crypto::sign_key(tag, params);
    let mut all = BTreeMap::new();
    for (k, v) in params {
        let s = match v {
            Value::String(st) => st.clone(),
            other => other.to_string(),
        };
        all.insert(k.clone(), s);
    }
    all.insert("signkey".into(), sig);
    all.into_iter()
        .map(|(k, v)| format!("{k}={}", urlencode(&v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_debug_never_leaks_secrets() {
        let mut s = Session::new().unwrap();
        s.set_zpw_sek("SECRET_SEK_TOKEN_999".into());
        s.set_zpw_enk("0123456789abcdef0123456789abcdef");
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("SECRET_SEK_TOKEN_999"));
        assert!(!dbg.contains("0123456789abcdef"));
    }

    #[test]
    fn session_save_and_load_seals_secrets_with_dpapi() {
        let dir = std::env::temp_dir().join(format!("litechat-sess-test-{}", std::process::id()));
        let paths = data_dir::StoragePaths::from_dir(&dir).unwrap();
        let mut s = Session::new().unwrap();
        s.set_zpw_sek("my_secret_sek_cookie".into());
        s.set_zpw_enk("0123456789abcdef0123456789abcdef");
        s.profile.uid = "123456".into();
        s.save_to_file(&paths.session_file()).unwrap();

        let disk_raw = std::fs::read_to_string(paths.session_file()).unwrap();
        assert!(!disk_raw.contains("my_secret_sek_cookie"));

        let loaded = Session::load_from_file(&paths.session_file()).unwrap();
        assert_eq!(loaded.zpw_sek.as_deref(), Some("my_secret_sek_cookie"));
        assert_eq!(loaded.profile.uid, "123456");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowlist_blocks_http_and_untrusted_domains() {
        assert!(is_allowed_zalo_http_url("https://chat.zalo.me/api/test"));
        assert!(is_allowed_zalo_http_url("https://tt-chat3-wpa.chat.zalo.me"));
        assert!(is_allowed_zalo_http_url("https://wpa.zaloapp.com/api/login"));
        assert!(!is_allowed_zalo_http_url("http://chat.zalo.me/api/test"));
        assert!(!is_allowed_zalo_http_url("https://evil.com/steal"));
        assert!(!is_allowed_zalo_http_url("https://chat.zalo.me.evil.com"));
        assert!(!is_allowed_zalo_http_url("https://evil.com@chat.zalo.me"));
        assert!(!is_allowed_zalo_http_url("https://chat.zalo.me:8443/api"));

        assert!(is_allowed_zalo_ws_url("wss://ws1-msg.chat.zalo.me"));
        assert!(!is_allowed_zalo_ws_url("ws://ws1-msg.chat.zalo.me"));
        assert!(!is_allowed_zalo_ws_url("wss://evil.com/ws"));
    }

    #[test]
    fn apply_service_map_and_server_info_reject_poisoned_hosts() {
        let mut s = Session::new().unwrap();
        s.apply_server_info(&serde_json::json!({
            "zpw_chts_new": ["http://evil.com", "https://p5-msg-chat.zaloapp.com"],
            "zpw_ctrs_new": ["https://chat.zalo.me.evil.com"],
            "zpw_ws": ["ws://ws1-msg.chat.zalo.me", "wss://evil.com", "wss://ws2-msg.chat.zalo.me"],
            "zpw_service_map_v3": {
                "reaction": ["http://reaction.chat.zalo.me", "https://evil.com", "https://reaction.chat.zalo.me"],
                "chat": ["https://evil.com"]
            }
        }));
        assert_eq!(s.domains.lp_chat, "https://p5-msg-chat.zaloapp.com");
        assert_eq!(s.domains.lp_control, endpoints::CONVERS_CTL);
        assert_eq!(s.domains.ws, vec!["wss://ws2-msg.chat.zalo.me"]);
        assert_eq!(s.domains.reaction, "https://reaction.chat.zalo.me");
        assert!(s.domains.candidates("reaction").contains(&"https://reaction.chat.zalo.me".to_string()));
        assert!(!s.domains.candidates("reaction").iter().any(|u| u.contains("evil.com")));
        assert!(s.domains.candidates("chat").iter().all(|u| is_allowed_zalo_http_url(u)));
        assert!(!s.domains.candidates("chat").iter().any(|u| u.contains("evil.com")));
    }

    #[tokio::test]
    async fn send_raw_refuses_untrusted_or_plaintext_url() {
        let mut s = Session::new().unwrap();
        s.set_zpw_sek("secret_cookie".into());
        let err_http = s.get_json("http://chat.zalo.me/api/test").await.unwrap_err();
        assert!(err_http.to_string().contains("không hợp lệ"));
        let err_evil = s.get_json("https://attacker.example/collect").await.unwrap_err();
        assert!(err_evil.to_string().contains("không hợp lệ"));
    }

    #[test]
    fn expired_codes_match_expected_set() {
        assert!(is_session_expired_code(600));
        assert!(is_session_expired_code(-102));
        assert!(is_session_expired_code(102));
        assert!(!is_session_expired_code(0));
        assert!(!is_session_expired_code(500));
    }

    #[test]
    fn media_url_allowlist_checks_cdn_hosts() {
        assert!(is_allowed_media_url("https://f10-zpc.zdn.vn/photo.jpg"));
        assert!(is_allowed_media_url("https://s120-ava-talk.zadn.vn/avatar.png"));
        assert!(is_allowed_media_url("https://api.zaloapp.com/api/emoticon/sticker/webpc?eid=1"));
        assert!(!is_allowed_media_url("http://f10-zpc.zdn.vn/photo.jpg"));
        assert!(!is_allowed_media_url("https://evil.example/a.png"));
        assert!(!is_allowed_media_url("https://f-zdn.vn/a.png"));
        assert!(!is_allowed_media_url("https://f10-zpc.zdn.vn.evil.com/a.png"));
    }

    #[test]
    fn data_url_decoder_and_session_relay_update_work() {
        let (bytes, mime) = decode_data_url_bytes("data:image/png;base64,aGVsbG8=", 32).unwrap();
        assert_eq!(bytes, b"hello");
        assert_eq!(mime, "image/png");
        assert!(decode_data_url_bytes("data:image/png;base64,aGVsbG8=", 4).is_err());
        assert!(decode_data_url_bytes("data:text/plain,hello", 32).is_err());

        let mut sess = Session::new().unwrap();
        assert!(!sess.relay.enabled);
        let cfg = RelayConfig {
            enabled: true,
            mode: crate::relay::RelayMode::Socks5,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "u".into(),
            password: "p".into(),
            route_webview_media: true,
        };
        sess.set_relay(cfg).unwrap();
        assert!(sess.relay.enabled);
        let cloned = sess.clone_shallow();
        assert!(cloned.relay.enabled);
        assert_eq!(cloned.relay.host, "10.7.12.233");
    }

    #[test]
    fn infer_media_data_url_mime_detects_audio_video_and_falls_back_cleanly() {
        assert_eq!(
            infer_media_data_url_mime("https://f.zdn.vn/clip.mp4?t=1", "application/octet-stream"),
            "video/mp4"
        );
        assert_eq!(
            infer_media_data_url_mime("https://f.zdn.vn/voice.aac", ""),
            "audio/aac"
        );
        assert_eq!(
            infer_media_data_url_mime("https://f.zdn.vn/blob", "audio/mpeg; charset=utf-8"),
            "audio/mpeg"
        );
        assert_eq!(
            infer_media_data_url_mime("https://f.zdn.vn/blob", "video/webm"),
            "video/webm"
        );
        assert_eq!(
            infer_media_data_url_mime("https://f.zdn.vn/blob", "application/octet-stream"),
            "image/png"
        );
    }

    #[test]
    fn infer_media_data_url_mime_never_returns_svg_or_malformed_mime() {
        for (url, ct, expected) in [
            ("https://f.zdn.vn/icon.svg", "image/svg+xml", "image/png"),
            (
                "https://f.zdn.vn/icon.svg?v=1",
                "IMAGE/SVG+XML; charset=utf-8",
                "image/png",
            ),
            ("https://f.zdn.vn/blob", "image/svg", "image/png"),
            ("https://f.zdn.vn/blob", "image/svg+xml", "image/png"),
            ("https://f.zdn.vn/clip.mp4", "image/svg+xml", "video/mp4"),
            ("https://f.zdn.vn/blob", "image/png,text/html", "image/png"),
            ("https://f.zdn.vn/blob", "image/x-bad\r\nX-Injected: 1", "image/png"),
            ("https://f.zdn.vn/blob", "image/webp", "image/webp"),
        ] {
            let got = infer_media_data_url_mime(url, ct);
            assert_ne!(got, "image/svg+xml");
            assert!(!got.contains("svg"));
            assert_eq!(got, expected, "url={url}, ct={ct}");
        }
    }

    #[test]
    fn media_host_allowlist_covers_verified_official_domains_and_blocks_unsafe() {
        for host in [
            "f10-zpc.zdn.vn",
            "zdn.vn",
            "s120-ava-talk.zadn.vn",
            "chat.zalo.me",
            "api.zaloapp.com",
            "static.zalo.vn",
            "photo.zalo.com",
            "files.zalo.cloud",
            "dl.zbox.vn",
            "cdn.zalo.video",
            "assets.zapps.me",
            "stc.zaloplatforms.com",
            "mp3.zing.vn",
            "file-stal-9.dlfl.vn",
            "fg41.dlfl.vn",
            "file-stal-1.dlfl.me",
            "file-stal-9.flchat.vn",
            "file-stal-9-te-vnso-pt-25.flchat.vn",
            "file-stal-9-te-vnso-pt-25.flchat.me",
            "video-stal-20.dlmd.vn",
            "video-stal-20.dlmd.me",
            "video-stal-20-te-vnno-cm-1.mdchat.vn",
            "video-stal-20-te-vnno-cm-1.mdchat.me",
            "FILES.ZALO.CLOUD.",
        ] {
            assert!(is_allowed_media_host(host), "expected allowed: {host}");
        }
        for unsafe_host in [
            "cdn.zalocdn.com",
            "zalocdn.com",
            "api.zaloapi.com",
            "bucket.vngcloud.vn",
            "cdn.vcdn.vn",
            "cdn.vcdn.com.vn",
            "evilzalo.cloud",
            "zalo.cloud.evil.com",
            "evildlfl.vn",
            "dlfl.vn.evil.com",
            "evilflchat.vn",
            "evilflchat.me",
            "dlmd.vn.evil.com",
            "mdchat.vn.evil.com",
            "mdchat.me.evil.com",
            ".zdn.vn",
            "",
        ] {
            assert!(
                !is_allowed_media_host(unsafe_host),
                "expected blocked: {unsafe_host}"
            );
        }
    }

    #[test]
    fn normalize_allowed_media_url_upgrades_and_sanitizes_diagnostics() {
        assert_eq!(
            normalize_allowed_media_url("http://f10-zpc.zdn.vn/photo.jpg?k=1")
                .unwrap()
                .as_str(),
            "https://f10-zpc.zdn.vn/photo.jpg?k=1"
        );
        assert_eq!(
            normalize_allowed_media_url("HTTP://files.zalo.cloud:80/doc.pdf")
                .unwrap()
                .as_str(),
            "https://files.zalo.cloud/doc.pdf"
        );
        assert_eq!(
            normalize_allowed_media_url("  //dl.zbox.vn/a/b.zip  ")
                .unwrap()
                .as_str(),
            "https://dl.zbox.vn/a/b.zip"
        );
        assert_eq!(
            normalize_allowed_media_url(r"https:\/\/f7-group-zf.zdn.vn\/dir\/file.pdf")
                .unwrap()
                .as_str(),
            "https://f7-group-zf.zdn.vn/dir/file.pdf"
        );
        assert_eq!(
            normalize_allowed_media_url(r"http:\/\/s160-ava-talk.zadn.vn\/14\/ava.jpg")
                .unwrap()
                .as_str(),
            "https://s160-ava-talk.zadn.vn/14/ava.jpg"
        );
        assert_eq!(
            normalize_allowed_media_url("http://file-stal-9.dlfl.vn/gr/test.xlsx")
                .unwrap()
                .as_str(),
            "https://file-stal-9.dlfl.vn/gr/test.xlsx"
        );
        assert_eq!(
            normalize_allowed_media_url("https://video-stal-20.dlmd.me/v.mp4")
                .unwrap()
                .as_str(),
            "https://video-stal-20.dlmd.me/v.mp4"
        );

        // Non-standard ports rejected with origin-only diagnostic (no path/query leak)
        let port_err =
            normalize_allowed_media_url("https://f10-zpc.zdn.vn:8443/secret/path?token=topsecret")
                .unwrap_err();
        assert_eq!(port_err, "https://f10-zpc.zdn.vn:8443");
        assert!(!port_err.contains("secret"));
        assert!(!port_err.contains("topsecret"));

        let http_port_err =
            normalize_allowed_media_url("http://f10-zpc.zdn.vn:8080/a?sig=xyz").unwrap_err();
        assert_eq!(http_port_err, "http://f10-zpc.zdn.vn:8080");
        assert!(!http_port_err.contains("sig"));

        // Userinfo rejected without leaking credentials or query
        let auth_err = normalize_allowed_media_url(
            "https://alice:hunter2@f10-zpc.zdn.vn/pic.jpg?token=leaked",
        )
        .unwrap_err();
        assert!(auth_err.starts_with("https://f10-zpc.zdn.vn"));
        assert!(!auth_err.contains("alice"));
        assert!(!auth_err.contains("hunter2"));
        assert!(!auth_err.contains("leaked"));

        // External host diagnostic exposes ONLY scheme://host
        let ext_err = normalize_allowed_media_url(
            "https://vnexpress.net/private/article?auth_token=abc123",
        )
        .unwrap_err();
        assert_eq!(ext_err, "https://vnexpress.net");
        assert!(!ext_err.contains("private"));
        assert!(!ext_err.contains("abc123"));

        // Non-http(s) & malformed inputs never echo secrets
        let blob_err =
            normalize_allowed_media_url("blob:https://tauri.localhost/secret-uuid").unwrap_err();
        assert_eq!(blob_err, "blob:");
        assert!(!blob_err.contains("secret-uuid"));

        let empty_err = normalize_allowed_media_url("   ").unwrap_err();
        assert_eq!(empty_err, "URL trống");
    }

    #[tokio::test]
    async fn download_media_bytes_error_includes_sanitized_origin_only() {
        let s = Session::new().unwrap();
        let err = s
            .download_media_bytes("https://untrusted.example/doc.pdf?secret_token=xyz", 1024)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Từ chối tải tệp từ URL không nằm trong danh sách CDN tin cậy"));
        assert!(err.contains("(https://untrusted.example)"));
        assert!(!err.contains("doc.pdf"));
        assert!(!err.contains("secret_token"));
    }

    #[test]
    fn decode_data_url_is_case_insensitive_and_login_normalizes_avatar() {
        let (bytes, mime) = decode_data_url_bytes("DATA:IMAGE/PNG;BASE64,aGVsbG8=", 32).unwrap();
        assert_eq!(bytes, b"hello");
        assert_eq!(mime, "IMAGE/PNG");

        let mut s = Session::new().unwrap();
        s.apply_login_config(&serde_json::json!({
            "avatar": "http://s160-ava-talk.zadn.vn/u/pic.jpg"
        }));
        assert_eq!(s.profile.avatar, "https://s160-ava-talk.zadn.vn/u/pic.jpg");

        s.apply_login_config(&serde_json::json!({
            "avatar": "https://s160-ava-talk.zadn.vn/default"
        }));
        assert_eq!(s.profile.avatar, "");
    }

    #[test]
    fn decode_data_url_rejects_oversized_base64_before_decoding() {
        // Exact boundary succeeds (5 bytes -> 8 base64 chars; 5.div_ceil(3)*4 = 8).
        let (bytes, mime) = decode_data_url_bytes("data:image/png;base64,aGVsbG8=", 5).unwrap();
        assert_eq!(bytes, b"hello");
        assert_eq!(mime, "image/png");

        // Invalid base64 characters exceeding max_bytes.div_ceil(3)*4 must fail with size error
        // BEFORE B64.decode runs (not a base64 syntax error).
        let oversized_invalid = format!("data:image/png;base64,{}", "!".repeat(13));
        let err = decode_data_url_bytes(&oversized_invalid, 9)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("vượt quá giới hạn"),
            "expected early size rejection before base64 decode, got: {err}"
        );

        // Valid oversized base64 payload is also rejected early.
        let oversized_valid = format!("data:application/octet-stream;base64,{}", B64.encode([0x41u8; 64]));
        let err2 = decode_data_url_bytes(&oversized_valid, 16)
            .unwrap_err()
            .to_string();
        assert!(err2.contains("vượt quá giới hạn"));

        // Zero max_bytes accepts empty payload and rejects non-empty payload early.
        let (empty_bytes, _) = decode_data_url_bytes("data:text/plain;base64,", 0).unwrap();
        assert!(empty_bytes.is_empty());
        let err_zero = decode_data_url_bytes("data:text/plain;base64,!!!!", 0)
            .unwrap_err()
            .to_string();
        assert!(err_zero.contains("vượt quá giới hạn"));
    }

    #[test]
    fn parse_content_disposition_filename_handles_plain_rfc5987_and_traversal() {
        assert_eq!(
            parse_content_disposition_filename(
                r#"attachment; filename="Check NAD Change PN.xlsx""#
            ),
            Some("Check NAD Change PN.xlsx".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(
                r#"attachment; filename="fallback.pdf"; filename*=UTF-8''B%C3%A1o%20c%C3%A1o.pdf"#
            ),
            Some("Báo cáo.pdf".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(
                r#"attachment; filename*=UTF-8'en'annual%20report.pdf; filename="fallback.pdf""#
            ),
            Some("annual report.pdf".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename="Q1; summary.xlsx"; size=42"#),
            Some("Q1; summary.xlsx".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename="Báo cáo.xlsx""#),
            Some("Báo cáo.xlsx".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(r#"attachment; filename="../../evil/report.docx""#),
            Some("report.docx".to_string())
        );
        assert_eq!(
            parse_content_disposition_filename(r#"inline; filename=archive.7z"#),
            Some("archive.7z".to_string())
        );
        assert_eq!(parse_content_disposition_filename("attachment"), None);
    }

    #[test]
    fn domains_wires_voice_call_with_backward_compatible_default_and_sanitization() {
        let def = Domains::default();
        assert_eq!(def.voice_call, "https://voicecall-wpa.chat.zalo.me");
        assert_eq!(
            def.candidates("voice_call"),
            vec!["https://voicecall-wpa.chat.zalo.me".to_string()]
        );

        // Legacy session JSON without `voice_call` must deserialize cleanly.
        let mut legacy = serde_json::to_value(&def).unwrap();
        legacy.as_object_mut().unwrap().remove("voice_call");
        let parsed: Domains = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.voice_call, "https://voicecall-wpa.chat.zalo.me");

        // Poisoned `voice_call` is sanitized back to official host while valid extra candidates are kept.
        let mut d = Domains {
            voice_call: "https://evil.example".into(),
            ..Default::default()
        };
        d.apply_service_map(&serde_json::json!({
            "voice_call": [
                "http://voicecall-wpa.chat.zalo.me",
                "https://evil.example",
                "https://voicecall2-wpa.chat.zalo.me"
            ]
        }));
        assert_eq!(d.voice_call, "https://voicecall-wpa.chat.zalo.me");
        let cands = d.candidates("voice_call");
        assert!(cands.contains(&"https://voicecall-wpa.chat.zalo.me".to_string()));
        assert!(cands.contains(&"https://voicecall2-wpa.chat.zalo.me".to_string()));
        assert!(!cands.iter().any(|u| u.contains("evil.example")));
    }

    #[test]
    fn decode_http_body_handles_empty_keepalive_bare_base64_and_rejects_html() {
        let empty = Session::decode_http_body(200, "").unwrap();
        assert_eq!(empty["error_code"], 0);
        assert!(empty["data"].is_null());

        let ws_only = Session::decode_http_body(200, "  \r\n\t ").unwrap();
        assert_eq!(ws_only["error_code"], 0);
        assert!(ws_only["data"].is_null());

        // Bare base64 AES body is wrapped so `maybe_decrypt` can decode it transparently.
        let mut s = Session::new().unwrap();
        s.set_zpw_enk("0123456789abcdef0123456789abcdef");
        let b64_enc =
            crypto::encode_aes(s.session_key_bytes(), r#"{"error_code":0,"data":{"ok":1}}"#)
                .unwrap();
        let wrapped = Session::decode_http_body(200, &b64_enc).unwrap();
        assert_eq!(wrapped["error_code"], 0);
        let dec = s.maybe_decrypt(wrapped, s.session_key_bytes());
        assert_eq!(dec["data"]["ok"], 1);

        // HTML error pages must still fail with Protocol(-1), never be mistaken for base64.
        let html_err = Session::decode_http_body(200, "<html><body>bad</body></html>").unwrap_err();
        assert!(matches!(
            html_err.downcast_ref::<SessionError>(),
            Some(SessionError::Protocol { code: -1, .. })
        ));

        // Session expired code 600 propagates as SessionError::Expired.
        let exp_err =
            Session::decode_http_body(200, r#"{"error_code":600,"error_message":"expired"}"#)
                .unwrap_err();
        assert!(matches!(
            exp_err.downcast_ref::<SessionError>(),
            Some(SessionError::Expired { code: 600 })
        ));
    }

    #[test]
    fn zpw_sek_header_and_zlang_cookie_attached_for_zalo_domains() {
        let mut s = Session::new().unwrap();
        s.set_zpw_sek("test_sek_token_123".into());

        // Jar contains both zpw_sek and _zlang=vi for lp_control and voice_call domains.
        for raw_url in [
            "https://p3-ctl-chat.zaloapp.com/",
            "https://voicecall-wpa.chat.zalo.me/api/voicecall/requestcall",
        ] {
            let url = reqwest::Url::parse(raw_url).unwrap();
            let cookies = s
                .jar
                .cookies(&url)
                .and_then(|v| v.to_str().ok().map(str::to_string))
                .unwrap_or_default();
            assert!(cookies.contains("zpw_sek=test_sek_token_123"), "missing zpw_sek for {raw_url}: {cookies}");
            assert!(cookies.contains("_zlang=vi"), "missing _zlang=vi for {raw_url}: {cookies}");

            let req = s
                .attach_zalo_auth_headers(s.http.get(url.clone()), &url)
                .build()
                .unwrap();
            assert_eq!(
                req.headers().get("zpw_sek").and_then(|v| v.to_str().ok()),
                Some("test_sek_token_123")
            );
        }
    }

    #[tokio::test]
    async fn download_media_to_temp_file_blocks_untrusted_url() {
        let s = Session::new().unwrap();
        let dir = std::env::temp_dir().join(format!("teza-dl-untrusted-{}", std::process::id()));
        let tmp = dir.join("out.part");
        let err = s
            .download_media_to_temp_file(
                "https://untrusted.example/big.mp4?token=secret",
                &tmp,
                |_, _| {},
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err.downcast_ref::<MediaDownloadError>(),
            Some(MediaDownloadError::UntrustedUrl { .. })
        ));
        assert!(!err.to_string().contains("secret"));
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stream_response_to_temp_file_streams_hashes_rejects_svg_and_flags_cdn_expiry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let payload: Vec<u8> = (0..(150 * 1024 + 19)).map(|i| (i % 239) as u8).collect();
        let expected_sha = {
            let d = Sha256::digest(&payload);
            d.iter().fold(String::with_capacity(64), |mut acc, b| {
                use std::fmt::Write;
                let _ = write!(acc, "{b:02x}");
                acc
            })
        };
        let payload_srv = payload.clone();

        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut req_buf = [0u8; 1024];
                let n = sock.read(&mut req_buf).await.unwrap();
                let req_str = String::from_utf8_lossy(&req_buf[..n]);
                let path = req_str
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                match path {
                    "/ok" => {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Disposition: attachment; filename=\"clip.mp4\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload_srv.len()
                        );
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                        sock.write_all(&payload_srv).await.unwrap();
                    }
                    "/svg" => {
                        let body = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                        sock.write_all(body).await.unwrap();
                    }
                    "/gone" => {
                        let hdr = "HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                    }
                    _ => {
                        let hdr = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                    }
                }
            }
        });

        let client = Client::new();
        let dir = std::env::temp_dir().join(format!("teza-dl-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // 1. Streaming 200 OK with SHA-256 + progress + Content-Disposition filename
        let ok_path = dir.join("ok.part");
        let resp_ok = client.get(format!("http://{addr}/ok")).send().await.unwrap();
        let mut progress_calls = Vec::new();
        let meta = stream_response_to_temp_file(
            resp_ok,
            &ok_path,
            &RelayConfig::default(),
            |got, total| {
                progress_calls.push((got, total));
            },
        )
        .await
        .unwrap();
        assert_eq!(meta.status_code, 200);
        assert_eq!(meta.mime, "video/mp4");
        assert_eq!(meta.file_name.as_deref(), Some("clip.mp4"));
        assert_eq!(meta.size_bytes, payload.len() as u64);
        assert_eq!(meta.sha256_hex, expected_sha);
        assert_eq!(std::fs::read(&ok_path).unwrap(), payload);
        assert!(progress_calls.first().unwrap().0 == 0);
        assert_eq!(progress_calls.last().unwrap().0, payload.len() as u64);

        // 2. Rejects image/svg+xml without creating temp file
        let svg_path = dir.join("svg.part");
        let resp_svg = client.get(format!("http://{addr}/svg")).send().await.unwrap();
        let svg_err =
            stream_response_to_temp_file(resp_svg, &svg_path, &RelayConfig::default(), |_, _| {})
                .await
                .unwrap_err();
        assert!(matches!(
            svg_err.downcast_ref::<MediaDownloadError>(),
            Some(MediaDownloadError::ForbiddenSvg)
        ));
        assert!(!svg_path.exists());

        // 3. Classifies 410 and 403 as MediaDownloadError::CdnExpired
        for (route, expected_code) in [("/gone", 410u16), ("/forbidden", 403u16)] {
            let p = dir.join(format!("exp_{expected_code}.part"));
            let r = client.get(format!("http://{addr}{route}")).send().await.unwrap();
            let err =
                stream_response_to_temp_file(r, &p, &RelayConfig::default(), |_, _| {})
                    .await
                    .unwrap_err();
            let dl_err = err.downcast_ref::<MediaDownloadError>().unwrap();
            assert!(dl_err.is_cdn_expired());
            assert!(matches!(
                dl_err,
                MediaDownloadError::CdnExpired { status } if *status == expected_code
            ));
            assert!(!p.exists());
        }

        server.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Debug)]
    struct TestChainError {
        msg: String,
        source: Option<Box<TestChainError>>,
    }

    impl fmt::Display for TestChainError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.msg)
        }
    }

    impl std::error::Error for TestChainError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source.as_deref().map(|s| s as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn format_media_network_error_distinguishes_policy_block_and_scrubs_secrets() {
        let url = reqwest::Url::parse(
            "https://file-stal-9.flchat.me/private/report.xlsx?token=TOP_SECRET_TOK&sig=999",
        )
        .unwrap();
        let relay = RelayConfig {
            enabled: true,
            mode: crate::relay::RelayMode::Socks5,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "corp_user".into(),
            password: "RelaySecretPassword!42".into(),
            route_webview_media: true,
        };

        // 1. Relay policy block walks full source() chain and scrubs URL paths/tokens & relay credentials
        let policy_err = TestChainError {
            msg: "error sending request for url (https://file-stal-9.flchat.me/private/report.xlsx?token=TOP_SECRET_TOK)".into(),
            source: Some(Box::new(TestChainError {
                msg: "error connecting to socks proxy socks5h://corp_user:RelaySecretPassword!42@10.7.12.233:1088".into(),
                source: Some(Box::new(TestChainError {
                    msg: "SOCKS error: connection not allowed by ruleset (0x02)".into(),
                    source: None,
                })),
            })),
        };
        let policy_msg = format_media_network_error(&url, &relay, &policy_err);
        assert!(policy_msg.contains("Máy chủ Relay (10.7.12.233:1088) từ chối kết nối tới https://file-stal-9.flchat.me"));
        assert!(policy_msg.contains("--extra-host file-stal-9.flchat.me"));
        assert!(policy_msg.contains("connection not allowed by ruleset"));
        assert!(!policy_msg.contains("private"));
        assert!(!policy_msg.contains("report.xlsx"));
        assert!(!policy_msg.contains("TOP_SECRET_TOK"));
        assert!(!policy_msg.contains("corp_user"));
        assert!(!policy_msg.contains("RelaySecretPassword!42"));

        // 2. HTTP CONNECT unsuccessful tunnel also reports Relay policy block cleanly
        let tunnel_err = TestChainError {
            msg: "client error (Connect)".into(),
            source: Some(Box::new(TestChainError {
                msg: "tunnel error: unsuccessful".into(),
                source: None,
            })),
        };
        let tunnel_msg = format_media_network_error(&url, &relay, &tunnel_err);
        assert!(tunnel_msg.contains("Máy chủ Relay (10.7.12.233:1088) từ chối kết nối tới https://file-stal-9.flchat.me"));
        assert!(tunnel_msg.contains("tunnel error: unsuccessful"));

        // 3. Generic transport error (e.g. host unreachable / connection reset) uses generic message
        let generic_err = TestChainError {
            msg: "client error (Connect)".into(),
            source: Some(Box::new(TestChainError {
                msg: "connection reset while reading https://file-stal-9.flchat.me/secret?k=leaked with RelaySecretPassword!42".into(),
                source: None,
            })),
        };
        let generic_msg = format_media_network_error(&url, &relay, &generic_err);
        assert!(generic_msg.starts_with("Không tải được từ https://file-stal-9.flchat.me ("));
        assert!(generic_msg.contains("connection reset"));
        assert!(!generic_msg.contains("từ chối kết nối"));
        assert!(!generic_msg.contains("leaked"));
        assert!(!generic_msg.contains("RelaySecretPassword!42"));

        // 4. Proxy auth failure (407) is NOT misclassified as policy block
        let auth_err = TestChainError {
            msg: "tunnel error: proxy authorization required (407)".into(),
            source: None,
        };
        let auth_msg = format_media_network_error(&url, &relay, &auth_err);
        assert!(auth_msg.starts_with("Không tải được từ https://file-stal-9.flchat.me ("));
        assert!(!auth_msg.contains("--extra-host"));

        // 5. With relay disabled, even a 403-like transport text uses generic format
        let disabled_relay = RelayConfig::default();
        let disabled_msg = format_media_network_error(&url, &disabled_relay, &policy_err);
        assert!(disabled_msg.starts_with("Không tải được từ https://file-stal-9.flchat.me ("));
        assert!(!disabled_msg.contains("Máy chủ Relay"));
    }

    #[tokio::test]
    async fn send_media_get_follows_manual_redirects_with_headers_and_retries_transient_eof() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let transient_hits = Arc::new(AtomicUsize::new(0));
        let transient_hits_srv = transient_hits.clone();
        let observed_headers = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let observed_headers_srv = observed_headers.clone();

        let server = tokio::spawn(async move {
            // Serve 5 redirect-chain requests (/r0..=/r4 -> /final) + 2 transient-retry requests
            // + 6 too-many-redirect requests (/loop0..=/loop5)
            for _ in 0..13 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut req_buf = [0u8; 2048];
                let n = sock.read(&mut req_buf).await.unwrap();
                let req_str = String::from_utf8_lossy(&req_buf[..n]).into_owned();
                let path = req_str
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                match path.as_str() {
                    "/r0" | "/r1" | "/r2" | "/r3" => {
                        observed_headers_srv.lock().await.push(req_str);
                        let next_idx: usize = path[2..].parse::<usize>().unwrap() + 1;
                        let hdr = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/r{next_idx}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                    }
                    "/r4" => {
                        observed_headers_srv.lock().await.push(req_str);
                        let body = b"redirect-ok";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                        sock.write_all(body).await.unwrap();
                    }
                    "/flaky" => {
                        let attempt = transient_hits_srv.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            // Drop immediately before writing any response bytes to trigger EOF retry.
                            drop(sock);
                        } else {
                            let body = b"retry-ok";
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            sock.write_all(hdr.as_bytes()).await.unwrap();
                            sock.write_all(body).await.unwrap();
                        }
                    }
                    p if p.starts_with("/loop") => {
                        let next_idx: usize = p[5..].parse::<usize>().unwrap() + 1;
                        let hdr = format!(
                            "HTTP/1.1 302 Found\r\nLocation: http://{addr}/loop{next_idx}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        sock.write_all(hdr.as_bytes()).await.unwrap();
                    }
                    _ => {
                        sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                    }
                }
            }
        });

        let session = Session::new().unwrap();
        let allow_local = |raw: &str| {
            reqwest::Url::parse(raw).map_err(|_| "invalid".to_string())
        };

        // 1. 4 manual redirects succeed (above old cap of 3) and send Referer + Origin + ZaloPC UA on every hop
        let resp = session
            .send_media_get_with_validator(&format!("http://{addr}/r0"), allow_local)
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"redirect-ok");
        let hdrs = observed_headers.lock().await;
        assert_eq!(hdrs.len(), 5);
        for req in hdrs.iter() {
            let lower = req.to_ascii_lowercase();
            assert!(lower.contains("referer: https://chat.zalo.me/"), "missing Referer: {req}");
            assert!(lower.contains("origin: https://chat.zalo.me"), "missing Origin: {req}");
            assert!(req.contains("ZaloPC/"), "expected ZaloPC user agent: {req}");
        }
        drop(hdrs);

        // 2. Transient EOF on first attempt retries once and succeeds
        let flaky = session
            .send_media_get_with_validator(&format!("http://{addr}/flaky"), allow_local)
            .await
            .unwrap();
        assert_eq!(flaky.bytes().await.unwrap().as_ref(), b"retry-ok");
        assert_eq!(transient_hits.load(Ordering::SeqCst), 2);

        // 3. Exceeding MAX_MANUAL_REDIRECT_HOPS (5) fails cleanly
        let too_many = session
            .send_media_get_with_validator(&format!("http://{addr}/loop0"), allow_local)
            .await
            .unwrap_err()
            .to_string();
        assert!(too_many.contains("quá nhiều chuyển hướng"));

        server.await.unwrap();
    }

    #[tokio::test]
    async fn send_media_get_does_not_retry_relay_policy_block_or_auth_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // 1. Mock SOCKS5 relay returning 0x02 (Connection not allowed by ruleset)
        let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks_addr = socks_listener.local_addr().unwrap();
        let socks_attempts = Arc::new(AtomicUsize::new(0));
        let socks_attempts_srv = socks_attempts.clone();
        let socks_srv = tokio::spawn(async move {
            let (mut s, _) = socks_listener.accept().await.unwrap();
            socks_attempts_srv.fetch_add(1, Ordering::SeqCst);
            let mut hello = [0u8; 4];
            let _ = s.read(&mut hello).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
            let mut req = [0u8; 128];
            let _ = s.read(&mut req).await.unwrap();
            s.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });

        let socks_session = Session::new_with_relay(RelayConfig {
            enabled: true,
            mode: crate::relay::RelayMode::Socks5,
            host: "127.0.0.1".into(),
            port: socks_addr.port(),
            username: String::new(),
            password: String::new(),
            route_webview_media: true,
        })
        .unwrap();

        let err = socks_session
            .send_media_get("https://file-stal-9.flchat.me/secret/doc.pdf?token=TOPSECRET")
            .await
            .unwrap_err()
            .to_string();
        socks_srv.await.unwrap();
        assert_eq!(socks_attempts.load(Ordering::SeqCst), 1);
        assert!(err.contains("Máy chủ Relay"));
        assert!(err.contains("https://file-stal-9.flchat.me"));
        assert!(err.contains("--extra-host file-stal-9.flchat.me"));
        assert!(!err.contains("TOPSECRET"));
        assert!(!err.contains("doc.pdf"));

        // 2. Mock HTTP CONNECT relay returning 407 Proxy Authentication Required
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let http_attempts = Arc::new(AtomicUsize::new(0));
        let http_attempts_srv = http_attempts.clone();
        let http_srv = tokio::spawn(async move {
            let (mut s, _) = http_listener.accept().await.unwrap();
            http_attempts_srv.fetch_add(1, Ordering::SeqCst);
            let mut req = [0u8; 512];
            let _ = s.read(&mut req).await.unwrap();
            s.write_all(
                b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"Relay\"\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        });

        let http_session = Session::new_with_relay(RelayConfig {
            enabled: true,
            mode: crate::relay::RelayMode::Http,
            host: "127.0.0.1".into(),
            port: http_addr.port(),
            username: "user1".into(),
            password: "SuperSecretRelayPw99".into(),
            route_webview_media: true,
        })
        .unwrap();

        let err407 = http_session
            .send_media_get("https://video-stal-20.dlmd.vn/priv/clip.mp4?sig=LEAKED")
            .await
            .unwrap_err()
            .to_string();
        http_srv.await.unwrap();
        assert_eq!(http_attempts.load(Ordering::SeqCst), 1);
        assert!(err407.contains("Không tải được từ https://video-stal-20.dlmd.vn"));
        assert!(!err407.contains("--extra-host"));
        assert!(!err407.contains("LEAKED"));
        assert!(!err407.contains("SuperSecretRelayPw99"));
    }
}
