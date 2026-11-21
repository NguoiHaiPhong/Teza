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
use thiserror::Error;

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

/// Returns `true` for trusted media/CDN hosts (`*.zalo.me`, `*.zaloapp.com`, `*.zdn.vn`, `*.zadn.vn`, `*.zalo.vn`).
pub fn is_allowed_media_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    is_allowed_zalo_host(&h)
        || h == "zdn.vn"
        || h.ends_with(".zdn.vn")
        || h == "zadn.vn"
        || h.ends_with(".zadn.vn")
        || h == "zalo.vn"
        || h.ends_with(".zalo.vn")
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
    let rest = raw
        .trim()
        .strip_prefix("data:")
        .context("URL data không hợp lệ")?;
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
    let bytes = B64
        .decode(payload.trim())
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
            self.profile.avatar = avatar;
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

        Ok(Self {
            imei: saved.imei,
            computer_name: saved.computer_name,
            first_launch_time: saved.first_launch_time,
            zpw_sek,
            secret_key,
            domains,
            profile: saved.profile,
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

        let resp = self
            .http
            .get(&url)
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
        self.send_raw(Method::GET, url, None).await
    }

    pub async fn post_form(&self, url: &str, body: String) -> Result<Value> {
        self.send_raw(Method::POST, url, Some(body)).await
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
            .attach_sek_cookie_if_missing(req, &parsed)
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

    fn attach_sek_cookie_if_missing(
        &self,
        req: reqwest::RequestBuilder,
        parsed: &reqwest::Url,
    ) -> reqwest::RequestBuilder {
        if let Some(sek) = self.effective_zpw_sek() {
            let jar_has_sek = self
                .jar
                .cookies(parsed)
                .and_then(|v| v.to_str().ok().map(|s| s.contains("zpw_sek=")))
                .unwrap_or(false);
            if !jar_has_sek {
                return req.header(COOKIE, format!("zpw_sek={sek}; _zlang=vi"));
            }
        }
        req
    }

    async fn send_raw(&self, method: Method, url: &str, form_body: Option<String>) -> Result<Value> {
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
        let mut req = self.attach_sek_cookie_if_missing(self.http.request(method, parsed.clone()), &parsed);
        if let Some(body) = form_body {
            req = req
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;
        Self::decode_http_response(resp).await
    }

    async fn decode_http_response(resp: reqwest::Response) -> Result<Value> {
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;
        if status != 200 {
            return Err(SessionError::Http {
                status,
                body: truncate(&text, 200),
            }
            .into());
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| SessionError::Protocol {
            code: -1,
            message: format!("Invalid JSON: {e}"),
        })?;
        let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        if is_session_expired_code(code) {
            return Err(SessionError::Expired { code }.into());
        }
        Ok(v)
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
                    self.profile.avatar = avt;
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
        if !is_allowed_media_url(url) {
            anyhow::bail!("Từ chối tải tệp từ URL không nằm trong danh sách CDN tin cậy");
        }
        let mut resp = self
            .media_http
            .get(url.trim())
            .header(REFERER, "https://chat.zalo.me/")
            .send()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("Tải tệp thất bại (HTTP {})", status.as_u16());
        }
        if let Some(len) = resp.content_length() {
            if len as usize > max_bytes {
                anyhow::bail!("Kích thước tệp vượt quá giới hạn cho phép");
            }
        }

        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();

        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| SessionError::Network(e.to_string()))?
        {
            if buf.len().saturating_add(chunk.len()) > max_bytes {
                anyhow::bail!("Kích thước tệp vượt quá giới hạn cho phép");
            }
            buf.extend_from_slice(&chunk);
        }
        Ok((buf, mime))
    }

    pub async fn download_media_data_url(&self, url: &str, max_bytes: usize) -> Result<String> {
        let (bytes, mime) = self.download_media_bytes(url, max_bytes).await?;
        let final_mime = infer_media_data_url_mime(url, &mime);
        Ok(format!("data:{final_mime};base64,{}", B64.encode(&bytes)))
    }
}

pub fn infer_media_data_url_mime(url: &str, content_type: &str) -> String {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if ct.starts_with("image/") || ct.starts_with("audio/") || ct.starts_with("video/") {
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
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ZaloPC/26.10.11 Chrome/122.0.0.0 Electron/29.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(35));
    relay
        .apply_to_client_builder(builder)?
        .build()
        .context("Không khởi tạo được HTTP client")
}

fn build_media_http_client(relay: &RelayConfig) -> Result<Client> {
    let redirect_policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.stop()
        } else if is_allowed_media_url(attempt.url().as_str()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let builder = Client::builder()
        .redirect(redirect_policy)
        .user_agent(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) LiteChat/",
            env!("CARGO_PKG_VERSION"),
            " Safari/537.36"
        ))
        .timeout(std::time::Duration::from_secs(60));
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
        }
    }
    for (_, host) in endpoints::SERVICE_MAP_V3 {
        if let Ok(url) = reqwest::Url::parse(host) {
            jar.add_cookie_str(&format!("zpw_sek={sek}; Path=/"), &url);
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
}
