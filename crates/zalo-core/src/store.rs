//! Encrypted SQLite persistence (AES-256-GCM with DPAPI-sealed key) + in-memory FTS5 search.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64_URL},
    Engine,
};
use chrono::{
    DateTime, Datelike, Days, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, TimeZone,
    Utc,
};
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::chat::parser::{canonicalize_reaction_emoji, clean_group_id, normalize_ts_ms};
use crate::chat::{
    contains_cjk_or_hangul, extract_korean_choseong, is_korean_choseong_query, strip_vi_accents,
    ChatFolderDto, FolderFilter, MessageDto, PollDto, QuoteRef, ReactionSummary, ThreadDto,
    ThreadKind,
};
use crate::translate::TranslationResult;
use crate::{data_dir, dpapi};

const ENC_PREFIX: &str = "enc:v1:";
const KEY_CHECK_VALUE: &str = "litechat-db-v1";

/// `(msg_id, cli_msg_id, sender_id, msg_type, ts)` for `seenv2` read receipts.
pub type IncomingSeenMeta = (String, Option<String>, String, String, i64);

struct DbCipher {
    cipher: Aes256Gcm,
    mac_key: [u8; 32],
}

impl DbCipher {
    fn from_raw_key(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key"),
            mac_key: *key,
        }
    }

    fn key_path_for_db(db_path: &Path) -> PathBuf {
        let mut p = db_path.as_os_str().to_os_string();
        p.push(".key");
        PathBuf::from(p)
    }

    fn load_or_create(db_path: &Path) -> Result<(Self, bool)> {
        let key_path = Self::key_path_for_db(db_path);
        if key_path.exists() {
            let sealed = std::fs::read_to_string(&key_path)
                .with_context(|| format!("Không đọc được tệp khóa {}", key_path.display()))?;
            let plain = dpapi::unprotect_secret(sealed.trim())
                .context("Không giải mã được khóa database bằng DPAPI")?;
            let raw = B64
                .decode(plain.trim())
                .context("Khóa database giải mã không hợp lệ")?;
            let key: [u8; 32] = raw
                .try_into()
                .map_err(|_| anyhow!("Độ dài khóa database không đúng 32 byte"))?;
            return Ok((Self::from_raw_key(&key), true));
        }

        let mut key = [0u8; 32];
        getrandom::fill(&mut key).context("Không tạo được khóa ngẫu nhiên CSPRNG")?;
        let sealed = dpapi::protect_secret(&B64.encode(key))?;
        data_dir::write_private_atomic(&key_path, sealed.as_bytes())?;
        Ok((Self::from_raw_key(&key), false))
    }

    fn encrypt_str(&self, aad: &[u8], plain: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; 12];
        getrandom::fill(&mut nonce_bytes).context("OS CSPRNG failure")?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plain.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| anyhow!("Mã hóa bản ghi lịch sử thất bại"))?;
        let mut blob = Vec::with_capacity(12 + ct.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ct);
        Ok(format!("{ENC_PREFIX}{}", B64.encode(blob)))
    }

    fn encrypt_opt(&self, aad: &[u8], plain: Option<&str>) -> Result<Option<String>> {
        plain.map(|s| self.encrypt_str(aad, s)).transpose()
    }

    fn decrypt_str(&self, aad: &[u8], stored: &str) -> Result<String> {
        let rest = stored
            .strip_prefix(ENC_PREFIX)
            .ok_or_else(|| anyhow!("Dữ liệu chưa được mã hóa đúng định dạng"))?;
        let blob = B64
            .decode(rest.trim())
            .context("Base64 ciphertext không hợp lệ")?;
        if blob.len() < 12 + 16 {
            bail!("Bản ghi mã hóa quá ngắn hoặc bị hỏng");
        }
        let nonce = Nonce::from_slice(&blob[..12]);
        let pt = self
            .cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &blob[12..],
                    aad,
                },
            )
            .map_err(|_| anyhow!("Giải mã lịch sử thất bại (sai khóa hoặc dữ liệu bị sửa đổi)"))?;
        String::from_utf8(pt).context("Dữ liệu giải mã không phải UTF-8 hợp lệ")
    }

    fn decrypt_opt(&self, aad: &[u8], stored: Option<String>) -> Result<Option<String>> {
        stored.map(|s| self.decrypt_str(aad, &s)).transpose()
    }

    fn decrypt_or_legacy(&self, aad: &[u8], value: &str) -> Result<String> {
        if value.starts_with(ENC_PREFIX) {
            self.decrypt_str(aad, value)
        } else {
            Ok(value.to_string())
        }
    }

    fn encrypt_if_plain(&self, aad: &[u8], value: &str) -> Result<String> {
        if value.starts_with(ENC_PREFIX) {
            Ok(value.to_string())
        } else {
            self.encrypt_str(aad, value)
        }
    }

    fn keyed_translation_mac(&self, msg_id: &str, text: &str) -> String {
        let mut ipad = [0x36u8; 64];
        let mut opad = [0x5cu8; 64];
        for (i, &kb) in self.mac_key.iter().enumerate() {
            ipad[i] ^= kb;
            opad[i] ^= kb;
        }

        let mut inner = Md5::new();
        inner.update(ipad);
        inner.update(b"tr-v2:");
        inner.update(msg_id.as_bytes());
        inner.update(b"\0");
        inner.update(text.trim().as_bytes());
        let inner_digest = inner.finalize();

        let mut outer = Md5::new();
        outer.update(opad);
        outer.update(inner_digest);
        let out = outer.finalize();
        out.iter().fold(String::with_capacity(32), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }
}

fn thread_aad(thread_id: &str) -> Vec<u8> {
    format!("thread:{thread_id}").into_bytes()
}

fn msg_aad(thread_id: &str, msg_id: &str) -> Vec<u8> {
    format!("msg:{thread_id}:{msg_id}").into_bytes()
}

fn folder_aad(folder_id: &str) -> Vec<u8> {
    format!("folder:{folder_id}").into_bytes()
}

fn translation_aad(msg_id: &str, target_lang: &str) -> Vec<u8> {
    format!("tr:{msg_id}:{target_lang}").into_bytes()
}

fn message_media_aad(thread_id: &str, msg_id: &str, variant: &str) -> Vec<u8> {
    format!("msg_media:{thread_id}:{msg_id}:{variant}").into_bytes()
}

fn ai_msg_aad(id: &str) -> Vec<u8> {
    format!("ai_msg:{id}").into_bytes()
}

fn ai_session_aad(id: &str) -> Vec<u8> {
    format!("ai_session:{id}").into_bytes()
}

fn sched_aad(id: &str) -> Vec<u8> {
    format!("sched:{id}").into_bytes()
}

fn api_audit_aad(id: i64) -> Vec<u8> {
    format!("api_audit:{id}").into_bytes()
}

fn api_idem_aad(client_id: &str, idem_key: &str) -> Vec<u8> {
    format!("api_idem:{client_id}:{idem_key}").into_bytes()
}

pub const MEDIA_LOCAL_STATE_MISSING: i32 = 0;
pub const MEDIA_LOCAL_STATE_LOCAL: i32 = 1;

pub const MEDIA_CDN_STATUS_UNKNOWN: i32 = 0;
pub const MEDIA_CDN_STATUS_OK: i32 = 1;
pub const MEDIA_CDN_STATUS_EXPIRED: i32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaObjectRecord {
    pub sha256: String,
    pub kind: String,
    pub mime: String,
    pub size_bytes: u64,
    pub thumb_sha256: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub local_state: i32,
    pub local_rel_path: Option<String>,
    pub is_encrypted: bool,
    pub pin_count: u32,
    pub created_ts: i64,
    pub last_accessed_ts: i64,
    pub cloud_provider: Option<String>,
    pub cloud_file_id: Option<String>,
    pub cloud_encrypted: bool,
    pub cloud_key_ver: i32,
    pub cloud_synced_ts: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageMediaRecord {
    pub thread_id: String,
    pub msg_id: String,
    pub variant: String,
    pub url_hash: String,
    pub sha256: Option<String>,
    pub original_name: Option<String>,
    pub saved_user_path: Option<String>,
    pub cdn_status: i32,
    pub updated_ts: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaVaultStatsDto {
    pub total_bytes: u64,
    pub object_count: u64,
    pub pinned_bytes: u64,
    pub pinned_count: u64,
    pub image_bytes: u64,
    pub video_bytes: u64,
    pub voice_bytes: u64,
    pub file_bytes: u64,
    pub thumb_bytes: u64,
    pub cloud_synced_count: u64,
    pub cloud_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveThreadUsageDto {
    pub thread_id: String,
    pub thread_name: String,
    pub total_bytes: u64,
    pub object_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GDriveCloudItemDto {
    pub sha256: String,
    pub cloud_file_id: String,
    pub kind: String,
    pub mime: String,
    pub size_bytes: u64,
    pub original_name: Option<String>,
    pub thread_id: Option<String>,
    pub thread_name: Option<String>,
    pub msg_id: Option<String>,
    pub cloud_synced_ts: i64,
    pub local_state: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudMediaBreakdown {
    pub total_bytes: u64,
    pub total_count: u64,
    pub images_bytes: u64,
    pub videos_bytes: u64,
    pub voice_bytes: u64,
    pub files_bytes: u64,
    pub pending_count: u64,
    pub pending_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryTranslationRecord {
    pub msg_id: String,
    pub target_lang: String,
    pub provider: String,
    pub translated_text: String,
    pub detected_source_lang: Option<String>,
    pub skipped: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCloudMediaRecord {
    pub thread_id: String,
    pub msg_id: String,
    pub variant: String,
    pub url_hash: String,
    pub sha256: String,
    pub kind: String,
    pub mime: String,
    pub size_bytes: u64,
    pub original_name: Option<String>,
    pub cloud_provider: String,
    pub cloud_file_id: String,
    pub cloud_key_ver: i32,
    pub cloud_synced_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDeletedMessageTombstone {
    pub thread_id: String,
    pub msg_id: String,
    pub deleted_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryDeletedThreadTombstone {
    pub thread_id: String,
    pub deleted_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCloudDeleteRecord {
    pub sha256: String,
    pub cloud_file_id: String,
    pub reason: String,
    pub queued_ts: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryDeltaImportReport {
    pub applied_messages: usize,
    pub deleted_messages: usize,
    pub deleted_threads: usize,
    pub orphaned_local_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryDeltaBatch {
    pub version: u32,
    pub uid: String,
    pub device_id: String,
    pub exported_ts: i64,
    #[serde(default)]
    pub threads: Vec<ThreadDto>,
    #[serde(default)]
    pub messages: Vec<MessageDto>,
    #[serde(default)]
    pub folders: Vec<ChatFolderDto>,
    #[serde(default)]
    pub thread_folders: Vec<(String, String)>,
    #[serde(default)]
    pub translations: Vec<HistoryTranslationRecord>,
    #[serde(default)]
    pub cloud_media: Vec<HistoryCloudMediaRecord>,
    #[serde(default)]
    pub deleted_messages: Vec<HistoryDeletedMessageTombstone>,
    #[serde(default)]
    pub deleted_threads: Vec<HistoryDeletedThreadTombstone>,
    /// True when more messages exist strictly after `max_message_ts()` beyond `limit_msgs`.
    #[serde(skip)]
    pub messages_truncated: bool,
    /// True when more cloud media links exist strictly after `max_cloud_media_cursor_ts`.
    #[serde(skip)]
    pub cloud_media_truncated: bool,
    /// True when more message mutations or deletion tombstones exist strictly after `max_mutation_ts_cursor`.
    #[serde(skip)]
    pub mutations_truncated: bool,
    /// Maximum `messages.ts` across messages selected via `ts >= since_ts` (server-clock cursor).
    #[serde(skip)]
    pub max_message_ts_cursor: Option<i64>,
    /// Maximum `MAX(cloud_synced_ts, message_media.updated_ts)` across exported `cloud_media` rows.
    #[serde(skip)]
    pub max_cloud_media_cursor_ts: Option<i64>,
    /// Maximum local mutation timestamp across mutated messages and deletion tombstones.
    #[serde(skip)]
    pub max_mutation_ts_cursor: Option<i64>,
}

impl HistoryDeltaBatch {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn max_message_ts(&self) -> Option<i64> {
        self.messages.iter().map(|m| m.ts).max()
    }
}

const API_AUDIT_LOG_MAX_ROWS: i64 = 500;
const API_IDEMPOTENCY_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
#[cfg(test)]
const DAY_MS: i64 = 86_400_000;
#[cfg(test)]
const WEEK_MS: i64 = 7 * DAY_MS;

const AI_SESSION_DEFAULT_TITLE: &str = "Cuộc trò chuyện mới";
const AI_SESSION_LEGACY_FALLBACK_TITLE: &str = "Cuộc trò chuyện AI";
const AI_SESSION_MAX_ID_CHARS: usize = 64;
const AI_SESSION_MAX_TITLE_CHARS: usize = 120;
const AI_SESSION_AUTO_TITLE_CHARS: usize = 48;
const AI_SESSION_PREVIEW_CHARS: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiStoredMessageDto {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub backend: String,
    pub ts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiSessionDto {
    pub id: String,
    pub title: String,
    pub created_ts: i64,
    pub updated_ts: i64,
    pub message_count: usize,
    pub last_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledMessageDto {
    pub id: String,
    pub thread_id: String,
    pub thread_title: String,
    pub thread_kind: String,
    pub text: String,
    #[serde(default)]
    pub attachment_path: String,
    pub send_at_ts: i64,
    #[serde(default)]
    pub next_attempt_ts: Option<i64>,
    #[serde(default)]
    pub bypass_misfire: bool,
    #[serde(default)]
    pub anchor_day: Option<u32>,
    pub recurrence: String,
    pub misfire_policy: String,
    pub misfire_grace_secs: i64,
    pub status: String,
    pub retry_count: u32,
    #[serde(default)]
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub sent_at_ts: i64,
}

fn default_sched_thread_kind() -> String {
    "direct".to_string()
}

fn default_sched_recurrence() -> String {
    "once".to_string()
}

fn default_sched_misfire_policy() -> String {
    "send_on_resume".to_string()
}

fn default_sched_misfire_grace_secs() -> i64 {
    600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewScheduledMessageInput {
    pub thread_id: String,
    #[serde(default)]
    pub thread_title: String,
    #[serde(default = "default_sched_thread_kind")]
    pub thread_kind: String,
    pub text: String,
    #[serde(default)]
    pub attachment_path: String,
    pub send_at_ts: i64,
    #[serde(default = "default_sched_recurrence")]
    pub recurrence: String,
    #[serde(default = "default_sched_misfire_policy")]
    pub misfire_policy: String,
    #[serde(default = "default_sched_misfire_grace_secs")]
    pub misfire_grace_secs: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateScheduledMessageInput {
    #[serde(default)]
    pub thread_title: Option<String>,
    #[serde(default)]
    pub thread_kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachment_path: Option<String>,
    #[serde(default)]
    pub send_at_ts: Option<i64>,
    #[serde(default)]
    pub recurrence: Option<String>,
    #[serde(default)]
    pub misfire_policy: Option<String>,
    #[serde(default)]
    pub misfire_grace_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiClientDto {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub allowed_thread_ids: Vec<String>,
    pub rate_limit_per_min: u32,
    pub enabled: bool,
    pub created_at: i64,
    pub last_used_at: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedApiClientDto {
    pub client: ApiClientDto,
    pub raw_token: String,
}

impl fmt::Debug for CreatedApiClientDto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedApiClientDto")
            .field("client", &self.client)
            .field("raw_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiAuditLogDto {
    pub id: i64,
    pub client_id: String,
    pub client_name: String,
    pub method: String,
    pub path: String,
    pub target_thread_id: String,
    pub status_code: u16,
    pub outcome: String,
    pub detail: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiIdempotencyRecord {
    pub status_code: u16,
    pub response_json: String,
    #[serde(default)]
    pub request_hash: String,
}

fn days_in_month(year: i32, month: u32) -> Option<u32> {
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    next_month_first.pred_opt().map(|d| d.day())
}

fn add_months_with_anchor(
    base_year: i32,
    base_month: u32,
    months_to_add: u32,
    desired_day: u32,
) -> Option<NaiveDate> {
    let zero_based = (base_month.checked_sub(1)? as i64).checked_add(i64::from(months_to_add))?;
    let year_delta = zero_based.div_euclid(12);
    let target_month = (zero_based.rem_euclid(12) + 1) as u32;
    let target_year = base_year.checked_add(i32::try_from(year_delta).ok()?)?;
    let max_day = days_in_month(target_year, target_month)?;
    let clamped_day = desired_day.clamp(1, max_day);
    NaiveDate::from_ymd_opt(target_year, target_month, clamped_day)
}

fn resolve_local_datetime_in_tz<Tz: TimeZone>(
    tz: &Tz,
    naive: NaiveDateTime,
) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            return Some(dt);
        }
        chrono::LocalResult::None => {}
    }
    // Spring-forward DST gap (`LocalResult::None`): advance minute-by-minute (up to 4 hours)
    // to the earliest valid local instant right after the skipped window so recurring schedules
    // never degrade into `None` (one-off).
    for mins in 1..=240_i64 {
        let shifted = naive.checked_add_signed(ChronoDuration::minutes(mins))?;
        match tz.from_local_datetime(&shifted) {
            chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
                return Some(dt);
            }
            chrono::LocalResult::None => {}
        }
    }
    None
}

fn local_day_of_month(ts_ms: i64) -> Option<u32> {
    let ms = normalize_ts_ms(ts_ms);
    DateTime::<Utc>::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&Local).day())
}

/// Computes the next occurrence timestamp (in Unix ms) strictly after `max(send_at_ts_ms, now_ms)`
/// for recurring schedules (`"daily"`, `"weekly"`, `"monthly"`) in the given timezone `tz`,
/// preserving local wall-clock `HH:MM:SS` across DST transitions and clamping `anchor_day`
/// (`1..=31`) to each target month's last day.
/// Returns `None` for `"once"` or unrecognized recurrence values.
pub fn compute_next_occurrence_in_tz<Tz: TimeZone>(
    tz: &Tz,
    send_at_ts_ms: i64,
    anchor_day: Option<u32>,
    recurrence: &str,
    now_ms: i64,
) -> Option<i64> {
    let base_ms = normalize_ts_ms(send_at_ts_ms);
    let target_min = normalize_ts_ms(now_ms).max(base_ms);
    let base_local = DateTime::<Utc>::from_timestamp_millis(base_ms)?.with_timezone(tz);
    let target_local = DateTime::<Utc>::from_timestamp_millis(target_min)?.with_timezone(tz);
    let base_date = base_local.date_naive();
    let wall_time = base_local.time();

    match recurrence.trim().to_ascii_lowercase().as_str() {
        "daily" => {
            let day_diff = (target_local.date_naive() - base_date).num_days();
            let mut step_days = (day_diff - 1).max(1);
            loop {
                let cand_date = base_date.checked_add_days(Days::new(step_days as u64))?;
                let cand_dt = resolve_local_datetime_in_tz(tz, cand_date.and_time(wall_time))?;
                let cand_ms = cand_dt.timestamp_millis();
                if cand_ms > target_min {
                    return Some(cand_ms);
                }
                step_days = step_days.checked_add(1)?;
            }
        }
        "weekly" => {
            let day_diff = (target_local.date_naive() - base_date).num_days();
            let approx_weeks = day_diff.max(0) / 7;
            let mut step_weeks = (approx_weeks - 1).max(1);
            loop {
                let days = (step_weeks as u64).checked_mul(7)?;
                let cand_date = base_date.checked_add_days(Days::new(days))?;
                let cand_dt = resolve_local_datetime_in_tz(tz, cand_date.and_time(wall_time))?;
                let cand_ms = cand_dt.timestamp_millis();
                if cand_ms > target_min {
                    return Some(cand_ms);
                }
                step_weeks = step_weeks.checked_add(1)?;
            }
        }
        "monthly" => {
            let desired_day = anchor_day
                .filter(|&d| (1..=31).contains(&d))
                .unwrap_or_else(|| base_date.day());
            let approx_months = ((target_local.year() - base_local.year()) * 12
                + (target_local.month() as i32 - base_local.month() as i32))
                .max(1) as u32;
            let mut months = approx_months.saturating_sub(1).max(1);
            loop {
                let cand_date = add_months_with_anchor(
                    base_local.year(),
                    base_local.month(),
                    months,
                    desired_day,
                )?;
                let cand_dt = resolve_local_datetime_in_tz(tz, cand_date.and_time(wall_time))?;
                let cand_ms = cand_dt.timestamp_millis();
                if cand_ms > target_min {
                    return Some(cand_ms);
                }
                months = months.checked_add(1)?;
            }
        }
        _ => None,
    }
}

/// Computes the next occurrence timestamp (in Unix ms) strictly after `max(send_at_ts_ms, now_ms)`
/// in `chrono::Local` time using an optional persisted `anchor_day` (`1..=31`) for monthly schedules.
pub fn compute_next_occurrence_ts_with_anchor(
    send_at_ts_ms: i64,
    anchor_day: Option<u32>,
    recurrence: &str,
    now_ms: i64,
) -> Option<i64> {
    compute_next_occurrence_in_tz(&Local, send_at_ts_ms, anchor_day, recurrence, now_ms)
}

/// Computes the next occurrence timestamp (in Unix ms) strictly after `max(send_at_ts_ms, now_ms)`
/// for recurring schedules (`"daily"`, `"weekly"`, `"monthly"`) in `chrono::Local` time.
/// Returns `None` for `"once"` or unrecognized recurrence values.
pub fn compute_next_occurrence_ts(
    send_at_ts_ms: i64,
    recurrence: &str,
    now_ms: i64,
) -> Option<i64> {
    compute_next_occurrence_ts_with_anchor(send_at_ts_ms, None, recurrence, now_ms)
}

/// Computes the SHA-256 lowercase hex digest of a raw Local Automation API token.
pub fn hash_api_token(raw_token: &str) -> String {
    let mut hasher = <Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, raw_token.as_bytes());
    let digest = sha2::Digest::finalize(hasher);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Generates a cryptographically random Local Automation API token (`teza_` + 32 random bytes base64url).
pub fn generate_api_token() -> Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).context("Không tạo được token ngẫu nhiên CSPRNG")?;
    Ok(format!("teza_{}", B64_URL.encode(raw)))
}

fn generate_prefixed_id(prefix: &str) -> Result<String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).context("Không tạo được định danh ngẫu nhiên CSPRNG")?;
    let mut hex = String::with_capacity(prefix.len() + 1 + 32);
    hex.push_str(prefix);
    hex.push('_');
    for b in raw {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn normalize_sched_thread_kind(raw: &str) -> Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "direct" => Ok("direct"),
        "group" => Ok("group"),
        "cloud" => Ok("cloud"),
        other => bail!("Loại hội thoại không hợp lệ: {other}"),
    }
}

fn normalize_sched_recurrence(raw: &str) -> Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "once" => Ok("once"),
        "daily" => Ok("daily"),
        "weekly" => Ok("weekly"),
        "monthly" => Ok("monthly"),
        other => bail!("Chu kỳ lặp không hợp lệ: {other}"),
    }
}

fn normalize_sched_misfire_policy(raw: &str) -> Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "send_on_resume" => Ok("send_on_resume"),
        "skip" => Ok("skip"),
        "ask" => Ok("ask"),
        other => bail!("Chính sách xử lý lỡ hẹn không hợp lệ: {other}"),
    }
}

fn normalize_sched_status_token(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" => Some("pending"),
        "sending" => Some("sending"),
        "sent" => Some("sent"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        "missed" => Some("missed"),
        _ => None,
    }
}

fn sanitize_api_scopes(scopes: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for raw in scopes {
        let s = raw.trim().to_ascii_lowercase();
        if s.is_empty() {
            continue;
        }
        let canonical = match s.as_str() {
            "send" => "send",
            "schedule" => "schedule",
            "read_threads" => "read_threads",
            other => bail!("Quyền API không hợp lệ: {other}"),
        };
        if !out.iter().any(|x| x == canonical) {
            out.push(canonical.to_string());
        }
    }
    if out.is_empty() {
        bail!("Cần chọn ít nhất một quyền (scope) cho ứng dụng API");
    }
    Ok(out)
}

fn sanitize_allowed_thread_ids(ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in ids {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !out.iter().any(|x| x == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn sanitize_rate_limit_per_min(raw: u32) -> u32 {
    if raw == 0 {
        20
    } else {
        raw.clamp(1, 600)
    }
}

pub struct MessageStore {
    conn: Connection,
    fts_conn: Connection,
    cipher: DbCipher,
}

fn is_synthetic_poll_msg_id(msg_id: &str) -> bool {
    msg_id.starts_with("poll-") || msg_id.starts_with("local-poll-")
}

fn parse_poll_dto_from_attachment_name(attachment_name: Option<&str>) -> Option<PollDto> {
    attachment_name
        .and_then(|s| s.strip_prefix("teza-poll:"))
        .and_then(|json| serde_json::from_str::<PollDto>(json).ok())
}

fn extract_poll_id_from_message(msg: &MessageDto) -> Option<i64> {
    if let Some(dto) = parse_poll_dto_from_attachment_name(msg.attachment_name.as_deref()) {
        if dto.poll_id > 0 {
            return Some(dto.poll_id);
        }
    }
    msg.msg_id
        .strip_prefix("poll-")
        .or_else(|| msg.msg_id.strip_prefix("local-poll-"))
        .and_then(|rest| rest.trim().parse::<i64>().ok())
        .filter(|&id| id > 0)
}

fn poll_thread_id_candidates(thread_id: &str, extra_thread_id: Option<&str>) -> Vec<String> {
    let mut tid_candidates: Vec<String> = Vec::with_capacity(4);
    for candidate in [Some(thread_id), extra_thread_id].into_iter().flatten() {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !tid_candidates.iter().any(|t| t == trimmed) {
            tid_candidates.push(trimmed.to_string());
        }
        let bare = clean_group_id(trimmed);
        if !bare.is_empty() && !tid_candidates.iter().any(|t| t == bare) {
            tid_candidates.push(bare.to_string());
        }
    }
    tid_candidates
}

fn is_raw_numeric_uid_like(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if let Some((lhs, rhs)) = t.split_once('_') {
        return !lhs.is_empty()
            && !rhs.is_empty()
            && lhs.chars().all(|c| c.is_ascii_digit())
            && rhs.chars().all(|c| c.is_ascii_digit());
    }
    false
}

fn is_placeholder_thread_title(title: &str, thread_id: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty() || trimmed == thread_id.trim() || is_raw_numeric_uid_like(trimmed)
}

fn fold_poll_dto(acc: Option<PollDto>, next: Option<PollDto>) -> Option<PollDto> {
    match (acc, next) {
        (Some(existing), Some(incoming)) => {
            Some(crate::chat::threads::merge_poll_dto(&existing, &incoming))
        }
        (Some(existing), None) => Some(existing),
        (None, Some(incoming)) => Some(incoming),
        (None, None) => None,
    }
}

fn format_merged_poll_text(question: &str) -> Option<String> {
    let q = question.trim();
    if !q.is_empty() && q != "Bình chọn" {
        Some(format!("Bình chọn: {q}"))
    } else {
        None
    }
}

fn normalize_reaction_summaries(reactions: &mut Vec<ReactionSummary>) -> bool {
    let original = reactions.clone();
    let mut normalized: Vec<ReactionSummary> = Vec::with_capacity(reactions.len());
    for item in reactions.drain(..) {
        if item.count == 0 {
            continue;
        }
        let Some(canon) = canonicalize_reaction_emoji(&item.emoji) else {
            continue;
        };
        if let Some(existing) = normalized.iter_mut().find(|r| r.emoji == canon) {
            existing.count = existing.count.saturating_add(item.count);
            existing.me |= item.me;
        } else {
            normalized.push(ReactionSummary {
                emoji: canon,
                count: item.count,
                me: item.me,
            });
        }
    }
    *reactions = normalized;
    *reactions != original
}

impl MessageStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            data_dir::ensure_private_dir(parent)?;
        }
        let db_existed = path.exists();
        let (cipher, _had_key_file) = DbCipher::load_or_create(path)?;

        let conn = Connection::open(path)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let fts_conn = Connection::open_in_memory()?;
        fts_conn.execute_batch(
            "CREATE VIRTUAL TABLE messages_fts USING fts5(
                msg_id UNINDEXED,
                thread_id UNINDEXED,
                ts UNINDEXED,
                sender_name,
                text_norm
            );",
        )?;

        let store = Self {
            conn,
            fts_conn,
            cipher,
        };
        store.migrate(db_existed)?;
        store.rebuild_in_memory_fts()?;
        Ok(store)
    }

    fn migrate(&self, db_existed: bool) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS store_meta (
                k TEXT PRIMARY KEY,
                v TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS threads (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                title_norm TEXT NOT NULL,
                avatar TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL DEFAULT 'direct',
                last_preview TEXT NOT NULL DEFAULT '',
                last_ts INTEGER NOT NULL DEFAULT 0,
                unread_count INTEGER NOT NULL DEFAULT 0,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                is_muted INTEGER NOT NULL DEFAULT 0
            );
            DROP INDEX IF EXISTS idx_threads_sort;
            CREATE INDEX IF NOT EXISTS idx_threads_sort
                ON threads(is_pinned DESC, last_ts DESC);

            CREATE TABLE IF NOT EXISTS chat_folders (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                icon TEXT NOT NULL DEFAULT 'folder',
                config_json TEXT NOT NULL,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                msg_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                sender_name TEXT NOT NULL,
                text TEXT NOT NULL,
                text_norm TEXT NOT NULL,
                msg_type TEXT NOT NULL DEFAULT 'chat.text',
                ts INTEGER NOT NULL,
                is_outgoing INTEGER NOT NULL DEFAULT 0,
                is_recalled INTEGER NOT NULL DEFAULT 0,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                quote_json TEXT,
                reactions_json TEXT NOT NULL DEFAULT '[]',
                attachment_url TEXT,
                attachment_name TEXT,
                updated_ts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (thread_id, msg_id)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_thread_ts
                ON messages(thread_id, ts DESC);

            CREATE TABLE IF NOT EXISTS message_translations (
                msg_id TEXT NOT NULL,
                target_lang TEXT NOT NULL,
                text_hash TEXT NOT NULL,
                provider TEXT NOT NULL DEFAULT '',
                translated_text TEXT NOT NULL,
                detected_source_lang TEXT,
                skipped INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (msg_id, target_lang)
            );
            CREATE INDEX IF NOT EXISTS idx_msg_translations_msg
                ON message_translations(msg_id);

            CREATE TABLE IF NOT EXISTS media_objects (
                sha256 TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                mime TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                thumb_sha256 TEXT,
                width INTEGER,
                height INTEGER,
                duration_ms INTEGER,
                local_state INTEGER NOT NULL DEFAULT 0,
                local_rel_path TEXT,
                is_encrypted INTEGER NOT NULL DEFAULT 1,
                pin_count INTEGER NOT NULL DEFAULT 0,
                created_ts INTEGER NOT NULL,
                last_accessed_ts INTEGER NOT NULL,
                cloud_provider TEXT,
                cloud_file_id TEXT,
                cloud_encrypted INTEGER NOT NULL DEFAULT 1,
                cloud_key_ver INTEGER NOT NULL DEFAULT 1,
                cloud_synced_ts INTEGER,
                cloud_excluded INTEGER NOT NULL DEFAULT 0,
                cloud_upload_fail_count INTEGER NOT NULL DEFAULT 0,
                cloud_upload_next_retry_ts INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_media_objects_lru
                ON media_objects(local_state, pin_count, last_accessed_ts ASC);
            CREATE INDEX IF NOT EXISTS idx_media_objects_cloud_pending
                ON media_objects(local_state, cloud_file_id);

            CREATE TABLE IF NOT EXISTS message_media (
                thread_id TEXT NOT NULL,
                msg_id TEXT NOT NULL,
                variant TEXT NOT NULL DEFAULT 'primary',
                url_hash TEXT NOT NULL,
                sha256 TEXT,
                original_name TEXT,
                saved_user_path TEXT,
                cdn_status INTEGER NOT NULL DEFAULT 0,
                updated_ts INTEGER NOT NULL,
                PRIMARY KEY (thread_id, msg_id, variant)
            );
            CREATE INDEX IF NOT EXISTS idx_message_media_url_hash
                ON message_media(url_hash);
            CREATE INDEX IF NOT EXISTS idx_message_media_sha256
                ON message_media(sha256);

            CREATE TABLE IF NOT EXISTS gdrive_applied_deltas (
                delta_file_id TEXT PRIMARY KEY,
                applied_ts INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS gdrive_deleted_messages (
                thread_id TEXT NOT NULL,
                msg_id TEXT NOT NULL,
                deleted_ts INTEGER NOT NULL,
                local_mutation_ts INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (thread_id, msg_id)
            );
            CREATE INDEX IF NOT EXISTS idx_gdrive_deleted_messages_mut
                ON gdrive_deleted_messages(local_mutation_ts);

            CREATE TABLE IF NOT EXISTS gdrive_deleted_threads (
                thread_id TEXT PRIMARY KEY,
                deleted_ts INTEGER NOT NULL,
                local_mutation_ts INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_gdrive_deleted_threads_mut
                ON gdrive_deleted_threads(local_mutation_ts);

            CREATE TABLE IF NOT EXISTS gdrive_pending_cloud_deletes (
                sha256 TEXT PRIMARY KEY,
                cloud_file_id TEXT NOT NULL,
                reason TEXT NOT NULL DEFAULT '',
                queued_ts INTEGER NOT NULL,
                retry_count INTEGER NOT NULL DEFAULT 0,
                next_retry_ts INTEGER NOT NULL DEFAULT 0,
                queued_pull_seq INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_gdrive_pending_cloud_deletes_ts
                ON gdrive_pending_cloud_deletes(queued_ts ASC);

            CREATE TABLE IF NOT EXISTS ai_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                backend TEXT NOT NULL DEFAULT '',
                ts INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ai_messages_session_ts
                ON ai_messages(session_id, ts);

            CREATE TABLE IF NOT EXISTS ai_sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_ts INTEGER NOT NULL,
                updated_ts INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ai_sessions_updated_ts
                ON ai_sessions(updated_ts DESC);

            CREATE TABLE IF NOT EXISTS scheduled_messages (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                thread_title TEXT NOT NULL,
                thread_kind TEXT NOT NULL DEFAULT 'direct',
                text TEXT NOT NULL,
                attachment_path TEXT NOT NULL DEFAULT '',
                send_at_ts INTEGER NOT NULL,
                next_attempt_ts INTEGER,
                bypass_misfire INTEGER NOT NULL DEFAULT 0,
                anchor_day INTEGER,
                recurrence TEXT NOT NULL DEFAULT 'once',
                misfire_policy TEXT NOT NULL DEFAULT 'send_on_resume',
                misfire_grace_secs INTEGER NOT NULL DEFAULT 600,
                status TEXT NOT NULL DEFAULT 'pending',
                retry_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                sent_at_ts INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_scheduled_messages_status_send_at
                ON scheduled_messages(status, send_at_ts);

            CREATE TABLE IF NOT EXISTS api_clients (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                token_prefix TEXT NOT NULL,
                scopes TEXT NOT NULL,
                allowed_thread_ids_json TEXT NOT NULL DEFAULT '[]',
                rate_limit_per_min INTEGER NOT NULL DEFAULT 20,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                last_used_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS api_audit_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL,
                client_name TEXT NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                target_thread_id TEXT NOT NULL DEFAULT '',
                status_code INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_api_audit_logs_created_at
                ON api_audit_logs(created_at DESC);

            CREATE TABLE IF NOT EXISTS api_idempotency (
                client_id TEXT NOT NULL,
                idem_key TEXT NOT NULL,
                status_code INTEGER NOT NULL,
                response_json TEXT NOT NULL,
                request_hash TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (client_id, idem_key)
            );
            CREATE INDEX IF NOT EXISTS idx_api_idempotency_created_at
                ON api_idempotency(created_at);
            "#,
        )?;
        self.ensure_column("messages", "quote_json", "TEXT")?;
        self.ensure_column("messages", "reactions_json", "TEXT NOT NULL DEFAULT '[]'")?;
        self.ensure_column("messages", "cli_msg_id", "TEXT NOT NULL DEFAULT ''")?;
        self.ensure_column("messages", "reaction_actors_json", "TEXT NOT NULL DEFAULT ''")?;
        self.ensure_column("messages", "updated_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_thread_cli_msg_id ON messages(thread_id, cli_msg_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_updated_ts ON messages(updated_ts)",
            [],
        )?;
        let backfilled_recalled_updated_ts: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'backfilled_recalled_updated_ts_v1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if backfilled_recalled_updated_ts.is_none() {
            let tx = self.conn.unchecked_transaction()?;
            let has_unbackfilled_recalled: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE is_recalled = 1 AND updated_ts = 0)",
                [],
                |r| r.get(0),
            )?;
            if has_unbackfilled_recalled {
                let mut_ts = Self::next_local_mutation_ts_in_tx(&tx)?;
                tx.execute(
                    "UPDATE messages SET updated_ts = ?1 WHERE is_recalled = 1 AND updated_ts = 0",
                    params![mut_ts],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('backfilled_recalled_updated_ts_v1', '1')",
                [],
            )?;
            tx.commit()?;
        }
        self.ensure_column("threads", "last_read_in_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "peer_read_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "muted_until_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("media_objects", "cloud_excluded", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column(
            "media_objects",
            "cloud_upload_fail_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "media_objects",
            "cloud_upload_next_retry_ts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "gdrive_pending_cloud_deletes",
            "retry_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "gdrive_pending_cloud_deletes",
            "next_retry_ts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "gdrive_pending_cloud_deletes",
            "queued_pull_seq",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_gdrive_pending_cloud_deletes_retry
             ON gdrive_pending_cloud_deletes(next_retry_ts ASC, queued_ts ASC)",
            [],
        )?;
        self.ensure_column("scheduled_messages", "next_attempt_ts", "INTEGER")?;
        self.ensure_column(
            "scheduled_messages",
            "bypass_misfire",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column("scheduled_messages", "anchor_day", "INTEGER")?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scheduled_messages_status_next_attempt
             ON scheduled_messages(status, next_attempt_ts)",
            [],
        )?;
        self.ensure_column(
            "api_idempotency",
            "request_hash",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        let missing_anchors: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, send_at_ts FROM scheduled_messages WHERE anchor_day IS NULL",
            )?;
            let out = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        };
        if !missing_anchors.is_empty() {
            let tx = self.conn.unchecked_transaction()?;
            for (id, send_at_ts) in missing_anchors {
                if let Some(day) = local_day_of_month(send_at_ts) {
                    tx.execute(
                        "UPDATE scheduled_messages SET anchor_day = ?2 WHERE id = ?1 AND anchor_day IS NULL",
                        params![id, i64::from(day)],
                    )?;
                }
            }
            tx.commit()?;
        }

        let existing_check: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'key_check' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(ref stored) = existing_check {
            let val = self.cipher.decrypt_str(b"meta:v1", stored)?;
            if val != KEY_CHECK_VALUE {
                bail!("Khóa mã hóa database không khớp");
            }
        } else {
            // If DB already has encrypted rows without store_meta (defensive check), verify key works.
            if db_existed {
                let sample: Option<(String, String, String)> = self
                    .conn
                    .query_row(
                        "SELECT thread_id, msg_id, text FROM messages WHERE text LIKE 'enc:v1:%' LIMIT 1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                if let Some((tid, mid, ct)) = sample {
                    self.cipher.decrypt_str(&msg_aad(&tid, &mid), &ct)?;
                }
            }
            let sealed_check = self.cipher.encrypt_str(b"meta:v1", KEY_CHECK_VALUE)?;
            self.conn.execute(
                "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('key_check', ?1)",
                params![sealed_check],
            )?;
        }

        let had_disk_fts: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages_fts')",
            [],
            |r| r.get(0),
        )?;
        if had_disk_fts {
            self.conn.execute_batch("DROP TABLE IF EXISTS messages_fts;")?;
        }

        let migrated_rows = self.migrate_legacy_plaintext_rows()?;
        self.conn.execute_batch(
            "UPDATE threads SET last_ts = last_ts * 1000 WHERE last_ts >= 1000000000 AND last_ts < 10000000000;
             UPDATE messages SET ts = ts * 1000 WHERE ts >= 1000000000 AND ts < 10000000000;",
        )?;
        let fixed_unread: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'fixed_outgoing_unread_v1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if fixed_unread.is_none() {
            self.conn.execute_batch(
                "UPDATE threads
                 SET unread_count = 0
                 WHERE unread_count > 0
                   AND COALESCE(
                     (SELECT m.is_outgoing FROM messages m WHERE m.thread_id = threads.id ORDER BY m.ts DESC LIMIT 1),
                     0
                   ) = 1;
                 INSERT OR REPLACE INTO store_meta (k, v) VALUES ('fixed_outgoing_unread_v1', '1');",
            )?;
        }
        let backfilled_watermarks: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'backfill_read_watermarks_v1' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if backfilled_watermarks.is_none() {
            self.conn.execute_batch(
                "UPDATE threads
                 SET peer_read_ts = MAX(
                     peer_read_ts,
                     COALESCE((SELECT MAX(m.ts) FROM messages m WHERE m.thread_id = threads.id AND m.is_outgoing = 0), 0)
                 )
                 WHERE peer_read_ts = 0;
                 UPDATE threads
                 SET last_read_in_ts = MAX(
                     last_read_in_ts,
                     COALESCE((SELECT MAX(m.ts) FROM messages m WHERE m.thread_id = threads.id AND m.is_outgoing = 0), last_ts)
                 )
                 WHERE unread_count = 0 AND last_read_in_ts = 0;
                 INSERT OR REPLACE INTO store_meta (k, v) VALUES ('backfill_read_watermarks_v1', '1');",
            )?;
        }
        if had_disk_fts || migrated_rows {
            self.conn.execute_batch("VACUUM;")?;
        }
        Ok(())
    }

    fn migrate_legacy_plaintext_rows(&self) -> Result<bool> {
        let mut touched = false;

        let mut thread_stmt = self.conn.prepare(
            "SELECT id, title, avatar, last_preview, title_norm FROM threads
             WHERE title NOT LIKE 'enc:v1:%'
                OR avatar NOT LIKE 'enc:v1:%'
                OR last_preview NOT LIKE 'enc:v1:%'
                OR title_norm != ''",
        )?;
        let thread_rows = thread_stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(thread_stmt);

        if !thread_rows.is_empty() {
            touched = true;
            let tx = self.conn.unchecked_transaction()?;
            for (id, title, avatar, last_preview) in thread_rows {
                let aad = thread_aad(&id);
                let enc_title = self.cipher.encrypt_if_plain(&aad, &title)?;
                let enc_avatar = self.cipher.encrypt_if_plain(&aad, &avatar)?;
                let enc_preview = self.cipher.encrypt_if_plain(&aad, &last_preview)?;
                tx.execute(
                    "UPDATE threads SET title = ?2, avatar = ?3, last_preview = ?4, title_norm = '' WHERE id = ?1",
                    params![id, enc_title, enc_avatar, enc_preview],
                )?;
            }
            tx.commit()?;
        }

        let mut msg_stmt = self.conn.prepare(
            "SELECT thread_id, msg_id, sender_id, sender_name, text, quote_json, reactions_json, attachment_url, attachment_name, text_norm
             FROM messages
             WHERE text NOT LIKE 'enc:v1:%'
                OR sender_name NOT LIKE 'enc:v1:%'
                OR sender_id NOT LIKE 'enc:v1:%'
                OR reactions_json NOT LIKE 'enc:v1:%'
                OR (quote_json IS NOT NULL AND quote_json NOT LIKE 'enc:v1:%')
                OR (attachment_url IS NOT NULL AND attachment_url NOT LIKE 'enc:v1:%')
                OR (attachment_name IS NOT NULL AND attachment_name NOT LIKE 'enc:v1:%')
                OR text_norm != ''",
        )?;
        let msg_rows = msg_stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(msg_stmt);

        if !msg_rows.is_empty() {
            touched = true;
            let tx = self.conn.unchecked_transaction()?;
            for (tid, mid, sid, sname, text, qj, rj, aurl, aname) in msg_rows {
                let aad = msg_aad(&tid, &mid);
                let enc_sid = self.cipher.encrypt_if_plain(&aad, &sid)?;
                let enc_sname = self.cipher.encrypt_if_plain(&aad, &sname)?;
                let enc_text = self.cipher.encrypt_if_plain(&aad, &text)?;
                let enc_qj = qj
                    .map(|s| self.cipher.encrypt_if_plain(&aad, &s))
                    .transpose()?;
                let enc_rj = self.cipher.encrypt_if_plain(&aad, &rj)?;
                let enc_aurl = aurl
                    .map(|s| self.cipher.encrypt_if_plain(&aad, &s))
                    .transpose()?;
                let enc_aname = aname
                    .map(|s| self.cipher.encrypt_if_plain(&aad, &s))
                    .transpose()?;
                tx.execute(
                    "UPDATE messages
                     SET sender_id = ?3, sender_name = ?4, text = ?5, text_norm = '',
                         quote_json = ?6, reactions_json = ?7, attachment_url = ?8, attachment_name = ?9
                     WHERE thread_id = ?1 AND msg_id = ?2",
                    params![tid, mid, enc_sid, enc_sname, enc_text, enc_qj, enc_rj, enc_aurl, enc_aname],
                )?;
            }
            tx.commit()?;
        }

        Ok(touched)
    }

    fn rebuild_in_memory_fts(&self) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT thread_id, msg_id, ts, sender_name, text FROM messages")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;

        let tx = self.fts_conn.unchecked_transaction()?;
        tx.execute("DELETE FROM messages_fts", [])?;
        for row in rows {
            let (tid, mid, ts, enc_sender, enc_text) = row?;
            let aad = msg_aad(&tid, &mid);
            let sender_name = self.cipher.decrypt_or_legacy(&aad, &enc_sender)?;
            let text = self.cipher.decrypt_or_legacy(&aad, &enc_text)?;
            tx.execute(
                "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    mid,
                    tid,
                    ts,
                    strip_vi_accents(&sender_name),
                    strip_vi_accents(&text)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn has_column(&self, table: &str, col: &str) -> Result<bool> {
        let sql = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&sql)?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(std::result::Result::ok)
            .any(|name| name == col);
        Ok(exists)
    }

    fn ensure_column(&self, table: &str, col: &str, def: &str) -> Result<()> {
        if !self.has_column(table, col)? {
            self.conn
                .execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {def}"), [])?;
        }
        Ok(())
    }

    pub fn upsert_thread(&self, t: &ThreadDto) -> Result<()> {
        self.upsert_thread_inner(t, false, false)
    }

    pub fn upsert_thread_synced(&self, t: &ThreadDto) -> Result<()> {
        self.upsert_thread_inner(t, true, false)
    }

    pub fn upsert_thread_synced_with_read_state(
        &self,
        t: &ThreadDto,
        overwrite_pinned: bool,
        remote_read_authoritative: bool,
    ) -> Result<()> {
        self.upsert_thread_inner(t, overwrite_pinned, remote_read_authoritative)
    }

    fn upsert_thread_inner(
        &self,
        t: &ThreadDto,
        overwrite_pinned: bool,
        remote_read_authoritative: bool,
    ) -> Result<()> {
        let kind_str = match t.kind {
            ThreadKind::Direct => "direct",
            ThreadKind::Group => "group",
            ThreadKind::Cloud => "cloud",
        };
        let aad = thread_aad(&t.id);
        let enc_title = self.cipher.encrypt_str(&aad, &t.title)?;
        let enc_avatar = self.cipher.encrypt_str(&aad, &t.avatar)?;
        let enc_preview = self.cipher.encrypt_str(&aad, &t.last_preview)?;
        let preview_empty = t.last_preview.is_empty() as i64;
        let avatar_empty = t.avatar.is_empty() as i64;
        let title_fallback = (t.title == t.id) as i64;
        let clear_unread = (t.unread_count == 0
            && (t.last_preview.starts_with("Bạn:") || remote_read_authoritative))
            as i64;
        let last_ts = normalize_ts_ms(t.last_ts);
        let explicit_read_in_ts = normalize_ts_ms(t.last_read_in_ts);
        let read_in_ts = if clear_unread == 1 {
            last_ts.max(explicit_read_in_ts)
        } else {
            explicit_read_in_ts
        };
        self.conn.execute(
            r#"
            INSERT INTO threads (id, title, title_norm, avatar, kind, last_preview, last_ts, unread_count, is_pinned, is_muted, last_read_in_ts, peer_read_ts, muted_until_ts, is_archived)
            VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?15, ?16, ?17, ?18)
            ON CONFLICT(id) DO UPDATE SET
                title = CASE WHEN ?12 = 1 THEN threads.title ELSE excluded.title END,
                title_norm = '',
                avatar = CASE WHEN ?11 = 1 THEN threads.avatar ELSE excluded.avatar END,
                kind = excluded.kind,
                last_preview = CASE WHEN ?10 = 1 THEN threads.last_preview ELSE excluded.last_preview END,
                last_ts = MAX(excluded.last_ts, threads.last_ts),
                unread_count = CASE
                    WHEN ?13 = 1 AND excluded.last_ts >= threads.last_ts THEN 0
                    ELSE MAX(excluded.unread_count, threads.unread_count)
                END,
                is_pinned = CASE
                    WHEN ?14 = 1 THEN excluded.is_pinned
                    ELSE MAX(excluded.is_pinned, threads.is_pinned)
                END,
                is_muted = MAX(excluded.is_muted, threads.is_muted),
                last_read_in_ts = CASE
                    WHEN ?13 = 1 AND excluded.last_ts >= threads.last_ts
                    THEN MAX(threads.last_read_in_ts, excluded.last_read_in_ts, excluded.last_ts)
                    ELSE MAX(threads.last_read_in_ts, ?19)
                END,
                peer_read_ts = MAX(threads.peer_read_ts, excluded.peer_read_ts),
                muted_until_ts = MAX(threads.muted_until_ts, excluded.muted_until_ts),
                is_archived = MAX(threads.is_archived, excluded.is_archived)
            "#,
            params![
                t.id,
                enc_title,
                enc_avatar,
                kind_str,
                enc_preview,
                last_ts,
                t.unread_count,
                t.is_pinned as i64,
                t.is_muted as i64,
                preview_empty,
                avatar_empty,
                title_fallback,
                clear_unread,
                overwrite_pinned as i64,
                read_in_ts,
                normalize_ts_ms(t.peer_read_ts),
                normalize_ts_ms(t.muted_until_ts),
                t.is_archived as i64,
                explicit_read_in_ts,
            ],
        )?;
        Ok(())
    }

    pub fn list_threads(&self, folder: FolderFilter) -> Result<Vec<ThreadDto>> {
        let where_clause = match folder {
            FolderFilter::All => "",
            FolderFilter::Unread => "WHERE unread_count > 0",
            FolderFilter::Direct => "WHERE kind = 'direct'",
            FolderFilter::Group => "WHERE kind = 'group'",
            FolderFilter::Cloud => "WHERE kind = 'cloud'",
        };
        let sql = format!(
            "SELECT id, title, avatar, kind, last_preview, last_ts, unread_count, is_pinned, is_muted, last_read_in_ts, peer_read_ts, muted_until_ts, is_archived
             FROM threads {where_clause} ORDER BY is_pinned DESC, last_ts DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })?;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut out = Vec::new();
        for r in rows {
            let (
                id,
                enc_title,
                enc_avatar,
                kind_str,
                enc_preview,
                last_ts,
                unread,
                pinned,
                muted,
                last_read_in_ts,
                peer_read_ts,
                raw_muted_until_ts,
                archived,
            ) = r?;
            let aad = thread_aad(&id);
            let title = self.cipher.decrypt_str(&aad, &enc_title)?;
            let avatar = self.cipher.decrypt_str(&aad, &enc_avatar)?;
            let last_preview = self.cipher.decrypt_str(&aad, &enc_preview)?;
            let kind = match kind_str.as_str() {
                "group" => ThreadKind::Group,
                "cloud" => ThreadKind::Cloud,
                _ => ThreadKind::Direct,
            };
            let mute_expired =
                muted != 0 && raw_muted_until_ts > 0 && raw_muted_until_ts <= now_ms;
            let is_muted = muted != 0 && !mute_expired;
            let muted_until_ts = if is_muted { raw_muted_until_ts } else { 0 };
            out.push(ThreadDto {
                id,
                title,
                avatar,
                kind,
                last_preview,
                last_ts,
                unread_count: unread.max(0) as u32,
                is_pinned: pinned != 0,
                is_muted,
                last_read_in_ts,
                peer_read_ts,
                muted_until_ts,
                is_archived: archived != 0,
            });
        }
        Ok(out)
    }

    fn next_local_mutation_ts_on_conn(conn: &Connection) -> Result<i64> {
        let now_ms = chrono::Utc::now().timestamp_millis().max(1);
        let last_sync_mut: i64 = conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_last_mutation_sync_ts' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let last_issued: i64 = conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'local_mutation_seq_ts' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let next = now_ms
            .max(last_sync_mut)
            .max(last_issued.saturating_add(1))
            .max(1);
        conn.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('local_mutation_seq_ts', ?1)",
            params![next.to_string()],
        )?;
        Ok(next)
    }

    fn advance_local_mutation_seq_on_conn(conn: &Connection, ts: i64) -> Result<()> {
        if ts <= 0 {
            return Ok(());
        }
        let last_issued: i64 = conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'local_mutation_seq_ts' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if ts > last_issued {
            conn.execute(
                "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('local_mutation_seq_ts', ?1)",
                params![ts.to_string()],
            )?;
        }
        Ok(())
    }

    fn next_local_mutation_ts_in_tx(tx: &rusqlite::Transaction<'_>) -> Result<i64> {
        Self::next_local_mutation_ts_on_conn(tx)
    }

    pub(crate) fn next_local_mutation_ts(&self) -> Result<i64> {
        Self::next_local_mutation_ts_on_conn(&self.conn)
    }

    fn get_gdrive_history_pull_seq_on_conn(conn: &Connection) -> Result<i64> {
        let seq = conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_history_pull_seq' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        Ok(seq.max(0))
    }

    fn cleanup_orphaned_media_for_shas_in_tx(
        tx: &rusqlite::Transaction<'_>,
        shas: Vec<String>,
        queue_cloud_delete_reason: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut orphaned_paths: Vec<String> = Vec::new();
        let mut thumb_candidates: Vec<String> = Vec::new();
        let now_ms = chrono::Utc::now().timestamp_millis().max(1);
        let current_pull_seq = if queue_cloud_delete_reason.is_some() {
            Self::get_gdrive_history_pull_seq_on_conn(tx)?
        } else {
            0
        };

        for sha in shas {
            let still_referenced: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_media WHERE sha256 = ?1)
                     OR EXISTS(SELECT 1 FROM media_objects WHERE thumb_sha256 = ?1)",
                params![&sha],
                |r| r.get(0),
            )?;
            if still_referenced {
                continue;
            }
            struct OrphanMediaRow {
                local_state: i32,
                local_rel_path: Option<String>,
                thumb_sha256: Option<String>,
                cloud_file_id: Option<String>,
            }
            let row: Option<OrphanMediaRow> = tx
                .query_row(
                    "SELECT local_state, local_rel_path, thumb_sha256, cloud_file_id
                     FROM media_objects
                     WHERE sha256 = ?1
                     LIMIT 1",
                    params![&sha],
                    |r| {
                        Ok(OrphanMediaRow {
                            local_state: r.get(0)?,
                            local_rel_path: r.get(1)?,
                            thumb_sha256: r.get(2)?,
                            cloud_file_id: r.get(3)?,
                        })
                    },
                )
                .optional()?;
            if let Some(OrphanMediaRow {
                local_state,
                local_rel_path,
                thumb_sha256,
                cloud_file_id,
            }) = row
            {
                if local_state == MEDIA_LOCAL_STATE_LOCAL {
                    if let Some(rel) = local_rel_path {
                        if !orphaned_paths.contains(&rel) {
                            orphaned_paths.push(rel);
                        }
                    }
                }
                if let Some(cid) = cloud_file_id.as_deref().filter(|s| !s.is_empty()) {
                    if let Some(reason) = queue_cloud_delete_reason {
                        tx.execute(
                            "UPDATE media_objects SET local_state = 0, local_rel_path = NULL, thumb_sha256 = NULL WHERE sha256 = ?1",
                            params![&sha],
                        )?;
                        tx.execute(
                            "INSERT INTO gdrive_pending_cloud_deletes (
                                 sha256, cloud_file_id, reason, queued_ts, retry_count, next_retry_ts, queued_pull_seq
                             )
                             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)
                             ON CONFLICT(sha256) DO UPDATE SET
                                 cloud_file_id = excluded.cloud_file_id,
                                 reason = excluded.reason,
                                 queued_ts = excluded.queued_ts,
                                 retry_count = 0,
                                 next_retry_ts = 0,
                                 queued_pull_seq = excluded.queued_pull_seq",
                            params![&sha, cid, reason, now_ms, current_pull_seq],
                        )?;
                    } else {
                        tx.execute("DELETE FROM media_objects WHERE sha256 = ?1", params![&sha])?;
                        tx.execute(
                            "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
                            params![&sha],
                        )?;
                    }
                } else {
                    tx.execute("DELETE FROM media_objects WHERE sha256 = ?1", params![&sha])?;
                }
                if let Some(tsha) = thumb_sha256.filter(|s| !s.is_empty()) {
                    thumb_candidates.push(tsha);
                }
            }
        }

        for tsha in thumb_candidates {
            let still_referenced: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_media WHERE sha256 = ?1)
                     OR EXISTS(SELECT 1 FROM media_objects WHERE thumb_sha256 = ?1)",
                params![&tsha],
                |r| r.get(0),
            )?;
            if still_referenced {
                continue;
            }
            let row: Option<(i32, Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT local_state, local_rel_path, cloud_file_id
                     FROM media_objects
                     WHERE sha256 = ?1
                     LIMIT 1",
                    params![&tsha],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((local_state, local_rel_path, cloud_file_id)) = row {
                if local_state == MEDIA_LOCAL_STATE_LOCAL {
                    if let Some(rel) = local_rel_path {
                        if !orphaned_paths.contains(&rel) {
                            orphaned_paths.push(rel);
                        }
                    }
                }
                if let Some(cid) = cloud_file_id.as_deref().filter(|s| !s.is_empty()) {
                    if let Some(reason) = queue_cloud_delete_reason {
                        tx.execute(
                            "UPDATE media_objects SET local_state = 0, local_rel_path = NULL WHERE sha256 = ?1",
                            params![&tsha],
                        )?;
                        tx.execute(
                            "INSERT INTO gdrive_pending_cloud_deletes (
                                 sha256, cloud_file_id, reason, queued_ts, retry_count, next_retry_ts, queued_pull_seq
                             )
                             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)
                             ON CONFLICT(sha256) DO UPDATE SET
                                 cloud_file_id = excluded.cloud_file_id,
                                 reason = excluded.reason,
                                 queued_ts = excluded.queued_ts,
                                 retry_count = 0,
                                 next_retry_ts = 0,
                                 queued_pull_seq = excluded.queued_pull_seq",
                            params![&tsha, cid, reason, now_ms, current_pull_seq],
                        )?;
                    } else {
                        tx.execute("DELETE FROM media_objects WHERE sha256 = ?1", params![&tsha])?;
                        tx.execute(
                            "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
                            params![&tsha],
                        )?;
                    }
                } else {
                    tx.execute("DELETE FROM media_objects WHERE sha256 = ?1", params![&tsha])?;
                }
            }
        }

        Ok(orphaned_paths)
    }

    fn cleanup_orphaned_media_for_msg_in_tx(
        tx: &rusqlite::Transaction<'_>,
        thread_id: &str,
        msg_id: &str,
        queue_cloud_delete_reason: Option<&str>,
    ) -> Result<Vec<String>> {
        let shas: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT sha256
                 FROM message_media
                 WHERE thread_id = ?1 AND msg_id = ?2 AND sha256 IS NOT NULL AND sha256 != ''",
            )?;
            let out = stmt
                .query_map(params![thread_id, msg_id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        };

        tx.execute(
            "DELETE FROM message_media WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;

        Self::cleanup_orphaned_media_for_shas_in_tx(tx, shas, queue_cloud_delete_reason)
    }

    fn upsert_message_row_in_tx(
        cipher: &DbCipher,
        tx: &rusqlite::Transaction<'_>,
        m: &MessageDto,
        force_updated_ts: bool,
    ) -> Result<(i64, String, String, Vec<String>)> {
        let sender_norm = strip_vi_accents(&m.sender_name);
        let text_norm = strip_vi_accents(&m.text);
        let quote_json = m
            .quote
            .as_ref()
            .and_then(|q| serde_json::to_string(q).ok());
        let reactions_json =
            serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".to_string());
        let reactions_empty = m.reactions.is_empty() as i64;

        let aad = msg_aad(&m.thread_id, &m.msg_id);
        let ts = normalize_ts_ms(m.ts);
        struct ExistingUpsertMsg {
            is_recalled: i64,
            is_pinned: i64,
            enc_text: String,
            enc_att_name: Option<String>,
        }
        let existing_row: Option<ExistingUpsertMsg> = tx
            .query_row(
                "SELECT is_recalled, is_pinned, text, attachment_name
                 FROM messages
                 WHERE thread_id = ?1 AND msg_id = ?2
                 LIMIT 1",
                params![&m.thread_id, &m.msg_id],
                |r| {
                    Ok(ExistingUpsertMsg {
                        is_recalled: r.get(0)?,
                        is_pinned: r.get(1)?,
                        enc_text: r.get(2)?,
                        enc_att_name: r.get(3)?,
                    })
                },
            )
            .optional()?;

        let mut effective_force_updated = force_updated_ts;
        if !effective_force_updated
            && m.msg_type.eq_ignore_ascii_case("chat.poll")
            && m.attachment_name.is_some()
        {
            if let Some(existing_enc_att) = existing_row
                .as_ref()
                .and_then(|row| row.enc_att_name.as_deref())
            {
                let existing_plain = cipher.decrypt_or_legacy(&aad, existing_enc_att).ok();
                if existing_plain.as_deref() != m.attachment_name.as_deref() {
                    effective_force_updated = true;
                }
            }
        }

        let last_history_sync_ts: i64 = tx
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_last_history_sync_ts' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);

        let preserve_existing_recall = existing_row
            .as_ref()
            .is_some_and(|row| row.is_recalled != 0 && !m.is_recalled);
        let fts_text_norm = if preserve_existing_recall {
            existing_row
                .as_ref()
                .and_then(|row| cipher.decrypt_or_legacy(&aad, &row.enc_text).ok())
                .map(|plain| strip_vi_accents(&plain))
                .unwrap_or(text_norm)
        } else {
            text_norm
        };

        let insert_needs_mut_ts = effective_force_updated
            || m.is_recalled
            || (last_history_sync_ts > 0 && ts < last_history_sync_ts);
        let update_needs_mut_ts = match &existing_row {
            None => false,
            Some(_) if preserve_existing_recall => false,
            Some(cur) => {
                effective_force_updated
                    || (m.is_recalled && cur.is_recalled == 0)
                    || (m.is_pinned && cur.is_pinned == 0)
            }
        };
        let mut_ts = if (existing_row.is_none() && insert_needs_mut_ts) || update_needs_mut_ts {
            Self::next_local_mutation_ts_in_tx(tx)?
        } else {
            0
        };
        let insert_updated_ts = if insert_needs_mut_ts { mut_ts } else { 0 };

        let enc_sender_id = cipher.encrypt_str(&aad, &m.sender_id)?;
        let enc_sender_name = cipher.encrypt_str(&aad, &m.sender_name)?;
        let enc_text = cipher.encrypt_str(&aad, &m.text)?;
        let enc_quote = cipher.encrypt_opt(&aad, quote_json.as_deref())?;
        let enc_reactions = cipher.encrypt_str(&aad, &reactions_json)?;
        let enc_att_url = if m.is_recalled {
            None
        } else {
            cipher.encrypt_opt(&aad, m.attachment_url.as_deref())?
        };
        let enc_att_name = if m.is_recalled {
            None
        } else {
            cipher.encrypt_opt(&aad, m.attachment_name.as_deref())?
        };
        let cli_msg_id = m
            .cli_msg_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .unwrap_or("");

        tx.execute(
            r#"
            INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id, updated_ts)
            VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?19)
            ON CONFLICT(thread_id, msg_id) DO UPDATE SET
                text = CASE
                    WHEN messages.is_recalled = 1 AND excluded.is_recalled = 0 THEN messages.text
                    ELSE excluded.text
                END,
                text_norm = '',
                msg_type = CASE
                    WHEN messages.is_recalled = 1 AND excluded.is_recalled = 0 THEN messages.msg_type
                    ELSE excluded.msg_type
                END,
                is_recalled = MAX(messages.is_recalled, excluded.is_recalled),
                is_pinned = MAX(messages.is_pinned, excluded.is_pinned),
                quote_json = CASE
                    WHEN messages.is_recalled = 1 AND excluded.is_recalled = 0 THEN messages.quote_json
                    ELSE COALESCE(excluded.quote_json, messages.quote_json)
                END,
                reactions_json = CASE
                    WHEN (messages.is_recalled = 1 AND excluded.is_recalled = 0) OR ?16 = 1 THEN messages.reactions_json
                    ELSE excluded.reactions_json
                END,
                cli_msg_id = CASE WHEN excluded.cli_msg_id != '' THEN excluded.cli_msg_id ELSE messages.cli_msg_id END,
                attachment_url = CASE
                    WHEN messages.is_recalled = 1 OR excluded.is_recalled = 1 OR LOWER(excluded.msg_type) IN ('chat.reminder', 'chat.poll') THEN NULL
                    WHEN LOWER(excluded.msg_type) IN ('chat.link', 'link', 'chat.location', 'chat.location.new', 'chat.recommended', 'chat.ecard') THEN excluded.attachment_url
                    ELSE COALESCE(excluded.attachment_url, messages.attachment_url)
                END,
                attachment_name = CASE
                    WHEN messages.is_recalled = 1 OR excluded.is_recalled = 1 THEN NULL
                    WHEN LOWER(excluded.msg_type) IN ('chat.link', 'link', 'chat.location', 'chat.location.new', 'chat.recommended', 'chat.ecard') AND excluded.attachment_url IS NULL THEN NULL
                    ELSE COALESCE(excluded.attachment_name, messages.attachment_name)
                END,
                updated_ts = CASE
                    WHEN messages.is_recalled = 1 AND excluded.is_recalled = 0 THEN messages.updated_ts
                    WHEN ?18 = 1
                      OR excluded.is_recalled > messages.is_recalled
                      OR excluded.is_pinned > messages.is_pinned
                    THEN ?17
                    ELSE messages.updated_ts
                END
            "#,
            params![
                m.msg_id,
                m.thread_id,
                enc_sender_id,
                enc_sender_name,
                enc_text,
                m.msg_type,
                ts,
                m.is_outgoing as i64,
                m.is_recalled as i64,
                m.is_pinned as i64,
                enc_quote,
                enc_reactions,
                enc_att_url,
                enc_att_name,
                cli_msg_id,
                reactions_empty,
                mut_ts,
                effective_force_updated as i64,
                insert_updated_ts,
            ],
        )?;
        if !m.is_outgoing {
            tx.execute(
                "UPDATE threads SET peer_read_ts = MAX(peer_read_ts, ?2) WHERE id = ?1",
                params![m.thread_id, ts],
            )?;
        }
        let mut orphaned_paths = Vec::new();
        if m.is_recalled {
            tx.execute(
                "DELETE FROM message_translations WHERE msg_id = ?1",
                params![m.msg_id],
            )?;
            orphaned_paths = Self::cleanup_orphaned_media_for_msg_in_tx(
                tx,
                &m.thread_id,
                &m.msg_id,
                Some("msg_recalled"),
            )?;
        }
        Ok((ts, sender_norm, fts_text_norm, orphaned_paths))
    }

    fn upsert_message_inner(&mut self, m: &MessageDto, force_updated_ts: bool) -> Result<Vec<String>> {
        let tx = self.conn.transaction()?;
        let (ts, sender_norm, text_norm, orphaned_paths) =
            Self::upsert_message_row_in_tx(&self.cipher, &tx, m, force_updated_ts)?;
        tx.commit()?;

        let fts_tx = self.fts_conn.transaction()?;
        fts_tx.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
            params![m.thread_id, m.msg_id],
        )?;
        fts_tx.execute(
            "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.msg_id, m.thread_id, ts, sender_norm, text_norm],
        )?;
        fts_tx.commit()?;
        Ok(orphaned_paths)
    }

    pub fn upsert_message(&mut self, m: &MessageDto) -> Result<()> {
        let _ = self.upsert_message_inner(m, false)?;
        Ok(())
    }

    pub fn upsert_message_returning_orphans(&mut self, m: &MessageDto) -> Result<Vec<String>> {
        self.upsert_message_inner(m, false)
    }

    fn query_poll_candidates_in_tx(
        &self,
        tx: &Transaction<'_>,
        tid_candidates: &[String],
    ) -> Result<Vec<MessageDto>> {
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut out: Vec<MessageDto> = Vec::new();
        let mut stmt = tx.prepare(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing,
                    is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE thread_id = ?1
               AND (
                   LOWER(msg_type) LIKE '%poll%'
                   OR msg_id LIKE 'poll-%'
                   OR msg_id LIKE 'local-poll-%'
                   OR (attachment_name IS NOT NULL AND attachment_url IS NULL)
               )
             ORDER BY ts ASC, msg_id ASC",
        )?;
        for tid in tid_candidates {
            let mut rows = stmt.query(params![tid])?;
            while let Some(row) = rows.next()? {
                let dto = self.decrypt_raw_message_tuple(row)?;
                if seen.insert((dto.thread_id.clone(), dto.msg_id.clone())) {
                    out.push(dto);
                }
            }
        }
        Ok(out)
    }

    fn delete_message_row_in_tx(tx: &Transaction<'_>, thread_id: &str, msg_id: &str) -> Result<()> {
        tx.execute(
            "DELETE FROM messages WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;
        tx.execute(
            "DELETE FROM message_translations WHERE msg_id = ?1",
            params![msg_id],
        )?;
        tx.execute(
            "DELETE FROM message_media WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;
        Ok(())
    }

    /// Atomically upserts or merges a poll message in `thread_id` keyed by `(thread_id, poll_id)`
    /// so vote/update pushes referencing an existing real server poll NEVER create duplicate rows.
    ///
    /// Returns `(saved_message, was_updated_existing)`:
    /// - `true` when an existing real server poll row was found and updated in place (preserving
    ///   its original `msg_id`, `ts`, `sender_id`, `sender_name`, `is_outgoing`).
    /// - `false` when no real server poll row existed yet (brand-new poll or replacing only
    ///   synthetic optimistic `poll-*`/`local-poll-*` rows).
    pub fn upsert_or_merge_poll_message(
        &self,
        thread_id: &str,
        poll_id: i64,
        incoming: &MessageDto,
    ) -> Result<(MessageDto, bool)> {
        let incoming_poll = parse_poll_dto_from_attachment_name(incoming.attachment_name.as_deref());
        let effective_poll_id = if poll_id > 0 {
            poll_id
        } else {
            incoming_poll
                .as_ref()
                .map(|p| p.poll_id)
                .filter(|&id| id > 0)
                .unwrap_or(0)
        };

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if effective_poll_id <= 0 {
            let existed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE thread_id = ?1 AND msg_id = ?2)",
                params![&incoming.thread_id, &incoming.msg_id],
                |r| r.get::<_, i64>(0),
            )? != 0;
            let (ts, sender_norm, text_norm, _) =
                Self::upsert_message_row_in_tx(&self.cipher, &tx, incoming, false)?;
            tx.commit()?;

            let fts_tx = self.fts_conn.unchecked_transaction()?;
            fts_tx.execute(
                "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                params![&incoming.thread_id, &incoming.msg_id],
            )?;
            fts_tx.execute(
                "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![&incoming.msg_id, &incoming.thread_id, ts, sender_norm, text_norm],
            )?;
            fts_tx.commit()?;

            let mut saved = incoming.clone();
            saved.ts = ts;
            return Ok((saved, existed));
        }

        let tid_candidates = poll_thread_id_candidates(thread_id, Some(&incoming.thread_id));
        let exact_poll_id = format!("poll-{effective_poll_id}");
        let exact_local_poll_id = format!("local-poll-{effective_poll_id}");

        let candidates = self.query_poll_candidates_in_tx(&tx, &tid_candidates)?;
        let mut real_rows: Vec<MessageDto> = Vec::new();
        let mut synthetic_rows: Vec<MessageDto> = Vec::new();
        for row in candidates {
            let matches = row.msg_id == exact_poll_id
                || row.msg_id == exact_local_poll_id
                || parse_poll_dto_from_attachment_name(row.attachment_name.as_deref())
                    .is_some_and(|dto| dto.poll_id == effective_poll_id);
            if !matches {
                continue;
            }
            if is_synthetic_poll_msg_id(&row.msg_id) {
                synthetic_rows.push(row);
            } else {
                real_rows.push(row);
            }
        }
        real_rows.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));
        synthetic_rows.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));

        let mut to_delete: Vec<(String, String)> = Vec::new();
        let (mut saved, was_updated_existing) = if !real_rows.is_empty() {
            // Case 1: At least one real server poll row exists; keep earliest real row as canonical.
            let mut canonical = real_rows.remove(0);
            let mut merged_poll: Option<PollDto> = None;

            for syn in &synthetic_rows {
                merged_poll = fold_poll_dto(
                    merged_poll,
                    parse_poll_dto_from_attachment_name(syn.attachment_name.as_deref()),
                );
                to_delete.push((syn.thread_id.clone(), syn.msg_id.clone()));
            }
            merged_poll = fold_poll_dto(
                merged_poll,
                parse_poll_dto_from_attachment_name(canonical.attachment_name.as_deref()),
            );
            for dup in &real_rows {
                merged_poll = fold_poll_dto(
                    merged_poll,
                    parse_poll_dto_from_attachment_name(dup.attachment_name.as_deref()),
                );
                to_delete.push((dup.thread_id.clone(), dup.msg_id.clone()));
            }
            merged_poll = fold_poll_dto(merged_poll, incoming_poll);

            canonical.msg_type = "chat.poll".to_string();
            canonical.attachment_url = None;
            if incoming.msg_id == canonical.msg_id {
                canonical.is_pinned |= incoming.is_pinned;
                canonical.is_recalled |= incoming.is_recalled;
                if canonical.cli_msg_id.is_none() {
                    canonical.cli_msg_id = incoming.cli_msg_id.clone();
                }
            }
            if let Some(merged) = merged_poll {
                let poll_json = serde_json::to_string(&merged)?;
                canonical.attachment_name = Some(format!("teza-poll:{poll_json}"));
                if let Some(formatted) = format_merged_poll_text(&merged.question) {
                    canonical.text = formatted;
                } else if canonical.text.trim().is_empty() {
                    canonical.text = "Bình chọn".to_string();
                }
            }
            (canonical, true)
        } else {
            // Case 2: No existing real server poll row; merge any synthetic row metadata & remove synthetics.
            let mut target = incoming.clone();
            let mut syn_poll: Option<PollDto> = None;
            for syn in &synthetic_rows {
                syn_poll = fold_poll_dto(
                    syn_poll,
                    parse_poll_dto_from_attachment_name(syn.attachment_name.as_deref()),
                );
                if !(syn.thread_id == target.thread_id && syn.msg_id == target.msg_id) {
                    to_delete.push((syn.thread_id.clone(), syn.msg_id.clone()));
                }
            }
            if syn_poll.is_some() {
                if let Some(merged) = fold_poll_dto(syn_poll, incoming_poll) {
                    let poll_json = serde_json::to_string(&merged)?;
                    target.attachment_name = Some(format!("teza-poll:{poll_json}"));
                    target.attachment_url = None;
                    let trimmed_text = target.text.trim();
                    if trimmed_text.is_empty()
                        || trimmed_text == "Bình chọn"
                        || trimmed_text == "[Bình chọn]"
                    {
                        if let Some(formatted) = format_merged_poll_text(&merged.question) {
                            target.text = formatted;
                        }
                    }
                }
            }
            (target, false)
        };

        for (del_tid, del_mid) in &to_delete {
            Self::delete_message_row_in_tx(&tx, del_tid, del_mid)?;
        }

        let (ts, sender_norm, text_norm, _) =
            Self::upsert_message_row_in_tx(&self.cipher, &tx, &saved, was_updated_existing)?;
        saved.ts = ts;
        tx.commit()?;

        let fts_tx = self.fts_conn.unchecked_transaction()?;
        for (del_tid, del_mid) in &to_delete {
            fts_tx.execute(
                "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                params![del_tid, del_mid],
            )?;
        }
        fts_tx.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
            params![&saved.thread_id, &saved.msg_id],
        )?;
        fts_tx.execute(
            "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&saved.msg_id, &saved.thread_id, ts, sender_norm, text_norm],
        )?;
        fts_tx.commit()?;

        Ok((saved, was_updated_existing))
    }

    /// Cleans up pre-existing duplicate poll rows in SQLite for the same `(thread_id, poll_id)`,
    /// keeping the earliest real row, merging newer `PollDto` state into it, and removing extra
    /// duplicate rows from `messages`, `message_translations`, `message_media`, and `messages_fts`.
    /// Returns the number of duplicate rows removed.
    pub fn dedupe_duplicate_poll_messages_in_thread(&self, thread_id: &str) -> Result<usize> {
        let tid_candidates = poll_thread_id_candidates(thread_id, None);
        if tid_candidates.is_empty() {
            return Ok(0);
        }

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let candidates = self.query_poll_candidates_in_tx(&tx, &tid_candidates)?;

        let mut grouped: BTreeMap<i64, Vec<MessageDto>> = BTreeMap::new();
        for msg in candidates {
            if let Some(pid) = extract_poll_id_from_message(&msg) {
                grouped.entry(pid).or_default().push(msg);
            }
        }

        let mut to_delete: Vec<(String, String)> = Vec::new();
        let mut fts_updates: Vec<(String, String, i64, String, String)> = Vec::new();

        for (_pid, rows) in grouped {
            if rows.len() <= 1 {
                continue;
            }
            let (mut synthetic_rows, mut real_rows): (Vec<MessageDto>, Vec<MessageDto>) = rows
                .into_iter()
                .partition(|m| is_synthetic_poll_msg_id(&m.msg_id));
            real_rows.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));
            synthetic_rows.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));

            let mut merged_poll: Option<PollDto> = None;
            let mut canonical = if !real_rows.is_empty() {
                let first_real = real_rows.remove(0);
                for syn in &synthetic_rows {
                    merged_poll = fold_poll_dto(
                        merged_poll,
                        parse_poll_dto_from_attachment_name(syn.attachment_name.as_deref()),
                    );
                    to_delete.push((syn.thread_id.clone(), syn.msg_id.clone()));
                }
                merged_poll = fold_poll_dto(
                    merged_poll,
                    parse_poll_dto_from_attachment_name(first_real.attachment_name.as_deref()),
                );
                for dup in &real_rows {
                    merged_poll = fold_poll_dto(
                        merged_poll,
                        parse_poll_dto_from_attachment_name(dup.attachment_name.as_deref()),
                    );
                    to_delete.push((dup.thread_id.clone(), dup.msg_id.clone()));
                }
                first_real
            } else {
                let first_syn = synthetic_rows.remove(0);
                merged_poll = fold_poll_dto(
                    merged_poll,
                    parse_poll_dto_from_attachment_name(first_syn.attachment_name.as_deref()),
                );
                for dup in &synthetic_rows {
                    merged_poll = fold_poll_dto(
                        merged_poll,
                        parse_poll_dto_from_attachment_name(dup.attachment_name.as_deref()),
                    );
                    to_delete.push((dup.thread_id.clone(), dup.msg_id.clone()));
                }
                first_syn
            };

            canonical.msg_type = "chat.poll".to_string();
            canonical.attachment_url = None;
            if let Some(merged) = merged_poll {
                let poll_json = serde_json::to_string(&merged)?;
                canonical.attachment_name = Some(format!("teza-poll:{poll_json}"));
                if let Some(formatted) = format_merged_poll_text(&merged.question) {
                    canonical.text = formatted;
                } else if canonical.text.trim().is_empty() {
                    canonical.text = "Bình chọn".to_string();
                }
            }

            let (ts, sender_norm, text_norm, _) =
                Self::upsert_message_row_in_tx(&self.cipher, &tx, &canonical, true)?;
            fts_updates.push((
                canonical.thread_id,
                canonical.msg_id,
                ts,
                sender_norm,
                text_norm,
            ));
        }

        if to_delete.is_empty() {
            return Ok(0);
        }

        for (del_tid, del_mid) in &to_delete {
            Self::delete_message_row_in_tx(&tx, del_tid, del_mid)?;
        }
        tx.commit()?;

        let fts_tx = self.fts_conn.unchecked_transaction()?;
        for (del_tid, del_mid) in &to_delete {
            fts_tx.execute(
                "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                params![del_tid, del_mid],
            )?;
        }
        for (tid, mid, ts, sender_norm, text_norm) in fts_updates {
            fts_tx.execute(
                "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                params![&tid, &mid],
            )?;
            fts_tx.execute(
                "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![&mid, &tid, ts, sender_norm, text_norm],
            )?;
        }
        fts_tx.commit()?;

        Ok(to_delete.len())
    }

    /// Deduplicates an optimistic local poll message (`poll-{poll_id}`, `local-poll-{poll_id}`,
    /// or a `local-poll-*` row whose `teza-poll:` payload carries `poll_id`) against the real
    /// server message `real_msg`, removing the synthetic row from `messages` + `messages_fts`
    /// and upserting `real_msg` atomically.
    pub fn replace_optimistic_poll_message(
        &self,
        thread_id: &str,
        poll_id: i64,
        real_msg: &MessageDto,
    ) -> Result<()> {
        let _ = self.upsert_or_merge_poll_message(thread_id, poll_id, real_msg)?;
        Ok(())
    }

    fn decrypt_raw_message_tuple(&self, row: &rusqlite::Row<'_>) -> Result<MessageDto> {
        let msg_id: String = row.get(0)?;
        let thread_id: String = row.get(1)?;
        let aad = msg_aad(&thread_id, &msg_id);
        let sender_id = self.cipher.decrypt_str(&aad, &row.get::<_, String>(2)?)?;
        let sender_name = self.cipher.decrypt_str(&aad, &row.get::<_, String>(3)?)?;
        let text = self.cipher.decrypt_str(&aad, &row.get::<_, String>(4)?)?;
        let msg_type: String = row.get(5)?;
        let ts: i64 = row.get(6)?;
        let is_outgoing = row.get::<_, i64>(7)? != 0;
        let is_recalled = row.get::<_, i64>(8)? != 0;
        let is_pinned = row.get::<_, i64>(9)? != 0;
        let quote_json = self.cipher.decrypt_opt(&aad, row.get(10)?)?;
        let reactions_json = self.cipher.decrypt_str(&aad, &row.get::<_, String>(11)?)?;
        let attachment_url = self.cipher.decrypt_opt(&aad, row.get(12)?)?;
        let attachment_name = self.cipher.decrypt_opt(&aad, row.get(13)?)?;
        let raw_cli_msg_id: String = row.get(14).unwrap_or_default();
        let cli_msg_id = {
            let trimmed = raw_cli_msg_id.trim();
            if trimmed.is_empty() || trimmed == "0" {
                None
            } else {
                Some(trimmed.to_string())
            }
        };

        let quote: Option<QuoteRef> = quote_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let reactions: Vec<ReactionSummary> =
            serde_json::from_str(&reactions_json).unwrap_or_default();

        Ok(MessageDto {
            msg_id,
            cli_msg_id,
            thread_id,
            sender_id,
            sender_name,
            text,
            msg_type,
            ts,
            is_outgoing,
            is_recalled,
            is_pinned,
            quote,
            reactions,
            attachment_url,
            attachment_name,
        })
    }

    fn query_messages<P: rusqlite::Params>(&self, sql: &str, params: P) -> Result<Vec<MessageDto>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.decrypt_raw_message_tuple(row)?);
        }
        Ok(out)
    }

    pub fn get_message(&self, thread_id: &str, msg_id: &str) -> Result<Option<MessageDto>> {
        let mut msgs = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages WHERE thread_id = ?1 AND msg_id = ?2 LIMIT 1",
            params![thread_id, msg_id],
        )?;
        Ok(msgs.pop())
    }

    /// Look up a message in `thread_id` by server `msg_id` (`gMsgID`) and/or client `cli_msg_id` (`cMsgID`).
    /// Also normalizes `group_`/`g`-prefixed group thread IDs and falls back to matching `ts == cMsgID`
    /// for pre-migration rows that did not store `cli_msg_id`.
    pub fn get_message_by_ids(
        &self,
        thread_id: &str,
        msg_id: &str,
        cli_msg_id: Option<&str>,
    ) -> Result<Option<MessageDto>> {
        let mut tid_candidates: Vec<&str> = Vec::with_capacity(2);
        let raw_tid = thread_id.trim();
        if !raw_tid.is_empty() {
            tid_candidates.push(raw_tid);
            let bare_tid = clean_group_id(raw_tid);
            if !bare_tid.is_empty() && bare_tid != raw_tid {
                tid_candidates.push(bare_tid);
            }
        }
        let clean_mid = msg_id.trim();
        let clean_cid = cli_msg_id
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0");

        for tid in &tid_candidates {
            if !clean_mid.is_empty() && clean_mid != "0" {
                if let Some(m) = self.get_message(tid, clean_mid)? {
                    return Ok(Some(m));
                }
                let mut by_cli = self.query_messages(
                    "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                     FROM messages WHERE thread_id = ?1 AND cli_msg_id = ?2 LIMIT 1",
                    params![*tid, clean_mid],
                )?;
                if let Some(m) = by_cli.pop() {
                    return Ok(Some(m));
                }
            }
            if let Some(cid) = clean_cid {
                let mut by_cli = self.query_messages(
                    "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                     FROM messages WHERE thread_id = ?1 AND cli_msg_id = ?2 LIMIT 1",
                    params![*tid, cid],
                )?;
                if let Some(m) = by_cli.pop() {
                    return Ok(Some(m));
                }
                if cid != clean_mid {
                    if let Some(m) = self.get_message(tid, cid)? {
                        return Ok(Some(m));
                    }
                }
            }
        }

        // Legacy fallback for rows stored before `cli_msg_id` was persisted:
        // outgoing Teza messages originally stored `client_id` as `ts`.
        let ts_probe = clean_cid
            .or_else(|| (!clean_mid.is_empty() && clean_mid != "0").then_some(clean_mid))
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|&n| n >= 1_000_000_000_000);
        if let Some(ts_val) = ts_probe {
            for tid in &tid_candidates {
                let mut by_ts = self.query_messages(
                    "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                     FROM messages WHERE thread_id = ?1 AND ts = ?2 AND cli_msg_id = '' LIMIT 1",
                    params![*tid, ts_val],
                )?;
                if let Some(m) = by_ts.pop() {
                    return Ok(Some(m));
                }
            }
        }

        Ok(None)
    }

    pub fn mark_recalled(&mut self, thread_id: &str, msg_id: &str) -> Result<Option<MessageDto>> {
        let Some(mut msg) = self.get_message(thread_id, msg_id)? else {
            return Ok(None);
        };
        msg.is_recalled = true;
        msg.text = "[Tin nhắn đã thu hồi]".to_string();
        msg.attachment_url = None;
        msg.attachment_name = None;
        let _ = self.upsert_message_inner(&msg, true)?;
        Ok(Some(msg))
    }

    pub fn delete_message(&mut self, thread_id: &str, msg_id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let existing_ts: Option<i64> = tx
            .query_row(
                "SELECT ts FROM messages WHERE thread_id = ?1 AND msg_id = ?2 LIMIT 1",
                params![thread_id, msg_id],
                |r| r.get(0),
            )
            .optional()?;
        let _ = Self::cleanup_orphaned_media_for_msg_in_tx(
            &tx,
            thread_id,
            msg_id,
            Some("msg_deleted"),
        )?;
        let affected = tx.execute(
            "DELETE FROM messages WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;
        tx.execute(
            "DELETE FROM message_translations WHERE msg_id = ?1",
            params![msg_id],
        )?;
        if affected > 0 {
            let mut_ts = Self::next_local_mutation_ts_in_tx(&tx)?;
            let deleted_ts = mut_ts.max(existing_ts.unwrap_or(0));
            tx.execute(
                "INSERT INTO gdrive_deleted_messages (thread_id, msg_id, deleted_ts, local_mutation_ts)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(thread_id, msg_id) DO UPDATE SET
                     deleted_ts = MAX(gdrive_deleted_messages.deleted_ts, excluded.deleted_ts),
                     local_mutation_ts = MAX(gdrive_deleted_messages.local_mutation_ts, excluded.local_mutation_ts)",
                params![thread_id, msg_id, deleted_ts, mut_ts],
            )?;
        }
        tx.commit()?;

        self.fts_conn.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;
        Ok(affected > 0)
    }

    /// Atomically removes a thread and all of its local messages, translations, media links,
    /// folder memberships, and in-memory FTS entries. Returns `local_rel_path`s of newly
    /// orphaned media blobs so the caller can unlink them from disk outside the store mutex.
    pub fn delete_thread(&mut self, thread_id: &str) -> Result<Vec<String>> {
        let tid = thread_id.trim();
        if tid.is_empty() {
            return Ok(Vec::new());
        }

        let tx = self.conn.transaction()?;

        let max_msg_ts: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(ts), 0) FROM messages WHERE thread_id = ?1",
                params![tid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let thread_last_ts: i64 = tx
            .query_row(
                "SELECT COALESCE(last_ts, 0) FROM threads WHERE id = ?1 LIMIT 1",
                params![tid],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);

        // Delete cached translations for messages belonging exclusively to this thread.
        tx.execute(
            "DELETE FROM message_translations
             WHERE msg_id IN (SELECT msg_id FROM messages WHERE thread_id = ?1)
               AND msg_id NOT IN (SELECT msg_id FROM messages WHERE thread_id != ?1)",
            params![tid],
        )?;

        // Collect media object hashes referenced by this thread before unlinking `message_media`.
        let shas: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT sha256
                 FROM message_media
                 WHERE thread_id = ?1 AND sha256 IS NOT NULL AND sha256 != ''",
            )?;
            let out = stmt
                .query_map(params![tid], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        };

        tx.execute("DELETE FROM message_media WHERE thread_id = ?1", params![tid])?;

        let orphaned_paths =
            Self::cleanup_orphaned_media_for_shas_in_tx(&tx, shas, Some("thread_deleted"))?;

        // Scrub thread membership from any encrypted `chat_folders` entries.
        let folder_rows: Vec<(String, String)> = {
            let mut stmt = tx.prepare("SELECT id, config_json FROM chat_folders")?;
            let out = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            out
        };
        for (fid, enc_json) in folder_rows {
            let aad = folder_aad(&fid);
            if let Ok(raw_json) = self.cipher.decrypt_str(&aad, &enc_json) {
                if let Ok(mut dto) = serde_json::from_str::<ChatFolderDto>(&raw_json) {
                    let prev_inc = dto.thread_ids.len();
                    let prev_exc = dto.excluded_thread_ids.len();
                    dto.thread_ids.retain(|id| id != tid);
                    dto.excluded_thread_ids.retain(|id| id != tid);
                    if dto.thread_ids.len() != prev_inc || dto.excluded_thread_ids.len() != prev_exc
                    {
                        let updated_json = serde_json::to_string(&dto)?;
                        let updated_enc = self.cipher.encrypt_str(&aad, &updated_json)?;
                        tx.execute(
                            "UPDATE chat_folders SET config_json = ?2 WHERE id = ?1",
                            params![fid, updated_enc],
                        )?;
                    }
                }
            }
        }

        let del_msgs = tx.execute("DELETE FROM messages WHERE thread_id = ?1", params![tid])?;
        let del_threads = tx.execute("DELETE FROM threads WHERE id = ?1", params![tid])?;
        if del_msgs > 0 || del_threads > 0 {
            let mut_ts = Self::next_local_mutation_ts_in_tx(&tx)?;
            let server_anchor_ts = max_msg_ts.max(thread_last_ts);
            let deleted_ts = if server_anchor_ts > 0 {
                server_anchor_ts
            } else {
                mut_ts
            };
            tx.execute(
                "INSERT INTO gdrive_deleted_threads (thread_id, deleted_ts, local_mutation_ts)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(thread_id) DO UPDATE SET
                     deleted_ts = MAX(gdrive_deleted_threads.deleted_ts, excluded.deleted_ts),
                     local_mutation_ts = MAX(gdrive_deleted_threads.local_mutation_ts, excluded.local_mutation_ts)",
                params![tid, deleted_ts, mut_ts],
            )?;
        }
        tx.commit()?;

        self.fts_conn.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1",
            params![tid],
        )?;

        Ok(orphaned_paths)
    }

    pub fn get_cached_translations(
        &self,
        items: &[(String, String)],
        target_lang: &str,
    ) -> Result<HashMap<String, TranslationResult>> {
        let lang = target_lang.trim().to_ascii_uppercase();
        let mut out = HashMap::new();
        if lang.is_empty() || items.is_empty() {
            return Ok(out);
        }
        let mut stmt = self.conn.prepare(
            "SELECT text_hash, translated_text, detected_source_lang, skipped
             FROM message_translations
             WHERE msg_id = ?1 AND target_lang = ?2
             LIMIT 1",
        )?;
        for (msg_id, text) in items {
            let expected_hash = self.cipher.keyed_translation_mac(msg_id, text);
            let row: Option<(String, String, Option<String>, i64)> = stmt
                .query_row(params![msg_id, &lang], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            if let Some((stored_hash, enc_text, det_lang, skipped_i64)) = row {
                if stored_hash != expected_hash {
                    continue;
                }
                let Ok(plain) = self
                    .cipher
                    .decrypt_str(&translation_aad(msg_id, &lang), &enc_text)
                else {
                    continue;
                };
                out.insert(
                    msg_id.clone(),
                    TranslationResult {
                        msg_id: msg_id.clone(),
                        translated_text: plain,
                        detected_source_lang: det_lang,
                        skipped: skipped_i64 != 0,
                    },
                );
            }
        }
        Ok(out)
    }

    pub fn upsert_cached_translations(
        &mut self,
        entries: &[(String, String, TranslationResult)],
        target_lang: &str,
        provider: &str,
    ) -> Result<()> {
        let lang = target_lang.trim().to_ascii_uppercase();
        if lang.is_empty() || entries.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO message_translations
                 (msg_id, target_lang, text_hash, provider, translated_text, detected_source_lang, skipped, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for (msg_id, orig_text, tr) in entries {
                let text_hash = self.cipher.keyed_translation_mac(msg_id, orig_text);
                let enc_text = self
                    .cipher
                    .encrypt_str(&translation_aad(msg_id, &lang), &tr.translated_text)?;
                stmt.execute(params![
                    msg_id,
                    &lang,
                    text_hash,
                    provider,
                    enc_text,
                    tr.detected_source_lang.as_deref(),
                    tr.skipped as i64,
                    now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_cached_translations(
        &self,
        msg_ids: Option<&[String]>,
        target_lang: Option<&str>,
    ) -> Result<usize> {
        let lang = target_lang
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase);
        let affected = match (msg_ids, lang.as_deref()) {
            (Some([]), _) => return Ok(0),
            (None, None) => self.conn.execute("DELETE FROM message_translations", [])?,
            (None, Some(l)) => self.conn.execute(
                "DELETE FROM message_translations WHERE target_lang = ?1",
                params![l],
            )?,
            (Some(ids), None) => {
                let tx = self.conn.unchecked_transaction()?;
                let mut total = 0usize;
                {
                    let mut stmt =
                        tx.prepare("DELETE FROM message_translations WHERE msg_id = ?1")?;
                    for id in ids {
                        total += stmt.execute(params![id])?;
                    }
                }
                tx.commit()?;
                total
            }
            (Some(ids), Some(l)) => {
                let tx = self.conn.unchecked_transaction()?;
                let mut total = 0usize;
                {
                    let mut stmt = tx.prepare(
                        "DELETE FROM message_translations WHERE msg_id = ?1 AND target_lang = ?2",
                    )?;
                    for id in ids {
                        total += stmt.execute(params![id, l])?;
                    }
                }
                tx.commit()?;
                total
            }
        };
        Ok(affected)
    }

    pub fn set_message_pinned(
        &self,
        thread_id: &str,
        msg_id: &str,
        pinned: bool,
    ) -> Result<Option<MessageDto>> {
        let mut_ts = self.next_local_mutation_ts()?;
        let affected = self.conn.execute(
            "UPDATE messages SET is_pinned = ?3, updated_ts = ?4 WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id, pinned as i64, mut_ts],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        self.get_message(thread_id, msg_id)
    }

    fn load_reaction_actors(&self, thread_id: &str, msg_id: &str) -> BTreeMap<String, String> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT reaction_actors_json FROM messages WHERE thread_id = ?1 AND msg_id = ?2 LIMIT 1",
                params![thread_id, msg_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        let Some(stored) = raw.filter(|s| !s.trim().is_empty()) else {
            return BTreeMap::new();
        };
        let aad = msg_aad(thread_id, msg_id);
        let Ok(plain) = self.cipher.decrypt_or_legacy(&aad, &stored) else {
            return BTreeMap::new();
        };
        let raw_map = serde_json::from_str::<BTreeMap<String, String>>(&plain).unwrap_or_default();
        raw_map
            .into_iter()
            .filter_map(|(k, v)| canonicalize_reaction_emoji(&v).map(|canon| (k, canon)))
            .collect()
    }

    fn save_message_reactions(
        &self,
        thread_id: &str,
        msg_id: &str,
        reactions: &[ReactionSummary],
        actors: &BTreeMap<String, String>,
    ) -> Result<()> {
        let aad = msg_aad(thread_id, msg_id);
        let reactions_json =
            serde_json::to_string(reactions).unwrap_or_else(|_| "[]".to_string());
        let enc_reactions = self.cipher.encrypt_str(&aad, &reactions_json)?;
        let enc_actors = if actors.is_empty() {
            String::new()
        } else {
            let actors_json = serde_json::to_string(actors).unwrap_or_else(|_| "{}".to_string());
            self.cipher.encrypt_str(&aad, &actors_json)?
        };
        let mut_ts = self.next_local_mutation_ts()?;
        self.conn.execute(
            "UPDATE messages
             SET reactions_json = ?3, reaction_actors_json = ?4, updated_ts = ?5
             WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id, enc_reactions, enc_actors, mut_ts],
        )?;
        Ok(())
    }

    pub fn apply_reaction(
        &mut self,
        thread_id: &str,
        msg_id: &str,
        emoji: Option<&str>,
        me: bool,
        removed: bool,
    ) -> Result<Option<MessageDto>> {
        self.apply_reaction_event(thread_id, msg_id, None, None, emoji, me, removed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_reaction_event(
        &mut self,
        thread_id: &str,
        msg_id: &str,
        cli_msg_id: Option<&str>,
        actor_id: Option<&str>,
        emoji: Option<&str>,
        me: bool,
        removed: bool,
    ) -> Result<Option<MessageDto>> {
        let Some(mut msg) = self.get_message_by_ids(thread_id, msg_id, cli_msg_id)? else {
            return Ok(None);
        };
        let normalized_changed = normalize_reaction_summaries(&mut msg.reactions);
        let mut actors = self.load_reaction_actors(&msg.thread_id, &msg.msg_id);
        let canonical_emoji = emoji.and_then(canonicalize_reaction_emoji);
        let actor_key = if me {
            Some("__me__".to_string())
        } else {
            actor_id
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != "0")
                .map(|s| format!("u:{s}"))
        };

        if removed {
            let mut changed = normalized_changed;
            if me {
                let prev_me = actors.remove("__me__");
                if prev_me.is_some() {
                    changed = true;
                }
                if let Some(pos) = msg.reactions.iter().position(|r| {
                    r.me && canonical_emoji
                        .as_deref()
                        .is_none_or(|target| r.emoji == target)
                }) {
                    msg.reactions[pos].me = false;
                    msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                    if msg.reactions[pos].count == 0 {
                        msg.reactions.remove(pos);
                    }
                    changed = true;
                }
            } else {
                let tracked_prev = actor_key.as_deref().and_then(|k| actors.remove(k));
                if tracked_prev.is_some() {
                    changed = true;
                }
                let target_opt = tracked_prev.or(canonical_emoji);
                if let Some(target) = target_opt {
                    if let Some(pos) = msg
                        .reactions
                        .iter()
                        .position(|r| r.emoji == target && r.count > u32::from(r.me))
                    {
                        msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                        if msg.reactions[pos].count == 0 {
                            msg.reactions.remove(pos);
                        }
                        changed = true;
                    }
                } else if let Some(pos) = msg
                    .reactions
                    .iter()
                    .rposition(|r| r.count > u32::from(r.me))
                {
                    // Peer unreact without rIcon (Zalo sends rIcon="" & rType=-1) for a reaction
                    // that predated per-actor tracking: decrement the peer's non-self reaction.
                    msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                    if msg.reactions[pos].count == 0 {
                        msg.reactions.remove(pos);
                    }
                    changed = true;
                }
            }

            if changed {
                self.save_message_reactions(&msg.thread_id, &msg.msg_id, &msg.reactions, &actors)?;
            }
            return Ok(Some(msg));
        }

        let Some(target) = canonical_emoji else {
            if normalized_changed {
                self.save_message_reactions(&msg.thread_id, &msg.msg_id, &msg.reactions, &actors)?;
            }
            return Ok(Some(msg));
        };

        if me {
            if msg.reactions.iter().any(|r| r.emoji == target && r.me) {
                if normalized_changed
                    || actors.get("__me__").map(String::as_str) != Some(target.as_str())
                {
                    actors.insert("__me__".to_string(), target);
                    self.save_message_reactions(
                        &msg.thread_id,
                        &msg.msg_id,
                        &msg.reactions,
                        &actors,
                    )?;
                }
                return Ok(Some(msg));
            }
            if let Some(prev_pos) = msg.reactions.iter().position(|r| r.me) {
                msg.reactions[prev_pos].me = false;
                msg.reactions[prev_pos].count = msg.reactions[prev_pos].count.saturating_sub(1);
                if msg.reactions[prev_pos].count == 0 {
                    msg.reactions.remove(prev_pos);
                }
            }
            actors.insert("__me__".to_string(), target.clone());
        } else if let Some(ref ak) = actor_key {
            if actors.get(ak).map(String::as_str) == Some(target.as_str()) {
                if normalized_changed {
                    self.save_message_reactions(
                        &msg.thread_id,
                        &msg.msg_id,
                        &msg.reactions,
                        &actors,
                    )?;
                }
                return Ok(Some(msg));
            }
            if let Some(prev_emoji) = actors.get(ak).cloned() {
                if let Some(prev_pos) = msg
                    .reactions
                    .iter()
                    .position(|r| r.emoji == prev_emoji && r.count > u32::from(r.me))
                {
                    msg.reactions[prev_pos].count =
                        msg.reactions[prev_pos].count.saturating_sub(1);
                    if msg.reactions[prev_pos].count == 0 {
                        msg.reactions.remove(prev_pos);
                    }
                }
            }
            actors.insert(ak.clone(), target.clone());
        }

        if let Some(existing) = msg.reactions.iter_mut().find(|r| r.emoji == target) {
            existing.count += 1;
            if me {
                existing.me = true;
            }
        } else {
            msg.reactions.push(ReactionSummary {
                emoji: target,
                count: 1,
                me,
            });
        }
        self.save_message_reactions(&msg.thread_id, &msg.msg_id, &msg.reactions, &actors)?;
        Ok(Some(msg))
    }

    pub fn toggle_reaction(
        &mut self,
        thread_id: &str,
        msg_id: &str,
        emoji: &str,
    ) -> Result<Option<(MessageDto, bool)>> {
        let Some(mut msg) = self.get_message_by_ids(thread_id, msg_id, None)? else {
            return Ok(None);
        };
        normalize_reaction_summaries(&mut msg.reactions);
        let target = canonicalize_reaction_emoji(emoji).unwrap_or_else(|| emoji.trim().to_string());
        let already_me = msg.reactions.iter().any(|r| r.emoji == target && r.me);
        let updated = self
            .apply_reaction_event(
                &msg.thread_id,
                &msg.msg_id,
                msg.cli_msg_id.as_deref(),
                None,
                Some(&target),
                true,
                already_me,
            )?
            .unwrap_or(msg);
        Ok(Some((updated, already_me)))
    }

    pub fn update_thread_preview(
        &self,
        thread_id: &str,
        preview: &str,
        ts: i64,
        inc_unread: bool,
    ) -> Result<()> {
        self.update_thread_preview_with_unread(thread_id, preview, ts, inc_unread, false)
    }

    pub fn update_thread_preview_with_unread(
        &self,
        thread_id: &str,
        preview: &str,
        ts: i64,
        inc_unread: bool,
        clear_unread: bool,
    ) -> Result<()> {
        let _ = self.update_thread_preview_returning_created(
            thread_id,
            preview,
            ts,
            inc_unread,
            clear_unread,
        )?;
        Ok(())
    }

    fn get_thread_row(&self, thread_id: &str) -> Result<Option<ThreadDto>> {
        struct RawThreadRow {
            id: String,
            enc_title: String,
            enc_avatar: String,
            kind_str: String,
            enc_preview: String,
            last_ts: i64,
            unread: i64,
            pinned: i64,
            muted: i64,
            last_read_in_ts: i64,
            peer_read_ts: i64,
            raw_muted_until_ts: i64,
            archived: i64,
        }
        let row: Option<RawThreadRow> = self
            .conn
            .query_row(
                "SELECT id, title, avatar, kind, last_preview, last_ts, unread_count, is_pinned, is_muted, last_read_in_ts, peer_read_ts, muted_until_ts, is_archived
                 FROM threads WHERE id = ?1 LIMIT 1",
                params![thread_id],
                |row| {
                    Ok(RawThreadRow {
                        id: row.get(0)?,
                        enc_title: row.get(1)?,
                        enc_avatar: row.get(2)?,
                        kind_str: row.get(3)?,
                        enc_preview: row.get(4)?,
                        last_ts: row.get(5)?,
                        unread: row.get(6)?,
                        pinned: row.get(7)?,
                        muted: row.get(8)?,
                        last_read_in_ts: row.get(9)?,
                        peer_read_ts: row.get(10)?,
                        raw_muted_until_ts: row.get(11)?,
                        archived: row.get(12)?,
                    })
                },
            )
            .optional()?;
        let Some(RawThreadRow {
            id,
            enc_title,
            enc_avatar,
            kind_str,
            enc_preview,
            last_ts,
            unread,
            pinned,
            muted,
            last_read_in_ts,
            peer_read_ts,
            raw_muted_until_ts,
            archived,
        }) = row
        else {
            return Ok(None);
        };
        let aad = thread_aad(&id);
        let title = self.cipher.decrypt_str(&aad, &enc_title)?;
        let avatar = self.cipher.decrypt_str(&aad, &enc_avatar)?;
        let last_preview = self.cipher.decrypt_str(&aad, &enc_preview)?;
        let kind = match kind_str.as_str() {
            "group" => ThreadKind::Group,
            "cloud" => ThreadKind::Cloud,
            _ => ThreadKind::Direct,
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mute_expired = muted != 0 && raw_muted_until_ts > 0 && raw_muted_until_ts <= now_ms;
        let is_muted = muted != 0 && !mute_expired;
        let muted_until_ts = if is_muted { raw_muted_until_ts } else { 0 };
        Ok(Some(ThreadDto {
            id,
            title,
            avatar,
            kind,
            last_preview,
            last_ts,
            unread_count: unread.max(0) as u32,
            is_pinned: pinned != 0,
            is_muted,
            last_read_in_ts,
            peer_read_ts,
            muted_until_ts,
            is_archived: archived != 0,
        }))
    }

    /// Updates thread preview & unread counters, returning `true` when a brand-new thread row
    /// was inserted locally because `thread_id` did not exist in SQLite yet.
    ///
    /// When inserting a new stub thread, uses `fallback_kind` (defaulting to [`ThreadKind::Direct`]),
    /// prefers `fallback_title` (if non-empty and not raw numeric) over raw `thread_id`, and
    /// `fallback_avatar` if non-empty. When the thread row already exists with a placeholder title
    /// (`title == id`, empty, or raw numeric), upgrades `title` (and `avatar` if currently empty)
    /// without clobbering real names or avatars.
    #[allow(clippy::too_many_arguments)]
    pub fn update_thread_preview_with_fallback_kind_and_meta(
        &self,
        thread_id: &str,
        preview: &str,
        ts: i64,
        inc_unread: bool,
        clear_unread: bool,
        fallback_kind: Option<ThreadKind>,
        fallback_title: Option<&str>,
        fallback_avatar: Option<&str>,
    ) -> Result<bool> {
        let ts = normalize_ts_ms(ts);
        let aad = thread_aad(thread_id);
        let enc_preview = self.cipher.encrypt_str(&aad, preview)?;
        let clean_fallback_title = fallback_title
            .map(str::trim)
            .filter(|t| !t.is_empty() && !is_raw_numeric_uid_like(t));
        let clean_fallback_avatar = fallback_avatar.map(str::trim).filter(|a| !a.is_empty());

        let updated = self.conn.execute(
            "UPDATE threads
             SET last_preview = ?2,
                 last_ts = MAX(last_ts, ?3),
                 last_read_in_ts = CASE
                     WHEN ?5 = 1 THEN MAX(last_read_in_ts, ?3)
                     ELSE last_read_in_ts
                 END,
                 unread_count = CASE
                     WHEN ?5 = 1 THEN 0
                     ELSE unread_count + ?4
                 END
             WHERE id = ?1",
            params![
                thread_id,
                enc_preview,
                ts,
                if inc_unread && !clear_unread { 1 } else { 0 },
                clear_unread as i64
            ],
        )?;
        if updated == 0 {
            self.upsert_thread(&ThreadDto {
                id: thread_id.to_string(),
                title: clean_fallback_title.unwrap_or(thread_id).to_string(),
                avatar: clean_fallback_avatar.unwrap_or("").to_string(),
                kind: fallback_kind.unwrap_or(ThreadKind::Direct),
                last_preview: preview.to_string(),
                last_ts: ts,
                unread_count: if inc_unread && !clear_unread { 1 } else { 0 },
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: if clear_unread { ts } else { 0 },
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })?;
            return Ok(true);
        }

        if clean_fallback_title.is_some() || clean_fallback_avatar.is_some() {
            let existing: Option<(String, String)> = self
                .conn
                .query_row(
                    "SELECT title, avatar FROM threads WHERE id = ?1 LIMIT 1",
                    params![thread_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((enc_title, enc_avatar)) = existing {
                let curr_title = self.cipher.decrypt_str(&aad, &enc_title)?;
                let curr_avatar = self.cipher.decrypt_str(&aad, &enc_avatar)?;
                let upgrade_title = clean_fallback_title
                    .filter(|_| is_placeholder_thread_title(&curr_title, thread_id));
                let upgrade_avatar = clean_fallback_avatar.filter(|_| curr_avatar.trim().is_empty());
                if upgrade_title.is_some() || upgrade_avatar.is_some() {
                    let next_enc_title = match upgrade_title {
                        Some(t) => self.cipher.encrypt_str(&aad, t)?,
                        None => enc_title,
                    };
                    let next_enc_avatar = match upgrade_avatar {
                        Some(a) => self.cipher.encrypt_str(&aad, a)?,
                        None => enc_avatar,
                    };
                    self.conn.execute(
                        "UPDATE threads SET title = ?2, avatar = ?3, title_norm = '' WHERE id = ?1",
                        params![thread_id, next_enc_title, next_enc_avatar],
                    )?;
                }
            }
        }
        Ok(false)
    }

    /// Updates thread preview & unread counters, returning `true` when a brand-new thread row
    /// was inserted locally because `thread_id` did not exist in SQLite yet (defaulting to
    /// [`ThreadKind::Direct`] when creating a new row).
    #[allow(clippy::too_many_arguments)]
    pub fn update_thread_preview_with_fallback_meta(
        &self,
        thread_id: &str,
        preview: &str,
        ts: i64,
        inc_unread: bool,
        clear_unread: bool,
        fallback_title: Option<&str>,
        fallback_avatar: Option<&str>,
    ) -> Result<bool> {
        self.update_thread_preview_with_fallback_kind_and_meta(
            thread_id,
            preview,
            ts,
            inc_unread,
            clear_unread,
            None,
            fallback_title,
            fallback_avatar,
        )
    }

    /// Updates thread preview & unread counters, returning `true` when a brand-new thread row
    /// was inserted locally because `thread_id` did not exist in SQLite yet.
    pub fn update_thread_preview_returning_created(
        &self,
        thread_id: &str,
        preview: &str,
        ts: i64,
        inc_unread: bool,
        clear_unread: bool,
    ) -> Result<bool> {
        self.update_thread_preview_with_fallback_meta(
            thread_id,
            preview,
            ts,
            inc_unread,
            clear_unread,
            None,
            None,
        )
    }

    /// Updates `title` and/or `avatar` on an existing thread row without clobbering `last_preview`,
    /// `last_ts`, `unread_count`, `is_pinned`, `is_muted`, `is_archived`, or `kind`.
    /// Returns `true` if the existing row was modified.
    pub fn upsert_thread_profile_meta(
        &self,
        thread_id: &str,
        title: &str,
        avatar: &str,
    ) -> Result<bool> {
        let tid = thread_id.trim();
        if tid.is_empty() {
            return Ok(false);
        }
        let existing: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT title, avatar FROM threads WHERE id = ?1 LIMIT 1",
                params![tid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((enc_title, enc_avatar)) = existing else {
            return Ok(false);
        };

        let aad = thread_aad(tid);
        let curr_title = self.cipher.decrypt_str(&aad, &enc_title)?;
        let curr_avatar = self.cipher.decrypt_str(&aad, &enc_avatar)?;

        let clean_title = title.trim();
        let clean_avatar = avatar.trim();
        let incoming_title_is_real =
            !clean_title.is_empty() && !is_raw_numeric_uid_like(clean_title);
        let curr_title_is_placeholder = is_placeholder_thread_title(&curr_title, tid);

        let next_title = if incoming_title_is_real
            || (!clean_title.is_empty() && curr_title_is_placeholder)
        {
            clean_title
        } else {
            curr_title.as_str()
        };
        let next_avatar = if !clean_avatar.is_empty() {
            clean_avatar
        } else {
            curr_avatar.as_str()
        };

        if next_title == curr_title && next_avatar == curr_avatar {
            return Ok(false);
        }

        let next_enc_title = if next_title != curr_title {
            self.cipher.encrypt_str(&aad, next_title)?
        } else {
            enc_title
        };
        let next_enc_avatar = if next_avatar != curr_avatar {
            self.cipher.encrypt_str(&aad, next_avatar)?
        } else {
            enc_avatar
        };
        let affected = self.conn.execute(
            "UPDATE threads SET title = ?2, avatar = ?3, title_norm = '' WHERE id = ?1",
            params![tid, next_enc_title, next_enc_avatar],
        )?;
        Ok(affected > 0)
    }

    /// Ensures a `ThreadKind::Direct` row exists for `thread_id`: inserts a new row (with
    /// `last_ts = 0`, `unread_count = 0`) if missing, or updates `title`/`avatar` via
    /// `upsert_thread_profile_meta` if already present, returning the resulting `ThreadDto`.
    pub fn ensure_direct_thread(
        &self,
        thread_id: &str,
        title: &str,
        avatar: &str,
    ) -> Result<ThreadDto> {
        let tid = thread_id.trim();
        if tid.is_empty() {
            bail!("Mã hội thoại không hợp lệ");
        }
        if self.get_thread_row(tid)?.is_some() {
            let _ = self.upsert_thread_profile_meta(tid, title, avatar)?;
            return self
                .get_thread_row(tid)?
                .ok_or_else(|| anyhow!("Không tìm thấy hội thoại sau khi cập nhật"));
        }
        let clean_title = title.trim();
        let effective_title = if clean_title.is_empty() {
            tid.to_string()
        } else {
            clean_title.to_string()
        };
        let dto = ThreadDto {
            id: tid.to_string(),
            title: effective_title,
            avatar: avatar.trim().to_string(),
            kind: ThreadKind::Direct,
            last_preview: String::new(),
            last_ts: 0,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        };
        self.upsert_thread(&dto)?;
        Ok(dto)
    }

    /// Returns direct thread IDs (`kind = 'direct'`, excluding `cloud_*` / `g*`) whose `title`
    /// is empty, equals `id`, or consists solely of ASCII digits, OR whose `avatar` is empty,
    /// ordered by `last_ts DESC` up to `limit`.
    pub fn list_unresolved_direct_thread_ids(&self, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, title, avatar
             FROM threads
             WHERE kind = 'direct'
               AND LOWER(id) NOT LIKE 'cloud\\_%' ESCAPE '\\'
               AND LOWER(id) NOT LIKE 'g%'
             ORDER BY last_ts DESC, id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let trimmed_id = id.trim();
            if trimmed_id.is_empty() {
                continue;
            }
            let lower_id = trimmed_id.to_ascii_lowercase();
            if lower_id.starts_with("cloud_") || lower_id.starts_with('g') {
                continue;
            }
            let enc_title: String = row.get(1)?;
            let enc_avatar: String = row.get(2)?;
            let aad = thread_aad(&id);
            let title = self.cipher.decrypt_str(&aad, &enc_title).unwrap_or_default();
            let avatar = self
                .cipher
                .decrypt_str(&aad, &enc_avatar)
                .unwrap_or_default();
            if is_placeholder_thread_title(&title, &id) || avatar.trim().is_empty() {
                out.push(id);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn mark_thread_read(&self, thread_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE threads
             SET unread_count = 0,
                 last_read_in_ts = MAX(
                     last_read_in_ts,
                     last_ts,
                     COALESCE((SELECT MAX(ts) FROM messages WHERE thread_id = ?1 AND is_outgoing = 0), 0)
                 )
             WHERE id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    pub fn advance_last_read_in_ts(&self, thread_id: &str, ts: i64) -> Result<()> {
        let ts = normalize_ts_ms(ts);
        if ts <= 0 {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE threads SET last_read_in_ts = MAX(last_read_in_ts, ?2) WHERE id = ?1",
            params![thread_id, ts],
        )?;
        Ok(())
    }

    pub fn set_thread_unread(&self, thread_id: &str, unread: u32) -> Result<()> {
        if unread == 0 {
            return self.mark_thread_read(thread_id);
        }
        self.conn.execute(
            "UPDATE threads
             SET unread_count = ?2,
                 last_read_in_ts = CASE
                     WHEN (SELECT MAX(ts) FROM messages WHERE thread_id = ?1 AND is_outgoing = 0) IS NOT NULL
                          AND last_read_in_ts >= (SELECT MAX(ts) FROM messages WHERE thread_id = ?1 AND is_outgoing = 0)
                     THEN (SELECT MAX(ts) FROM messages WHERE thread_id = ?1 AND is_outgoing = 0) - 1
                     ELSE last_read_in_ts
                 END
             WHERE id = ?1",
            params![thread_id, unread],
        )?;
        Ok(())
    }

    pub fn update_peer_read_ts(&self, thread_id: &str, seen_ts: i64) -> Result<i64> {
        let ts = normalize_ts_ms(seen_ts);
        self.conn.execute(
            "UPDATE threads SET peer_read_ts = MAX(peer_read_ts, ?2) WHERE id = ?1",
            params![thread_id, ts],
        )?;
        let saved: Option<i64> = self
            .conn
            .query_row(
                "SELECT peer_read_ts FROM threads WHERE id = ?1 LIMIT 1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(saved.unwrap_or(ts))
    }

    pub fn get_latest_incoming_message(&self, thread_id: &str) -> Result<Option<(String, String, i64)>> {
        let row: Option<(String, String, i64)> = self
            .conn
            .query_row(
                "SELECT msg_id, sender_id, ts FROM messages
                 WHERE thread_id = ?1 AND is_outgoing = 0
                 ORDER BY ts DESC LIMIT 1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((msg_id, enc_sender_id, ts)) = row else {
            return Ok(None);
        };
        let sender_id = self
            .cipher
            .decrypt_str(&msg_aad(thread_id, &msg_id), &enc_sender_id)?;
        Ok(Some((msg_id, sender_id, ts)))
    }

    pub fn get_latest_incoming_message_with_type(
        &self,
        thread_id: &str,
    ) -> Result<Option<(String, String, String)>> {
        let Some((msg_id, _cli_msg_id, sender_id, msg_type, _ts)) =
            self.get_latest_incoming_message_for_seen(thread_id)?
        else {
            return Ok(None);
        };
        Ok(Some((msg_id, sender_id, msg_type)))
    }

    /// Returns `(msg_id, cli_msg_id, sender_id, msg_type, ts)` for the latest incoming message
    /// in `thread_id`, so `seenv2` can send the exact server/client IDs and timestamp.
    pub fn get_latest_incoming_message_for_seen(
        &self,
        thread_id: &str,
    ) -> Result<Option<IncomingSeenMeta>> {
        let row: Option<(String, String, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT msg_id, cli_msg_id, sender_id, msg_type, ts FROM messages
                 WHERE thread_id = ?1 AND is_outgoing = 0
                 ORDER BY ts DESC, msg_id DESC LIMIT 1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((msg_id, raw_cli_msg_id, enc_sender_id, msg_type, ts)) = row else {
            return Ok(None);
        };
        let sender_id = self
            .cipher
            .decrypt_str(&msg_aad(thread_id, &msg_id), &enc_sender_id)?;
        let normalized_type = if msg_type.trim().is_empty() {
            "chat.text".to_string()
        } else {
            msg_type
        };
        let cli_msg_id = {
            let trimmed = raw_cli_msg_id.trim();
            if trimmed.is_empty() || trimmed == "0" {
                None
            } else {
                Some(trimmed.to_string())
            }
        };
        Ok(Some((msg_id, cli_msg_id, sender_id, normalized_type, ts)))
    }

    pub fn set_thread_pinned(&self, thread_id: &str, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET is_pinned = ?2 WHERE id = ?1",
            params![thread_id, pinned as i64],
        )?;
        Ok(())
    }

    pub fn set_thread_muted_until(
        &self,
        thread_id: &str,
        muted: bool,
        muted_until_ts: i64,
    ) -> Result<()> {
        let until = if muted { muted_until_ts.max(0) } else { 0 };
        self.conn.execute(
            "UPDATE threads SET is_muted = ?2, muted_until_ts = ?3 WHERE id = ?1",
            params![thread_id, muted as i64, until],
        )?;
        Ok(())
    }

    pub fn set_thread_muted(&self, thread_id: &str, muted: bool) -> Result<()> {
        self.set_thread_muted_until(thread_id, muted, 0)
    }

    pub fn set_thread_archived(&self, thread_id: &str, archived: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET is_archived = ?2 WHERE id = ?1",
            params![thread_id, archived as i64],
        )?;
        Ok(())
    }

    pub fn resolve_known_profiles(
        &self,
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, avatar FROM threads WHERE kind = 'direct'")?;
        let mut rows = stmt.query([])?;
        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let enc_title: String = row.get(1)?;
            let enc_avatar: String = row.get(2)?;
            let aad = thread_aad(&id);
            if let Ok(title) = self.cipher.decrypt_str(&aad, &enc_title) {
                let avatar = self.cipher.decrypt_str(&aad, &enc_avatar).unwrap_or_default();
                if (!title.is_empty() && title != id) || !avatar.is_empty() {
                    map.insert(id, (title, avatar));
                }
            }
        }
        Ok(map)
    }

    pub fn resolve_known_titles(&self) -> Result<std::collections::HashMap<String, String>> {
        Ok(self
            .resolve_known_profiles()?
            .into_iter()
            .filter_map(|(id, (title, _))| {
                (!title.is_empty() && title != id).then_some((id, title))
            })
            .collect())
    }

    pub fn list_thread_media(
        &self,
        thread_id: &str,
        limit: usize,
    ) -> Result<(Vec<MessageDto>, Vec<MessageDto>)> {
        let scan_limit = limit.saturating_mul(6).max(300) as i64;
        let rows = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE thread_id = ?1
               AND is_recalled = 0
               AND attachment_url IS NOT NULL
               AND attachment_url != ''
               AND LOWER(msg_type) NOT IN ('chat.sticker', 'sticker', '35', '36')
             ORDER BY ts DESC, msg_id DESC
             LIMIT ?2",
            params![thread_id, scan_limit],
        )?;
        let mut media = Vec::new();
        let mut files = Vec::new();
        for mut m in rows {
            if media.len() >= limit && files.len() >= limit {
                break;
            }
            let mt_lower = m.msg_type.to_lowercase();
            if mt_lower.contains("sticker")
                || mt_lower.contains("reminder")
                || mt_lower.contains("poll")
                || matches!(
                    mt_lower.as_str(),
                    "chat.link"
                        | "link"
                        | "chat.location"
                        | "chat.location.new"
                        | "chat.recommended"
                        | "chat.ecard"
                )
                || m.msg_type == "5"
                || m.msg_type == "35"
                || m.msg_type == "36"
            {
                continue;
            }
            let Some(raw_url) = m.attachment_url.as_deref() else {
                continue;
            };
            let Ok(normalized_url) = crate::session::normalize_allowed_media_url(raw_url) else {
                continue;
            };
            let url_str = normalized_url.to_string();
            let url_lower = url_str.to_lowercase();
            if url_lower.contains("/reminder_") || url_lower.contains("/reminder-") {
                continue;
            }
            m.attachment_url = Some(url_str);
            let is_img_or_video = m.msg_type.contains("photo")
                || m.msg_type == "2"
                || m.msg_type.contains("video")
                || m.msg_type == "4"
                || m.msg_type == "44"
                || m.msg_type.contains("gif")
                || [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".mp4", ".mov", ".webm"]
                    .iter()
                    .any(|ext| url_lower.contains(ext));
            if is_img_or_video {
                if media.len() < limit {
                    media.push(m);
                }
            } else if files.len() < limit {
                files.push(m);
            }
        }
        Ok((media, files))
    }

    pub fn get_thread_kind(&self, thread_id: &str) -> Result<Option<ThreadKind>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind FROM threads WHERE id = ?1 LIMIT 1")?;
        let kind_str: Option<String> = stmt
            .query_map(params![thread_id], |r| r.get(0))?
            .next()
            .transpose()?;
        Ok(kind_str.map(|s| match s.as_str() {
            "group" => ThreadKind::Group,
            "cloud" => ThreadKind::Cloud,
            _ => ThreadKind::Direct,
        }))
    }

    pub fn list_thread_links(&self, thread_id: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let scan_limit = limit.saturating_mul(10).max(400) as i64;
        let rows = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE thread_id = ?1 AND is_recalled = 0
             ORDER BY ts DESC, msg_id DESC
             LIMIT ?2",
            params![thread_id, scan_limit],
        )?;
        let mut links = Vec::new();
        for m in rows {
            if links.len() >= limit {
                break;
            }
            let lower = m.text.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                links.push(m);
            }
        }
        Ok(links)
    }

    pub fn list_chat_folders(&self) -> Result<Vec<ChatFolderDto>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, config_json FROM chat_folders ORDER BY position ASC, id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let enc_json: String = row.get(1)?;
            let raw_json = self.cipher.decrypt_str(&folder_aad(&id), &enc_json)?;
            if let Ok(mut dto) = serde_json::from_str::<ChatFolderDto>(&raw_json) {
                if dto.id.is_empty() {
                    dto.id = id;
                }
                out.push(dto);
            }
        }
        Ok(out)
    }

    pub fn save_chat_folder(&self, folder: &ChatFolderDto) -> Result<()> {
        let aad = folder_aad(&folder.id);
        let enc_name = self.cipher.encrypt_str(&aad, &folder.name)?;
        let raw_json = serde_json::to_string(folder)?;
        let enc_json = self.cipher.encrypt_str(&aad, &raw_json)?;
        self.conn.execute(
            "INSERT INTO chat_folders (id, name, icon, config_json, position)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 icon = excluded.icon,
                 config_json = excluded.config_json,
                 position = excluded.position",
            params![folder.id, enc_name, folder.icon, enc_json, folder.position],
        )?;
        Ok(())
    }

    pub fn delete_chat_folder(&self, folder_id: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "DELETE FROM chat_folders WHERE id = ?1",
            params![folder_id],
        )?;
        Ok(affected > 0)
    }

    pub fn reorder_chat_folders(&self, ordered_ids: &[String]) -> Result<()> {
        let mut existing = self.list_chat_folders()?;
        if existing.is_empty() {
            return Ok(());
        }
        let mut ordered: Vec<ChatFolderDto> = Vec::with_capacity(existing.len());
        for id in ordered_ids {
            if let Some(pos) = existing.iter().position(|f| &f.id == id) {
                ordered.push(existing.remove(pos));
            }
        }
        ordered.extend(existing);
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<()> {
            for (idx, mut folder) in ordered.into_iter().enumerate() {
                folder.position = (idx + 1) as i64;
                self.save_chat_folder(&folder)?;
            }
            Ok(())
        })();
        match res {
            Ok(()) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn thread_interaction_stats(&self, thread_id: &str) -> Result<(u64, u64, i64)> {
        let row: (i64, i64, i64) = self.conn.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN attachment_url IS NOT NULL AND attachment_url != '' AND LOWER(msg_type) NOT IN ('chat.sticker', 'sticker', '5', '35', '36') THEN 1 ELSE 0 END), 0),
                COALESCE(MAX(ts), 0)
             FROM messages
             WHERE thread_id = ?1 AND is_recalled = 0",
            params![thread_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((row.0.max(0) as u64, row.1.max(0) as u64, row.2))
    }

    pub fn list_recent_stickers(&self, limit: usize) -> Result<Vec<(u64, u64, String)>> {
        let scan_limit = limit.saturating_mul(5).max(150) as i64;
        let rows = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE is_recalled = 0
               AND attachment_url IS NOT NULL
               AND attachment_url != ''
               AND LOWER(msg_type) IN ('chat.sticker', 'sticker', '5', '35', '36')
             ORDER BY ts DESC, msg_id DESC
             LIMIT ?1",
            params![scan_limit],
        )?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for m in rows {
            if out.len() >= limit {
                break;
            }
            let Some(url) = m.attachment_url.clone().filter(|u| !u.is_empty()) else {
                continue;
            };
            let mut cate_id = 0_u64;
            let mut sticker_id = 0_u64;
            if let Some(name) = m.attachment_name.as_deref() {
                if let Some(rest) = name.strip_prefix("sticker_") {
                    let parts: Vec<&str> = rest.split(['_', '.']).collect();
                    if parts.len() >= 2 {
                        cate_id = parts[0].parse().unwrap_or(0);
                        sticker_id = parts[1].parse().unwrap_or(0);
                    }
                }
            }
            if sticker_id == 0 {
                if let Some(eid_part) = url.split("eid=").nth(1) {
                    sticker_id = eid_part
                        .split('&')
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                } else if let Some(last) = url.split('/').next_back() {
                    sticker_id = last
                        .split('.')
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                }
            }
            if sticker_id > 0 && seen.insert(sticker_id) {
                out.push((sticker_id, cate_id, url));
            }
        }
        Ok(out)
    }

    pub fn list_pinned_messages(&self, thread_id: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let mut msgs = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE thread_id = ?1 AND is_pinned = 1 AND is_recalled = 0
             ORDER BY ts DESC, msg_id DESC
             LIMIT ?2",
            params![thread_id, limit as i64],
        )?;
        msgs.reverse();
        Ok(msgs)
    }

    pub fn load_messages_before(
        &self,
        thread_id: &str,
        before_ts: Option<i64>,
        before_msg_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageDto>> {
        let cutoff_ts = before_ts.unwrap_or(i64::MAX);
        let mut msgs = match before_msg_id.filter(|s| !s.trim().is_empty()) {
            Some(cutoff_id) if before_ts.is_some() => self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                 FROM messages
                 WHERE thread_id = ?1 AND (ts < ?2 OR (ts = ?2 AND msg_id < ?3))
                 ORDER BY ts DESC, msg_id DESC
                 LIMIT ?4",
                params![thread_id, cutoff_ts, cutoff_id, limit as i64],
            )?,
            _ => self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                 FROM messages
                 WHERE thread_id = ?1 AND ts < ?2
                 ORDER BY ts DESC, msg_id DESC
                 LIMIT ?3",
                params![thread_id, cutoff_ts, limit as i64],
            )?,
        };
        msgs.reverse();
        Ok(msgs)
    }

    pub fn load_messages(
        &self,
        thread_id: &str,
        before_ts: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageDto>> {
        self.load_messages_before(thread_id, before_ts, None, limit)
    }

    pub fn oldest_server_msg_id(&self, thread_id: &str) -> Result<Option<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts, msg_id FROM messages
             WHERE thread_id = ?1
               AND msg_id NOT LIKE 'local-%'
               AND msg_id NOT LIKE 'out_%'
             ORDER BY ts ASC, msg_id ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![thread_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    /// Load messages starting from a historical timestamp (with `context_before` earlier messages
    /// and up to `max_after` messages from `target_ts` onward), returning `(messages, has_newer)`
    /// where `has_newer` is `true` iff additional messages exist after the returned slice.
    pub fn load_messages_around_with_meta(
        &self,
        thread_id: &str,
        target_ts: i64,
        context_before: usize,
        max_after: usize,
    ) -> Result<(Vec<MessageDto>, bool)> {
        let mut msgs = if context_before > 0 {
            let mut before = self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                 FROM messages WHERE thread_id = ?1 AND ts < ?2
                 ORDER BY ts DESC, msg_id DESC LIMIT ?3",
                params![thread_id, target_ts, context_before as i64],
            )?;
            before.reverse();
            before
        } else {
            Vec::new()
        };

        let fetch_after = i64::try_from(max_after.saturating_add(1)).unwrap_or(i64::MAX);
        let mut after = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages WHERE thread_id = ?1 AND ts >= ?2
             ORDER BY ts ASC, msg_id ASC LIMIT ?3",
            params![thread_id, target_ts, fetch_after],
        )?;
        let has_newer = after.len() > max_after;
        if has_newer {
            after.truncate(max_after);
        }
        msgs.extend(after);
        Ok((msgs, has_newer))
    }

    /// Load messages starting from a historical timestamp (with `context_before` earlier messages
    /// and up to `max_after` messages from `target_ts` onward) so jumping to a search hit renders it.
    pub fn load_messages_around(
        &self,
        thread_id: &str,
        target_ts: i64,
        context_before: usize,
        max_after: usize,
    ) -> Result<Vec<MessageDto>> {
        let (msgs, _has_newer) =
            self.load_messages_around_with_meta(thread_id, target_ts, context_before, max_after)?;
        Ok(msgs)
    }

    /// Accent-insensitive full-text search across decrypted in-memory index (`Ctrl+K`),
    /// combining FTS5 prefix search with Hangul/CJK substring and Korean Choseong matching.
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let safe_limit = limit.clamp(1, 200);
        let safe_limit_i64 = safe_limit as i64;
        let clean = strip_vi_accents(query);
        let mut raw_tokens: Vec<String> = Vec::new();
        for w in clean.split(|c: char| c.is_whitespace() || c == '"' || c.is_control()) {
            let trimmed = w.trim_matches(|c: char| !c.is_alphanumeric());
            if trimmed.is_empty() || !trimmed.chars().any(char::is_alphanumeric) {
                continue;
            }
            if !raw_tokens.iter().any(|existing| existing == trimmed) {
                raw_tokens.push(trimmed.to_string());
                if raw_tokens.len() >= 32 {
                    break;
                }
            }
        }
        if raw_tokens.is_empty() {
            return Ok(Vec::new());
        }

        let is_cjk_or_hangul = contains_cjk_or_hangul(&clean);
        let has_choseong_token = raw_tokens.iter().any(|t| is_korean_choseong_query(t));

        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut ranked: Vec<(i64, String, String)> = Vec::new();

        // 1. Fast FTS5 prefix query for Latin/Vietnamese and word-initial hits.
        let fts_tokens: Vec<String> = raw_tokens.iter().map(|t| format!("\"{t}\"*")).collect();
        let fts_query = fts_tokens.join(" ");
        {
            let mut stmt = self.fts_conn.prepare(
                "SELECT thread_id, msg_id, CAST(ts AS INTEGER)
                 FROM messages_fts
                 WHERE messages_fts MATCH ?1
                 ORDER BY CAST(ts AS INTEGER) DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![fts_query, safe_limit_i64], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (tid, mid, ts) in rows {
                if seen.insert((tid.clone(), mid.clone())) {
                    ranked.push((ts, tid, mid));
                }
            }
        }

        // 2. Substring + Korean Choseong scan across in-memory `messages_fts` when query
        //    contains Hangul/CJK, contains Korean Choseong tokens, or FTS5 returned < limit.
        if is_cjk_or_hangul || has_choseong_token || ranked.len() < safe_limit {
            if !has_choseong_token {
                let mut clauses = Vec::with_capacity(raw_tokens.len());
                for idx in 1..=raw_tokens.len() {
                    clauses.push(format!(
                        "(INSTR(text_norm, ?{idx}) > 0 OR INSTR(sender_name, ?{idx}) > 0)"
                    ));
                }
                let limit_param = raw_tokens.len() + 1;
                let sql = format!(
                    "SELECT thread_id, msg_id, CAST(ts AS INTEGER)
                     FROM messages_fts
                     WHERE {}
                     ORDER BY CAST(ts AS INTEGER) DESC LIMIT ?{limit_param}",
                    clauses.join(" AND ")
                );
                let mut stmt = self.fts_conn.prepare(&sql)?;
                let mut sql_params: Vec<rusqlite::types::Value> = raw_tokens
                    .iter()
                    .map(|t| rusqlite::types::Value::Text(t.clone()))
                    .collect();
                sql_params.push(rusqlite::types::Value::Integer(safe_limit_i64));
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(sql_params), |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (tid, mid, ts) in rows {
                    if seen.insert((tid.clone(), mid.clone())) {
                        ranked.push((ts, tid, mid));
                    }
                }
            } else {
                let (choseong_tokens, plain_tokens): (Vec<&String>, Vec<&String>) = raw_tokens
                    .iter()
                    .partition(|t| is_korean_choseong_query(t));
                let mut sql_params: Vec<rusqlite::types::Value> =
                    Vec::with_capacity(plain_tokens.len());
                let where_clause = if plain_tokens.is_empty() {
                    String::new()
                } else {
                    let mut clauses = Vec::with_capacity(plain_tokens.len());
                    for (i, tok) in plain_tokens.iter().enumerate() {
                        let idx = i + 1;
                        clauses.push(format!(
                            "(INSTR(text_norm, ?{idx}) > 0 OR INSTR(sender_name, ?{idx}) > 0)"
                        ));
                        sql_params.push(rusqlite::types::Value::Text((*tok).clone()));
                    }
                    format!("WHERE {}", clauses.join(" AND "))
                };
                let sql = format!(
                    "SELECT thread_id, msg_id, CAST(ts AS INTEGER), sender_name, text_norm
                     FROM messages_fts
                     {where_clause}
                     ORDER BY CAST(ts AS INTEGER) DESC"
                );
                let mut stmt = self.fts_conn.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params_from_iter(sql_params))?;
                let mut matched_scan = 0usize;
                while let Some(r) = rows.next()? {
                    let tid: String = r.get(0)?;
                    let mid: String = r.get(1)?;
                    let ts: i64 = r.get(2)?;
                    let sender_norm: String = r.get(3)?;
                    let text_norm: String = r.get(4)?;
                    let sender_cho = extract_korean_choseong(&sender_norm);
                    let text_cho = extract_korean_choseong(&text_norm);
                    let all_match = choseong_tokens.iter().all(|tok| {
                        text_norm.contains(tok.as_str())
                            || sender_norm.contains(tok.as_str())
                            || text_cho.contains(tok.as_str())
                            || sender_cho.contains(tok.as_str())
                    });
                    if all_match {
                        if seen.insert((tid.clone(), mid.clone())) {
                            ranked.push((ts, tid, mid));
                        }
                        matched_scan += 1;
                        if matched_scan >= safe_limit {
                            break;
                        }
                    }
                }
            }
        }

        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
        ranked.truncate(safe_limit);

        let mut results = Vec::with_capacity(ranked.len());
        for (_ts, tid, mid) in ranked {
            if let Some(msg) = self.get_message(&tid, &mid)? {
                results.push(msg);
            }
        }
        Ok(results)
    }

    fn decode_media_object_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaObjectRecord> {
        Ok(MediaObjectRecord {
            sha256: row.get(0)?,
            kind: row.get(1)?,
            mime: row.get(2)?,
            size_bytes: row.get::<_, i64>(3)?.max(0) as u64,
            thumb_sha256: row.get(4)?,
            width: row
                .get::<_, Option<i64>>(5)?
                .and_then(|v| u32::try_from(v).ok()),
            height: row
                .get::<_, Option<i64>>(6)?
                .and_then(|v| u32::try_from(v).ok()),
            duration_ms: row.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u64),
            local_state: row.get(8)?,
            local_rel_path: row.get(9)?,
            is_encrypted: row.get::<_, i64>(10)? != 0,
            pin_count: row.get::<_, i64>(11)?.max(0) as u32,
            created_ts: row.get(12)?,
            last_accessed_ts: row.get(13)?,
            cloud_provider: row.get(14)?,
            cloud_file_id: row.get(15)?,
            cloud_encrypted: row.get::<_, i64>(16)? != 0,
            cloud_key_ver: row.get(17)?,
            cloud_synced_ts: row.get(18)?,
        })
    }

    pub fn upsert_media_object(&self, obj: &MediaObjectRecord) -> Result<()> {
        let size_i64 = i64::try_from(obj.size_bytes).unwrap_or(i64::MAX);
        let width_i64 = obj.width.map(i64::from);
        let height_i64 = obj.height.map(i64::from);
        let duration_i64 = obj
            .duration_ms
            .map(|v| i64::try_from(v).unwrap_or(i64::MAX));
        self.conn.execute(
            r#"
            INSERT INTO media_objects (
                sha256, kind, mime, size_bytes, thumb_sha256, width, height, duration_ms,
                local_state, local_rel_path, is_encrypted, pin_count, created_ts, last_accessed_ts,
                cloud_provider, cloud_file_id, cloud_encrypted, cloud_key_ver, cloud_synced_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(sha256) DO UPDATE SET
                kind = CASE WHEN excluded.kind != '' THEN excluded.kind ELSE media_objects.kind END,
                mime = CASE
                    WHEN excluded.mime != '' AND excluded.mime != 'application/octet-stream' THEN excluded.mime
                    WHEN media_objects.mime != '' THEN media_objects.mime
                    ELSE excluded.mime
                END,
                size_bytes = CASE WHEN excluded.size_bytes > 0 THEN excluded.size_bytes ELSE media_objects.size_bytes END,
                thumb_sha256 = COALESCE(excluded.thumb_sha256, media_objects.thumb_sha256),
                width = COALESCE(excluded.width, media_objects.width),
                height = COALESCE(excluded.height, media_objects.height),
                duration_ms = COALESCE(excluded.duration_ms, media_objects.duration_ms),
                local_state = CASE
                    WHEN excluded.local_state = 1 THEN 1
                    ELSE media_objects.local_state
                END,
                local_rel_path = COALESCE(excluded.local_rel_path, media_objects.local_rel_path),
                is_encrypted = CASE
                    WHEN excluded.local_state = 1 THEN excluded.is_encrypted
                    ELSE media_objects.is_encrypted
                END,
                pin_count = MAX(media_objects.pin_count, excluded.pin_count),
                last_accessed_ts = MAX(media_objects.last_accessed_ts, excluded.last_accessed_ts),
                cloud_provider = COALESCE(excluded.cloud_provider, media_objects.cloud_provider),
                cloud_file_id = COALESCE(excluded.cloud_file_id, media_objects.cloud_file_id),
                cloud_encrypted = excluded.cloud_encrypted,
                cloud_key_ver = MAX(media_objects.cloud_key_ver, excluded.cloud_key_ver),
                cloud_synced_ts = COALESCE(excluded.cloud_synced_ts, media_objects.cloud_synced_ts),
                cloud_upload_fail_count = CASE
                    WHEN excluded.local_state = 1 THEN 0
                    ELSE media_objects.cloud_upload_fail_count
                END,
                cloud_upload_next_retry_ts = CASE
                    WHEN excluded.local_state = 1 THEN 0
                    ELSE media_objects.cloud_upload_next_retry_ts
                END
            "#,
            params![
                obj.sha256,
                obj.kind,
                obj.mime,
                size_i64,
                obj.thumb_sha256.as_deref(),
                width_i64,
                height_i64,
                duration_i64,
                obj.local_state,
                obj.local_rel_path.as_deref(),
                obj.is_encrypted as i64,
                i64::from(obj.pin_count),
                obj.created_ts,
                obj.last_accessed_ts,
                obj.cloud_provider.as_deref(),
                obj.cloud_file_id.as_deref(),
                obj.cloud_encrypted as i64,
                obj.cloud_key_ver,
                obj.cloud_synced_ts,
            ],
        )?;
        Ok(())
    }

    pub fn get_media_object(&self, sha256: &str) -> Result<Option<MediaObjectRecord>> {
        let obj = self
            .conn
            .query_row(
                "SELECT sha256, kind, mime, size_bytes, thumb_sha256, width, height, duration_ms,
                        local_state, local_rel_path, is_encrypted, pin_count, created_ts, last_accessed_ts,
                        cloud_provider, cloud_file_id, cloud_encrypted, cloud_key_ver, cloud_synced_ts
                 FROM media_objects
                 WHERE sha256 = ?1
                 LIMIT 1",
                params![sha256],
                Self::decode_media_object_row,
            )
            .optional()?;
        Ok(obj)
    }

    pub fn link_message_media(&self, rec: &MessageMediaRecord) -> Result<()> {
        let variant = if rec.variant.trim().is_empty() {
            "primary"
        } else {
            rec.variant.trim()
        };
        let aad = message_media_aad(&rec.thread_id, &rec.msg_id, variant);
        let enc_orig_name = self.cipher.encrypt_opt(&aad, rec.original_name.as_deref())?;
        let enc_saved_path = self
            .cipher
            .encrypt_opt(&aad, rec.saved_user_path.as_deref())?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            r#"
            INSERT INTO message_media (
                thread_id, msg_id, variant, url_hash, sha256, original_name,
                saved_user_path, cdn_status, updated_ts
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(thread_id, msg_id, variant) DO UPDATE SET
                url_hash = CASE WHEN excluded.url_hash != '' THEN excluded.url_hash ELSE message_media.url_hash END,
                sha256 = COALESCE(excluded.sha256, message_media.sha256),
                original_name = COALESCE(excluded.original_name, message_media.original_name),
                saved_user_path = COALESCE(excluded.saved_user_path, message_media.saved_user_path),
                cdn_status = CASE WHEN excluded.cdn_status != 0 THEN excluded.cdn_status ELSE message_media.cdn_status END,
                updated_ts = MAX(message_media.updated_ts, excluded.updated_ts)
            "#,
            params![
                rec.thread_id,
                rec.msg_id,
                variant,
                rec.url_hash,
                rec.sha256.as_deref(),
                enc_orig_name,
                enc_saved_path,
                rec.cdn_status,
                rec.updated_ts,
            ],
        )?;
        if let Some(sha) = rec.sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            tx.execute(
                "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
                params![sha],
            )?;
        }
        if rec.thread_id != "__url__" && !rec.url_hash.is_empty() {
            tx.execute(
                "DELETE FROM message_media WHERE thread_id = '__url__' AND url_hash = ?1 AND variant = ?2",
                params![rec.url_hash, variant],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_message_media_record(
        &self,
        thread_id: &str,
        msg_id: &str,
        variant: &str,
    ) -> Result<Option<MessageMediaRecord>> {
        let variant = if variant.trim().is_empty() {
            "primary"
        } else {
            variant.trim()
        };
        struct RawMessageMediaRow {
            url_hash: String,
            sha256: Option<String>,
            enc_name: Option<String>,
            enc_saved: Option<String>,
            cdn_status: i32,
            updated_ts: i64,
        }
        let row: Option<RawMessageMediaRow> = self
            .conn
            .query_row(
                "SELECT url_hash, sha256, original_name, saved_user_path, cdn_status, updated_ts
                 FROM message_media
                 WHERE thread_id = ?1 AND msg_id = ?2 AND variant = ?3
                 LIMIT 1",
                params![thread_id, msg_id, variant],
                |r| {
                    Ok(RawMessageMediaRow {
                        url_hash: r.get(0)?,
                        sha256: r.get(1)?,
                        enc_name: r.get(2)?,
                        enc_saved: r.get(3)?,
                        cdn_status: r.get(4)?,
                        updated_ts: r.get(5)?,
                    })
                },
            )
            .optional()?;
        let Some(RawMessageMediaRow {
            url_hash,
            sha256,
            enc_name,
            enc_saved,
            cdn_status,
            updated_ts,
        }) = row
        else {
            return Ok(None);
        };
        let aad = message_media_aad(thread_id, msg_id, variant);
        let original_name = self.cipher.decrypt_opt(&aad, enc_name)?;
        let saved_user_path = self.cipher.decrypt_opt(&aad, enc_saved)?;
        Ok(Some(MessageMediaRecord {
            thread_id: thread_id.to_string(),
            msg_id: msg_id.to_string(),
            variant: variant.to_string(),
            url_hash,
            sha256,
            original_name,
            saved_user_path,
            cdn_status,
            updated_ts,
        }))
    }

    pub fn lookup_media_by_url_hash(&self, url_hash: &str) -> Result<Option<MediaObjectRecord>> {
        if url_hash.trim().is_empty() {
            return Ok(None);
        }
        let obj = self
            .conn
            .query_row(
                "SELECT o.sha256, o.kind, o.mime, o.size_bytes, o.thumb_sha256, o.width, o.height, o.duration_ms,
                        o.local_state, o.local_rel_path, o.is_encrypted, o.pin_count, o.created_ts, o.last_accessed_ts,
                        o.cloud_provider, o.cloud_file_id, o.cloud_encrypted, o.cloud_key_ver, o.cloud_synced_ts
                 FROM message_media mm
                 INNER JOIN media_objects o ON o.sha256 = mm.sha256
                 WHERE mm.url_hash = ?1 AND mm.sha256 IS NOT NULL
                 ORDER BY o.local_state DESC, mm.updated_ts DESC
                 LIMIT 1",
                params![url_hash],
                Self::decode_media_object_row,
            )
            .optional()?;
        Ok(obj)
    }

    /// Looks up any non-empty decrypted `original_name` in `message_media` matching `url_hash`,
    /// preferring `'primary'` rows and most recently updated records.
    pub fn find_media_original_name_by_hash(&self, url_hash: &str) -> Result<Option<String>> {
        let trimmed_hash = url_hash.trim();
        if trimmed_hash.is_empty() {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT thread_id, msg_id, variant, original_name
             FROM message_media
             WHERE url_hash = ?1
               AND original_name IS NOT NULL
               AND original_name != ''
             ORDER BY CASE WHEN variant = 'primary' THEN 0 ELSE 1 END ASC, updated_ts DESC",
        )?;
        let rows = stmt.query_map(params![trimmed_hash], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (thread_id, msg_id, variant, enc_name) = row?;
            let aad = message_media_aad(&thread_id, &msg_id, &variant);
            if let Ok(plain) = self.cipher.decrypt_or_legacy(&aad, &enc_name) {
                let name = plain.trim();
                if !name.is_empty() {
                    return Ok(Some(name.to_string()));
                }
            }
        }
        Ok(None)
    }

    pub fn lookup_media_by_msg(
        &self,
        thread_id: &str,
        msg_id: &str,
        variant: &str,
    ) -> Result<Option<MediaObjectRecord>> {
        let variant = if variant.trim().is_empty() {
            "primary"
        } else {
            variant.trim()
        };
        let obj = self
            .conn
            .query_row(
                "SELECT o.sha256, o.kind, o.mime, o.size_bytes, o.thumb_sha256, o.width, o.height, o.duration_ms,
                        o.local_state, o.local_rel_path, o.is_encrypted, o.pin_count, o.created_ts, o.last_accessed_ts,
                        o.cloud_provider, o.cloud_file_id, o.cloud_encrypted, o.cloud_key_ver, o.cloud_synced_ts
                 FROM message_media mm
                 INNER JOIN media_objects o ON o.sha256 = mm.sha256
                 WHERE mm.thread_id = ?1 AND mm.msg_id = ?2 AND mm.variant = ?3 AND mm.sha256 IS NOT NULL
                 LIMIT 1",
                params![thread_id, msg_id, variant],
                Self::decode_media_object_row,
            )
            .optional()?;
        Ok(obj)
    }

    pub fn batch_touch_media_access(&self, touches: &[(String, i64)]) -> Result<usize> {
        if touches.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut affected = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE media_objects
                 SET last_accessed_ts = MAX(last_accessed_ts, ?2)
                 WHERE sha256 = ?1",
            )?;
            for (sha256, ts) in touches {
                affected += stmt.execute(params![sha256, ts])?;
            }
        }
        tx.commit()?;
        Ok(affected)
    }

    pub fn select_lru_eviction_candidates(&self, limit: usize) -> Result<Vec<MediaObjectRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT sha256, kind, mime, size_bytes, thumb_sha256, width, height, duration_ms,
                    local_state, local_rel_path, is_encrypted, pin_count, created_ts, last_accessed_ts,
                    cloud_provider, cloud_file_id, cloud_encrypted, cloud_key_ver, cloud_synced_ts
             FROM media_objects
             WHERE local_state = 1 AND pin_count = 0 AND LOWER(kind) != 'thumb'
             ORDER BY last_accessed_ts ASC, created_ts ASC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], Self::decode_media_object_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn mark_media_evicted(&self, sha256: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "UPDATE media_objects
             SET local_state = ?2, local_rel_path = NULL
             WHERE sha256 = ?1",
            params![sha256, MEDIA_LOCAL_STATE_MISSING],
        )?;
        Ok(affected > 0)
    }

    pub fn mark_media_cdn_status(&self, url_hash: &str, status: i32) -> Result<usize> {
        let affected = self.conn.execute(
            "UPDATE message_media
             SET cdn_status = ?2
             WHERE url_hash = ?1",
            params![url_hash, status],
        )?;
        Ok(affected)
    }

    pub fn set_media_pin(&self, sha256: &str, pinned: bool) -> Result<bool> {
        let affected = if pinned {
            self.conn.execute(
                "UPDATE media_objects
                 SET pin_count = CASE WHEN pin_count < 1 THEN 1 ELSE pin_count END
                 WHERE sha256 = ?1",
                params![sha256],
            )?
        } else {
            self.conn.execute(
                "UPDATE media_objects
                 SET pin_count = 0
                 WHERE sha256 = ?1",
                params![sha256],
            )?
        };
        Ok(affected > 0)
    }

    pub fn set_message_saved_path(&self, thread_id: &str, msg_id: &str, path: &str) -> Result<()> {
        let variant = "primary";
        let aad = message_media_aad(thread_id, msg_id, variant);
        let enc_path = self.cipher.encrypt_str(&aad, path)?;
        self.conn.execute(
            "INSERT INTO message_media (
                thread_id, msg_id, variant, url_hash, saved_user_path, updated_ts
             )
             VALUES (?1, ?2, ?3, '', ?4, 0)
             ON CONFLICT(thread_id, msg_id, variant) DO UPDATE SET
                 saved_user_path = excluded.saved_user_path",
            params![thread_id, msg_id, variant, enc_path],
        )?;
        Ok(())
    }

    pub fn get_message_saved_path(
        &self,
        thread_id: &str,
        msg_id: &str,
    ) -> Result<Option<String>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT variant, saved_user_path
                 FROM message_media
                 WHERE thread_id = ?1 AND msg_id = ?2 AND saved_user_path IS NOT NULL
                 ORDER BY CASE WHEN variant = 'primary' THEN 0 ELSE 1 END, updated_ts DESC
                 LIMIT 1",
                params![thread_id, msg_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((variant, enc_path)) = row else {
            return Ok(None);
        };
        let aad = message_media_aad(thread_id, msg_id, &variant);
        let plain = self.cipher.decrypt_str(&aad, &enc_path)?;
        Ok(Some(plain))
    }

    pub fn compute_media_stats(&self) -> Result<MediaVaultStatsDto> {
        let stats = self.conn.query_row(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN local_state = 1 THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND pin_count > 0 THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND pin_count > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND LOWER(kind) IN ('image', 'photo', 'gif', 'sticker') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND LOWER(kind) = 'video' THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND LOWER(kind) IN ('voice', 'audio') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND LOWER(kind) NOT IN ('image', 'photo', 'gif', 'sticker', 'video', 'voice', 'audio', 'thumb') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND LOWER(kind) = 'thumb' THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' THEN size_bytes ELSE 0 END), 0)
            FROM media_objects
            "#,
            [],
            |r| {
                Ok(MediaVaultStatsDto {
                    total_bytes: r.get::<_, i64>(0)?.max(0) as u64,
                    object_count: r.get::<_, i64>(1)?.max(0) as u64,
                    pinned_bytes: r.get::<_, i64>(2)?.max(0) as u64,
                    pinned_count: r.get::<_, i64>(3)?.max(0) as u64,
                    image_bytes: r.get::<_, i64>(4)?.max(0) as u64,
                    video_bytes: r.get::<_, i64>(5)?.max(0) as u64,
                    voice_bytes: r.get::<_, i64>(6)?.max(0) as u64,
                    file_bytes: r.get::<_, i64>(7)?.max(0) as u64,
                    thumb_bytes: r.get::<_, i64>(8)?.max(0) as u64,
                    cloud_synced_count: r.get::<_, i64>(9)?.max(0) as u64,
                    cloud_total_bytes: r.get::<_, i64>(10)?.max(0) as u64,
                })
            },
        )?;
        Ok(stats)
    }

    pub fn list_unpinned_local_blobs(&self, include_pinned: bool) -> Result<Vec<(String, String)>> {
        let sql = if include_pinned {
            "SELECT sha256, local_rel_path
             FROM media_objects
             WHERE local_state = 1 AND local_rel_path IS NOT NULL
             ORDER BY last_accessed_ts ASC"
        } else {
            "SELECT sha256, local_rel_path
             FROM media_objects
             WHERE local_state = 1 AND pin_count = 0 AND local_rel_path IS NOT NULL
             ORDER BY last_accessed_ts ASC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Removes `message_media` rows for `(thread_id, msg_id)` and deletes/evicts any now-unreferenced
    /// `media_objects` (plus their orphaned thumbnails), queueing unreferenced cloud blobs for Drive
    /// pruning and returning `local_rel_path`s to unlink outside DB lock.
    pub fn cleanup_orphaned_media_for_msg(
        &self,
        thread_id: &str,
        msg_id: &str,
    ) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
        let orphaned_paths = Self::cleanup_orphaned_media_for_msg_in_tx(
            &tx,
            thread_id,
            msg_id,
            Some("msg_media_removed"),
        )?;
        tx.commit()?;
        Ok(orphaned_paths)
    }

    pub fn list_pending_cloud_deletes(&self, limit: usize) -> Result<Vec<PendingCloudDeleteRecord>> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.list_ready_pending_cloud_deletes(limit, false, 0, now_ms)
    }

    pub fn list_ready_pending_cloud_deletes(
        &self,
        limit: usize,
        require_history_pull: bool,
        min_grace_ms: i64,
        now_ms: i64,
    ) -> Result<Vec<PendingCloudDeleteRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Automatically cancel any queued delete whose blob was re-referenced by a live message or thumb.
        self.conn.execute(
            "DELETE FROM gdrive_pending_cloud_deletes
             WHERE EXISTS (SELECT 1 FROM message_media WHERE sha256 = gdrive_pending_cloud_deletes.sha256)
                OR EXISTS (SELECT 1 FROM media_objects WHERE thumb_sha256 = gdrive_pending_cloud_deletes.sha256)",
            [],
        )?;
        let current_pull_seq = Self::get_gdrive_history_pull_seq_on_conn(&self.conn)?;
        if require_history_pull && current_pull_seq <= 0 {
            return Ok(Vec::new());
        }
        let max_queued_ts = if min_grace_ms > 0 {
            now_ms.saturating_sub(min_grace_ms)
        } else {
            i64::MAX
        };
        let mut stmt = if require_history_pull {
            self.conn.prepare(
                "SELECT sha256, cloud_file_id, reason, queued_ts
                 FROM gdrive_pending_cloud_deletes
                 WHERE next_retry_ts <= ?1
                   AND queued_pull_seq < ?2
                   AND queued_ts <= ?3
                 ORDER BY next_retry_ts ASC, queued_ts ASC, sha256 ASC
                 LIMIT ?4",
            )?
        } else {
            self.conn.prepare(
                "SELECT sha256, cloud_file_id, reason, queued_ts
                 FROM gdrive_pending_cloud_deletes
                 WHERE next_retry_ts <= ?1
                   AND queued_ts <= ?3
                 ORDER BY next_retry_ts ASC, queued_ts ASC, sha256 ASC
                 LIMIT ?4",
            )?
        };
        let rows = stmt
            .query_map(
                params![now_ms, current_pull_seq, max_queued_ts, limit as i64],
                |r| {
                    Ok(PendingCloudDeleteRecord {
                        sha256: r.get(0)?,
                        cloud_file_id: r.get(1)?,
                        reason: r.get(2)?,
                        queued_ts: r.get(3)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn record_gdrive_history_pull_completed(&self, now_ms: i64) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let next_seq = Self::get_gdrive_history_pull_seq_on_conn(&tx)?.saturating_add(1);
        tx.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_history_pull_seq', ?1)",
            params![next_seq.to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_history_pull_ts', ?1)",
            params![now_ms.max(1).to_string()],
        )?;
        tx.commit()?;
        Ok(next_seq)
    }

    pub fn get_gdrive_history_pull_seq(&self) -> Result<i64> {
        Self::get_gdrive_history_pull_seq_on_conn(&self.conn)
    }

    /// Records a failed cloud delete attempt with exponential backoff so failing/poison rows yield
    /// the queue head immediately. Permanent client errors (HTTP 400/403) are dropped after 3 attempts
    /// and return `Ok(true)`.
    pub fn record_pending_cloud_delete_failure(
        &self,
        sha256: &str,
        cloud_file_id: &str,
        permanent: bool,
        now_ms: i64,
    ) -> Result<bool> {
        const PERMANENT_MAX_ATTEMPTS: i64 = 3;
        const BASE_BACKOFF_MS: i64 = 60_000;
        const MAX_BACKOFF_MS: i64 = 3_600_000;

        let cur_retries: Option<i64> = self
            .conn
            .query_row(
                "SELECT retry_count
                 FROM gdrive_pending_cloud_deletes
                 WHERE sha256 = ?1 AND cloud_file_id = ?2
                 LIMIT 1",
                params![sha256, cloud_file_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(prev) = cur_retries else {
            return Ok(false);
        };
        let next_retries = prev.saturating_add(1);
        if permanent && next_retries >= PERMANENT_MAX_ATTEMPTS {
            self.complete_pending_cloud_delete_for_file(sha256, cloud_file_id)?;
            return Ok(true);
        }
        let shift = (next_retries - 1).clamp(0, 6) as u32;
        let backoff_ms = BASE_BACKOFF_MS
            .saturating_mul(1i64 << shift)
            .min(MAX_BACKOFF_MS);
        let next_retry_ts = now_ms.saturating_add(backoff_ms);
        self.conn.execute(
            "UPDATE gdrive_pending_cloud_deletes
             SET retry_count = ?3, next_retry_ts = ?4
             WHERE sha256 = ?1 AND cloud_file_id = ?2",
            params![sha256, cloud_file_id, next_retries, next_retry_ts],
        )?;
        Ok(false)
    }

    pub fn is_media_unreferenced(&self, sha256: &str) -> Result<bool> {
        let referenced: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_media WHERE sha256 = ?1)
                 OR EXISTS(SELECT 1 FROM media_objects WHERE thumb_sha256 = ?1)",
            params![sha256],
            |r| r.get(0),
        )?;
        Ok(!referenced)
    }

    pub fn cancel_pending_cloud_delete(&self, sha256: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
            params![sha256],
        )?;
        Ok(())
    }

    pub fn complete_pending_cloud_delete(&self, sha256: &str) -> Result<()> {
        let queued_cid: Option<String> = self
            .conn
            .query_row(
                "SELECT cloud_file_id FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1 LIMIT 1",
                params![sha256],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(cid) = queued_cid {
            self.complete_pending_cloud_delete_for_file(sha256, &cid)?;
        }
        Ok(())
    }

    pub fn complete_pending_cloud_delete_for_file(
        &self,
        sha256: &str,
        cloud_file_id: &str,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1 AND cloud_file_id = ?2",
            params![sha256, cloud_file_id],
        )?;
        let still_referenced: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_media WHERE sha256 = ?1)
                 OR EXISTS(SELECT 1 FROM media_objects WHERE thumb_sha256 = ?1)",
            params![sha256],
            |r| r.get(0),
        )?;
        let row: Option<(i32, Option<String>)> = tx
            .query_row(
                "SELECT local_state, cloud_file_id FROM media_objects WHERE sha256 = ?1 LIMIT 1",
                params![sha256],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if !still_referenced
            && row.as_ref().is_some_and(|(ls, cid)| {
                *ls == MEDIA_LOCAL_STATE_MISSING && cid.as_deref() == Some(cloud_file_id)
            })
        {
            tx.execute(
                "DELETE FROM media_objects WHERE sha256 = ?1 AND cloud_file_id = ?2",
                params![sha256, cloud_file_id],
            )?;
        } else {
            tx.execute(
                "UPDATE media_objects
                 SET cloud_provider = NULL,
                     cloud_file_id = NULL,
                     cloud_synced_ts = NULL
                 WHERE sha256 = ?1 AND cloud_file_id = ?2",
                params![sha256, cloud_file_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_pending_cloud_uploads(&self, limit: usize) -> Result<Vec<MediaObjectRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut stmt = self.conn.prepare(
            "SELECT sha256, kind, mime, size_bytes, thumb_sha256, width, height, duration_ms,
                    local_state, local_rel_path, is_encrypted, pin_count, created_ts, last_accessed_ts,
                    cloud_provider, cloud_file_id, cloud_encrypted, cloud_key_ver, cloud_synced_ts
             FROM media_objects
             WHERE local_state = 1
               AND (cloud_file_id IS NULL OR cloud_file_id = '')
               AND cloud_excluded = 0
               AND LOWER(kind) != 'thumb'
               AND cloud_upload_next_retry_ts <= ?1
             ORDER BY cloud_upload_fail_count ASC, cloud_upload_next_retry_ts ASC, created_ts ASC, size_bytes ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![now_ms, limit as i64], Self::decode_media_object_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Marks a pending local media blob upload failure with exponential backoff (5m -> 24h)
    /// so a single unreadable/corrupted blob never starves newer pending uploads.
    pub fn record_media_cloud_upload_failure(&self, sha256: &str, now_ms: i64) -> Result<i64> {
        const BASE_UPLOAD_BACKOFF_MS: i64 = 5 * 60 * 1000;
        const MAX_UPLOAD_BACKOFF_MS: i64 = 24 * 60 * 60 * 1000;

        let prev: i64 = self
            .conn
            .query_row(
                "SELECT cloud_upload_fail_count FROM media_objects WHERE sha256 = ?1 LIMIT 1",
                params![sha256],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let next_fail = prev.saturating_add(1);
        let shift = (next_fail - 1).clamp(0, 8) as u32;
        let backoff = BASE_UPLOAD_BACKOFF_MS
            .saturating_mul(1i64 << shift)
            .min(MAX_UPLOAD_BACKOFF_MS);
        let next_retry_ts = now_ms.saturating_add(backoff);
        self.conn.execute(
            "UPDATE media_objects
             SET cloud_upload_fail_count = ?2,
                 cloud_upload_next_retry_ts = ?3
             WHERE sha256 = ?1",
            params![sha256, next_fail, next_retry_ts],
        )?;
        Ok(next_retry_ts)
    }

    pub fn clear_media_cloud_upload_failure(&self, sha256: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE media_objects
             SET cloud_upload_fail_count = 0,
                 cloud_upload_next_retry_ts = 0
             WHERE sha256 = ?1",
            params![sha256],
        )?;
        Ok(())
    }

    pub fn mark_media_cloud_synced(
        &self,
        sha256: &str,
        cloud_provider: &str,
        cloud_file_id: &str,
        key_ver: i64,
        synced_ts: i64,
    ) -> Result<()> {
        let safe_ver = key_ver.clamp(1, i64::from(i32::MAX));
        let tx = self.conn.unchecked_transaction()?;
        let mut_ts = Self::next_local_mutation_ts_on_conn(&tx)?;
        let effective_synced_ts = mut_ts.max(synced_ts);
        if effective_synced_ts > mut_ts {
            Self::advance_local_mutation_seq_on_conn(&tx, effective_synced_ts)?;
        }
        tx.execute(
            "UPDATE media_objects
             SET cloud_provider = ?2,
                 cloud_file_id = ?3,
                 cloud_encrypted = 1,
                 cloud_key_ver = ?4,
                 cloud_synced_ts = ?5,
                 cloud_excluded = 0,
                 cloud_upload_fail_count = 0,
                 cloud_upload_next_retry_ts = 0
             WHERE sha256 = ?1",
            params![sha256, cloud_provider, cloud_file_id, safe_ver, effective_synced_ts],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_media_cloud_links(&self, sha256s: &[String]) -> Result<usize> {
        if sha256s.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut affected = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE media_objects
                 SET cloud_provider = NULL,
                     cloud_file_id = NULL,
                     cloud_synced_ts = NULL,
                     cloud_upload_fail_count = 0,
                     cloud_upload_next_retry_ts = 0
                 WHERE sha256 = ?1",
            )?;
            for sha in sha256s {
                affected += stmt.execute(params![sha])?;
            }
        }
        tx.commit()?;
        Ok(affected)
    }

    /// Resets stale cloud file links and/or delta sync cursors when remote Google Drive vault
    /// folders (`Teza Vault/<uid>/{blobs,history}`) were newly recreated after deletion/trashing.
    pub fn reset_gdrive_sync_state_for_vault_recovery(
        &self,
        reset_media_links: bool,
        reset_history_cursors: bool,
    ) -> Result<()> {
        if !reset_media_links && !reset_history_cursors {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        if reset_media_links {
            tx.execute(
                "UPDATE media_objects
                 SET cloud_provider = NULL,
                     cloud_file_id = NULL,
                     cloud_synced_ts = NULL,
                     cloud_upload_fail_count = 0,
                     cloud_upload_next_retry_ts = 0
                 WHERE cloud_file_id IS NOT NULL AND cloud_file_id != ''",
                [],
            )?;
            tx.execute("DELETE FROM gdrive_pending_cloud_deletes", [])?;
            tx.execute(
                "DELETE FROM media_objects
                 WHERE local_state = 0
                   AND (cloud_file_id IS NULL OR cloud_file_id = '')
                   AND NOT EXISTS(SELECT 1 FROM message_media WHERE sha256 = media_objects.sha256)
                   AND NOT EXISTS(SELECT 1 FROM media_objects m2 WHERE m2.thumb_sha256 = media_objects.sha256)",
                [],
            )?;
        }
        if reset_history_cursors {
            tx.execute(
                "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_history_sync_ts', '0')",
                [],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_mutation_sync_ts', '0')",
                [],
            )?;
            tx.execute("DELETE FROM gdrive_applied_deltas", [])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Clears Google Drive cloud links after user-initiated Cloud Storage Manager deletion and
    /// marks `cloud_excluded = 1` so auto-sync will not immediately re-upload the preserved local blob.
    pub fn mark_media_cloud_pruned(&self, sha256s: &[String]) -> Result<usize> {
        if sha256s.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut affected = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE media_objects
                 SET cloud_provider = NULL,
                     cloud_file_id = NULL,
                     cloud_synced_ts = NULL,
                     cloud_excluded = 1
                 WHERE sha256 = ?1",
            )?;
            let mut del_q = tx.prepare(
                "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
            )?;
            let mut del_orphan = tx.prepare(
                "DELETE FROM media_objects
                 WHERE sha256 = ?1
                   AND local_state = 0
                   AND NOT EXISTS(SELECT 1 FROM message_media WHERE sha256 = ?1)
                   AND NOT EXISTS(SELECT 1 FROM media_objects WHERE thumb_sha256 = ?1)",
            )?;
            for sha in sha256s {
                affected += stmt.execute(params![sha])?;
                let _ = del_q.execute(params![sha])?;
                let _ = del_orphan.execute(params![sha])?;
            }
        }
        tx.commit()?;
        Ok(affected)
    }

    pub fn compute_cloud_media_breakdown(&self) -> Result<CloudMediaBreakdown> {
        let breakdown = self.conn.query_row(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' AND LOWER(kind) IN ('image', 'photo', 'gif', 'sticker') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' AND LOWER(kind) = 'video' THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' AND LOWER(kind) IN ('voice', 'audio') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cloud_file_id IS NOT NULL AND cloud_file_id != '' AND LOWER(kind) NOT IN ('image', 'photo', 'gif', 'sticker', 'video', 'voice', 'audio') THEN size_bytes ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND (cloud_file_id IS NULL OR cloud_file_id = '') AND cloud_excluded = 0 AND LOWER(kind) != 'thumb' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN local_state = 1 AND (cloud_file_id IS NULL OR cloud_file_id = '') AND cloud_excluded = 0 AND LOWER(kind) != 'thumb' THEN size_bytes ELSE 0 END), 0)
            FROM media_objects
            "#,
            [],
            |r| {
                Ok(CloudMediaBreakdown {
                    total_bytes: r.get::<_, i64>(0)?.max(0) as u64,
                    total_count: r.get::<_, i64>(1)?.max(0) as u64,
                    images_bytes: r.get::<_, i64>(2)?.max(0) as u64,
                    videos_bytes: r.get::<_, i64>(3)?.max(0) as u64,
                    voice_bytes: r.get::<_, i64>(4)?.max(0) as u64,
                    files_bytes: r.get::<_, i64>(5)?.max(0) as u64,
                    pending_count: r.get::<_, i64>(6)?.max(0) as u64,
                    pending_bytes: r.get::<_, i64>(7)?.max(0) as u64,
                })
            },
        )?;
        Ok(breakdown)
    }

    fn decrypt_thread_name_opt(&self, thread_id: &str, enc_title: Option<String>) -> String {
        if let Some(raw) = enc_title {
            if let Ok(plain) = self.cipher.decrypt_or_legacy(&thread_aad(thread_id), &raw) {
                let trimmed = plain.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        thread_id.to_string()
    }

    pub fn list_cloud_usage_by_thread(&self) -> Result<Vec<GDriveThreadUsageDto>> {
        struct RawThreadUsageRow {
            thread_id: String,
            enc_title: Option<String>,
            total_bytes: i64,
            object_count: i64,
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT
                sub.thread_id,
                t.title,
                COALESCE(SUM(sub.size_bytes), 0) AS total_bytes,
                COUNT(*) AS object_count
            FROM (
                SELECT DISTINCT mm.thread_id, mo.sha256, mo.size_bytes
                FROM message_media mm
                INNER JOIN media_objects mo ON mo.sha256 = mm.sha256
                WHERE mo.cloud_file_id IS NOT NULL
                  AND mo.cloud_file_id != ''
                  AND mm.thread_id != ''
                  AND mm.thread_id != '__url__'
            ) sub
            LEFT JOIN threads t ON t.id = sub.thread_id
            GROUP BY sub.thread_id
            ORDER BY total_bytes DESC, object_count DESC, sub.thread_id ASC
            "#,
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RawThreadUsageRow {
                    thread_id: r.get(0)?,
                    enc_title: r.get(1)?,
                    total_bytes: r.get(2)?,
                    object_count: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let thread_name = self.decrypt_thread_name_opt(&row.thread_id, row.enc_title);
            out.push(GDriveThreadUsageDto {
                thread_id: row.thread_id,
                thread_name,
                total_bytes: row.total_bytes.max(0) as u64,
                object_count: row.object_count.max(0) as u64,
            });
        }
        Ok(out)
    }

    pub fn list_largest_cloud_items(&self, limit: usize) -> Result<Vec<GDriveCloudItemDto>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        struct RawCloudObjRow {
            sha256: String,
            cloud_file_id: String,
            kind: String,
            mime: String,
            size_bytes: i64,
            cloud_synced_ts: i64,
            local_state: i64,
        }
        struct RawRefRow {
            thread_id: String,
            msg_id: String,
            variant: String,
            enc_orig_name: Option<String>,
            enc_thread_title: Option<String>,
        }

        let mut stmt = self.conn.prepare(
            "SELECT sha256, cloud_file_id, kind, mime, size_bytes, COALESCE(cloud_synced_ts, 0), local_state
             FROM media_objects
             WHERE cloud_file_id IS NOT NULL AND cloud_file_id != ''
             ORDER BY size_bytes DESC, COALESCE(cloud_synced_ts, 0) DESC, sha256 ASC
             LIMIT ?1",
        )?;
        let objs = stmt
            .query_map(params![limit as i64], |r| {
                Ok(RawCloudObjRow {
                    sha256: r.get(0)?,
                    cloud_file_id: r.get(1)?,
                    kind: r.get(2)?,
                    mime: r.get(3)?,
                    size_bytes: r.get(4)?,
                    cloud_synced_ts: r.get(5)?,
                    local_state: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut ref_stmt = self.conn.prepare(
            "SELECT mm.thread_id, mm.msg_id, mm.variant, mm.original_name, t.title
             FROM message_media mm
             LEFT JOIN threads t ON t.id = mm.thread_id
             WHERE mm.sha256 = ?1
             ORDER BY CASE WHEN mm.thread_id = '__url__' THEN 1 ELSE 0 END,
                      CASE WHEN mm.original_name IS NOT NULL THEN 0 ELSE 1 END,
                      mm.updated_ts DESC
             LIMIT 1",
        )?;

        let mut out = Vec::with_capacity(objs.len());
        for obj in objs {
            let ref_row: Option<RawRefRow> = ref_stmt
                .query_row(params![&obj.sha256], |r| {
                    Ok(RawRefRow {
                        thread_id: r.get(0)?,
                        msg_id: r.get(1)?,
                        variant: r.get(2)?,
                        enc_orig_name: r.get(3)?,
                        enc_thread_title: r.get(4)?,
                    })
                })
                .optional()?;

            let (original_name, thread_id, thread_name, msg_id) = match ref_row {
                Some(r) => {
                    let aad = message_media_aad(&r.thread_id, &r.msg_id, &r.variant);
                    let orig_name = r
                        .enc_orig_name
                        .and_then(|s| self.cipher.decrypt_or_legacy(&aad, &s).ok())
                        .filter(|s| !s.trim().is_empty());
                    if r.thread_id == "__url__" || r.thread_id.is_empty() {
                        (orig_name, None, None, None)
                    } else {
                        let tname =
                            self.decrypt_thread_name_opt(&r.thread_id, r.enc_thread_title);
                        (orig_name, Some(r.thread_id), Some(tname), Some(r.msg_id))
                    }
                }
                None => (None, None, None, None),
            };

            out.push(GDriveCloudItemDto {
                sha256: obj.sha256,
                cloud_file_id: obj.cloud_file_id,
                kind: obj.kind,
                mime: obj.mime,
                size_bytes: obj.size_bytes.max(0) as u64,
                original_name,
                thread_id,
                thread_name,
                msg_id,
                cloud_synced_ts: obj.cloud_synced_ts,
                local_state: obj.local_state,
            });
        }
        Ok(out)
    }

    pub fn list_cloud_items_by_sha256s(
        &self,
        sha256s: &[String],
    ) -> Result<Vec<(String, String, u64)>> {
        if sha256s.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT sha256, cloud_file_id, size_bytes
             FROM media_objects
             WHERE sha256 = ?1 AND cloud_file_id IS NOT NULL AND cloud_file_id != ''
             LIMIT 1",
        )?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for sha in sha256s {
            if !seen.insert(sha.as_str()) {
                continue;
            }
            let row: Option<(String, String, i64)> = stmt
                .query_row(params![sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            if let Some((s, cid, sz)) = row {
                out.push((s, cid, sz.max(0) as u64));
            }
        }
        Ok(out)
    }

    pub fn list_cloud_items_exclusive_to_thread(
        &self,
        thread_id: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT mo.sha256, mo.cloud_file_id, mo.size_bytes
             FROM media_objects mo
             INNER JOIN message_media mm ON mm.sha256 = mo.sha256
             WHERE mm.thread_id = ?1
               AND mo.cloud_file_id IS NOT NULL
               AND mo.cloud_file_id != ''
               AND NOT EXISTS (
                   SELECT 1 FROM message_media mm2
                   WHERE mm2.sha256 = mo.sha256
                     AND mm2.thread_id != ?1
                     AND mm2.thread_id != '__url__'
               )
             ORDER BY mo.size_bytes DESC, mo.sha256 ASC",
        )?;
        let rows = stmt
            .query_map(params![thread_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_cloud_items_older_than(
        &self,
        cutoff_ts_ms: i64,
    ) -> Result<Vec<(String, String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT sha256, cloud_file_id, size_bytes
             FROM media_objects
             WHERE cloud_file_id IS NOT NULL
               AND cloud_file_id != ''
               AND cloud_synced_ts IS NOT NULL
               AND cloud_synced_ts < ?1
             ORDER BY cloud_synced_ts ASC, sha256 ASC",
        )?;
        let rows = stmt
            .query_map(params![cutoff_ts_ms], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn is_delta_applied(&self, delta_file_id: &str) -> Result<bool> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM gdrive_applied_deltas WHERE delta_file_id = ?1)",
            params![delta_file_id],
            |r| r.get(0),
        )?;
        Ok(exists)
    }

    pub fn mark_delta_applied(&self, delta_file_id: &str, applied_ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO gdrive_applied_deltas (delta_file_id, applied_ts)
             VALUES (?1, ?2)",
            params![delta_file_id, applied_ts],
        )?;
        Ok(())
    }

    pub fn get_gdrive_last_sync_ts(&self) -> Result<Option<i64>> {
        let val: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_last_sync_ts' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(val.and_then(|s| s.parse::<i64>().ok()))
    }

    pub fn set_gdrive_last_sync_ts(&self, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_sync_ts', ?1)",
            params![ts.to_string()],
        )?;
        Ok(())
    }

    pub fn get_gdrive_last_history_sync_ts(&self) -> Result<Option<i64>> {
        let val: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_last_history_sync_ts' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(val.and_then(|s| s.parse::<i64>().ok()))
    }

    pub fn set_gdrive_last_history_sync_ts(&self, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_history_sync_ts', ?1)",
            params![ts.to_string()],
        )?;
        Ok(())
    }

    pub fn get_gdrive_last_mutation_sync_ts(&self) -> Result<Option<i64>> {
        let val: Option<String> = self
            .conn
            .query_row(
                "SELECT v FROM store_meta WHERE k = 'gdrive_last_mutation_sync_ts' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(val.and_then(|s| s.parse::<i64>().ok()))
    }

    pub fn set_gdrive_last_mutation_sync_ts(&self, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO store_meta (k, v) VALUES ('gdrive_last_mutation_sync_ts', ?1)",
            params![ts.to_string()],
        )?;
        Ok(())
    }

    pub fn export_history_delta_since(
        &self,
        uid: &str,
        device_id: &str,
        since_ts: i64,
        limit_msgs: usize,
    ) -> Result<HistoryDeltaBatch> {
        self.export_history_delta_since_with_mutation_cursor(
            uid,
            device_id,
            since_ts,
            since_ts,
            limit_msgs,
        )
    }

    pub fn export_history_delta_since_with_mutation_cursor(
        &self,
        uid: &str,
        device_id: &str,
        since_ts: i64,
        since_mutation_ts: i64,
        limit_msgs: usize,
    ) -> Result<HistoryDeltaBatch> {
        const HISTORY_CLOUD_MEDIA_BATCH_LIMIT: usize = 2000;
        const HISTORY_TOMBSTONE_BATCH_LIMIT: usize = 2000;

        let exported_ts = chrono::Utc::now().timestamp_millis();

        // 1A. Export new/unexported messages ordered by server-clock `ts >= since_ts`.
        let (mut messages, messages_truncated) = if limit_msgs == 0 {
            let has_any: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE ts >= ?1)",
                params![since_ts],
                |r| r.get(0),
            )?;
            (Vec::new(), has_any)
        } else {
            let probe_limit = limit_msgs.saturating_add(1) as i64;
            let mut msgs = self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                 FROM messages
                 WHERE ts >= ?1
                 ORDER BY ts ASC, thread_id ASC, msg_id ASC
                 LIMIT ?2",
                params![since_ts, probe_limit],
            )?;
            let hit_limit = msgs.len() > limit_msgs;
            if hit_limit {
                msgs.truncate(limit_msgs);
                if let Some(boundary_ts) = msgs.last().map(|m| m.ts) {
                    let mut existing_at_boundary: std::collections::HashSet<(String, String)> =
                        msgs.iter()
                            .filter(|m| m.ts == boundary_ts)
                            .map(|m| (m.thread_id.clone(), m.msg_id.clone()))
                            .collect();
                    let tie_msgs = self.query_messages(
                        "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                         FROM messages
                         WHERE ts = ?1
                         ORDER BY thread_id ASC, msg_id ASC",
                        params![boundary_ts],
                    )?;
                    for tm in tie_msgs {
                        if existing_at_boundary.insert((tm.thread_id.clone(), tm.msg_id.clone())) {
                            msgs.push(tm);
                        }
                    }
                    let has_more_after_boundary: bool = self.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE ts > ?1)",
                        params![boundary_ts],
                        |r| r.get(0),
                    )?;
                    (msgs, has_more_after_boundary)
                } else {
                    (msgs, false)
                }
            } else {
                (msgs, false)
            }
        };

        let max_message_ts_cursor = messages.iter().map(|m| m.ts).max();
        let mut seen_msg_keys: std::collections::HashSet<(String, String)> = messages
            .iter()
            .map(|m| (m.thread_id.clone(), m.msg_id.clone()))
            .collect();

        // 1B. Export mutated existing messages (e.g. recalls, pins, reactions, poll updates)
        // whose `updated_ts > 0 AND updated_ts >= since_mutation_ts`, independent of original `ts`.
        let (mut_msgs_max_ts, mut_msgs_truncated) = if limit_msgs == 0 {
            let has_any: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE updated_ts > 0 AND updated_ts >= ?1)",
                params![since_mutation_ts],
                |r| r.get(0),
            )?;
            (None, has_any)
        } else {
            let probe_limit = limit_msgs.saturating_add(1) as i64;
            let mut stmt = self.conn.prepare(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing,
                        is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name,
                        cli_msg_id, updated_ts
                 FROM messages
                 WHERE updated_ts > 0 AND updated_ts >= ?1
                 ORDER BY updated_ts ASC, thread_id ASC, msg_id ASC
                 LIMIT ?2",
            )?;
            let mut mut_rows: Vec<(MessageDto, i64)> = {
                let mut rows = stmt.query(params![since_mutation_ts, probe_limit])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    let dto = self.decrypt_raw_message_tuple(row)?;
                    let upd_ts: i64 = row.get(15)?;
                    out.push((dto, upd_ts));
                }
                out
            };
            let hit_limit = mut_rows.len() > limit_msgs;
            let truncated = if hit_limit {
                mut_rows.truncate(limit_msgs);
                if let Some(boundary_upd) = mut_rows.last().map(|(_, u)| *u) {
                    let mut existing_at_boundary: std::collections::HashSet<(String, String)> =
                        mut_rows
                            .iter()
                            .filter(|(_, u)| *u == boundary_upd)
                            .map(|(m, _)| (m.thread_id.clone(), m.msg_id.clone()))
                            .collect();
                    let mut tie_stmt = self.conn.prepare(
                        "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing,
                                is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name,
                                cli_msg_id, updated_ts
                         FROM messages
                         WHERE updated_ts = ?1
                         ORDER BY thread_id ASC, msg_id ASC",
                    )?;
                    let mut tie_rows = tie_stmt.query(params![boundary_upd])?;
                    while let Some(row) = tie_rows.next()? {
                        let dto = self.decrypt_raw_message_tuple(row)?;
                        let upd_ts: i64 = row.get(15)?;
                        if existing_at_boundary.insert((dto.thread_id.clone(), dto.msg_id.clone())) {
                            mut_rows.push((dto, upd_ts));
                        }
                    }
                    self.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE updated_ts > ?1)",
                        params![boundary_upd],
                        |r| r.get(0),
                    )?
                } else {
                    false
                }
            } else {
                false
            };
            let max_upd = mut_rows.iter().map(|(_, u)| *u).max();
            for (dto, _) in mut_rows {
                if seen_msg_keys.insert((dto.thread_id.clone(), dto.msg_id.clone())) {
                    messages.push(dto);
                }
            }
            (max_upd, truncated)
        };

        // 1C. Export local message deletion tombstones (`gdrive_deleted_messages`).
        let mut del_msg_stmt = self.conn.prepare(
            "SELECT thread_id, msg_id, deleted_ts, local_mutation_ts
             FROM gdrive_deleted_messages
             WHERE local_mutation_ts > 0 AND local_mutation_ts >= ?1
             ORDER BY local_mutation_ts ASC, thread_id ASC, msg_id ASC
             LIMIT ?2",
        )?;
        let mut del_msg_rows: Vec<(HistoryDeletedMessageTombstone, i64)> = del_msg_stmt
            .query_map(
                params![since_mutation_ts, (HISTORY_TOMBSTONE_BATCH_LIMIT + 1) as i64],
                |r| {
                    Ok((
                        HistoryDeletedMessageTombstone {
                            thread_id: r.get(0)?,
                            msg_id: r.get(1)?,
                            deleted_ts: r.get(2)?,
                        },
                        r.get::<_, i64>(3)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut del_msgs_truncated = false;
        if del_msg_rows.len() > HISTORY_TOMBSTONE_BATCH_LIMIT {
            del_msg_rows.truncate(HISTORY_TOMBSTONE_BATCH_LIMIT);
            if let Some(boundary_dm) = del_msg_rows.last().map(|(_, m)| *m) {
                let mut existing_dm: std::collections::HashSet<(String, String)> = del_msg_rows
                    .iter()
                    .filter(|(_, m)| *m == boundary_dm)
                    .map(|(t, _)| (t.thread_id.clone(), t.msg_id.clone()))
                    .collect();
                let mut tie_stmt = self.conn.prepare(
                    "SELECT thread_id, msg_id, deleted_ts, local_mutation_ts
                     FROM gdrive_deleted_messages
                     WHERE local_mutation_ts = ?1
                     ORDER BY thread_id ASC, msg_id ASC",
                )?;
                let tie_rows = tie_stmt
                    .query_map(params![boundary_dm], |r| {
                        Ok((
                            HistoryDeletedMessageTombstone {
                                thread_id: r.get(0)?,
                                msg_id: r.get(1)?,
                                deleted_ts: r.get(2)?,
                            },
                            r.get::<_, i64>(3)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (tomb, mut_ts) in tie_rows {
                    if existing_dm.insert((tomb.thread_id.clone(), tomb.msg_id.clone())) {
                        del_msg_rows.push((tomb, mut_ts));
                    }
                }
                del_msgs_truncated = self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM gdrive_deleted_messages WHERE local_mutation_ts > ?1)",
                    params![boundary_dm],
                    |r| r.get(0),
                )?;
            }
        }
        let del_msgs_max_ts = del_msg_rows.iter().map(|(_, m)| *m).max();
        let deleted_messages: Vec<HistoryDeletedMessageTombstone> =
            del_msg_rows.into_iter().map(|(t, _)| t).collect();

        // 1D. Export local thread deletion tombstones (`gdrive_deleted_threads`).
        let mut del_thr_stmt = self.conn.prepare(
            "SELECT thread_id, deleted_ts, local_mutation_ts
             FROM gdrive_deleted_threads
             WHERE local_mutation_ts > 0 AND local_mutation_ts >= ?1
             ORDER BY local_mutation_ts ASC, thread_id ASC
             LIMIT ?2",
        )?;
        let mut del_thr_rows: Vec<(HistoryDeletedThreadTombstone, i64)> = del_thr_stmt
            .query_map(
                params![since_mutation_ts, (HISTORY_TOMBSTONE_BATCH_LIMIT + 1) as i64],
                |r| {
                    Ok((
                        HistoryDeletedThreadTombstone {
                            thread_id: r.get(0)?,
                            deleted_ts: r.get(1)?,
                        },
                        r.get::<_, i64>(2)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut del_threads_truncated = false;
        if del_thr_rows.len() > HISTORY_TOMBSTONE_BATCH_LIMIT {
            del_thr_rows.truncate(HISTORY_TOMBSTONE_BATCH_LIMIT);
            if let Some(boundary_dt) = del_thr_rows.last().map(|(_, m)| *m) {
                let mut existing_dt: std::collections::HashSet<String> = del_thr_rows
                    .iter()
                    .filter(|(_, m)| *m == boundary_dt)
                    .map(|(t, _)| t.thread_id.clone())
                    .collect();
                let mut tie_stmt = self.conn.prepare(
                    "SELECT thread_id, deleted_ts, local_mutation_ts
                     FROM gdrive_deleted_threads
                     WHERE local_mutation_ts = ?1
                     ORDER BY thread_id ASC",
                )?;
                let tie_rows = tie_stmt
                    .query_map(params![boundary_dt], |r| {
                        Ok((
                            HistoryDeletedThreadTombstone {
                                thread_id: r.get(0)?,
                                deleted_ts: r.get(1)?,
                            },
                            r.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (tomb, mut_ts) in tie_rows {
                    if existing_dt.insert(tomb.thread_id.clone()) {
                        del_thr_rows.push((tomb, mut_ts));
                    }
                }
                del_threads_truncated = self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM gdrive_deleted_threads WHERE local_mutation_ts > ?1)",
                    params![boundary_dt],
                    |r| r.get(0),
                )?;
            }
        }
        let del_threads_max_ts = del_thr_rows.iter().map(|(_, m)| *m).max();
        let deleted_threads: Vec<HistoryDeletedThreadTombstone> =
            del_thr_rows.into_iter().map(|(t, _)| t).collect();

        let mutations_truncated = mut_msgs_truncated || del_msgs_truncated || del_threads_truncated;
        let max_mutation_ts_cursor = if mutations_truncated {
            let mut cap: Option<i64> = None;
            for (trunc, max_opt) in [
                (mut_msgs_truncated, mut_msgs_max_ts),
                (del_msgs_truncated, del_msgs_max_ts),
                (del_threads_truncated, del_threads_max_ts),
            ] {
                if trunc {
                    if let Some(v) = max_opt {
                        cap = Some(cap.map_or(v, |c| c.min(v)));
                    }
                }
            }
            cap.or_else(|| {
                [mut_msgs_max_ts, del_msgs_max_ts, del_threads_max_ts]
                    .into_iter()
                    .flatten()
                    .max()
            })
        } else {
            [mut_msgs_max_ts, del_msgs_max_ts, del_threads_max_ts]
                .into_iter()
                .flatten()
                .max()
        };

        let msg_thread_ids: std::collections::HashSet<&str> =
            messages.iter().map(|m| m.thread_id.as_str()).collect();
        let all_threads = self.list_threads(FolderFilter::All)?;
        let threads = if since_ts <= 0 {
            all_threads
        } else {
            all_threads
                .into_iter()
                .filter(|t| {
                    t.last_ts >= since_ts
                        || t.last_read_in_ts >= since_ts
                        || t.peer_read_ts >= since_ts
                        || msg_thread_ids.contains(t.id.as_str())
                })
                .collect()
        };

        let folders = self.list_chat_folders()?;
        let mut thread_folders = Vec::new();
        for f in &folders {
            for tid in &f.thread_ids {
                thread_folders.push((f.id.clone(), tid.clone()));
            }
        }

        // Export cached translations for exported messages (re-encrypted on target device)
        let mut translations = Vec::new();
        if !messages.is_empty() {
            struct RawTrRow {
                target_lang: String,
                provider: String,
                enc_translated: String,
                detected_source_lang: Option<String>,
                skipped: i64,
                updated_at: i64,
            }
            let mut tr_stmt = self.conn.prepare(
                "SELECT target_lang, provider, translated_text, detected_source_lang, skipped, updated_at
                 FROM message_translations
                 WHERE msg_id = ?1",
            )?;
            let mut seen_msg_ids = std::collections::HashSet::new();
            for m in &messages {
                if m.is_recalled || !seen_msg_ids.insert(m.msg_id.as_str()) {
                    continue;
                }
                let rows = tr_stmt
                    .query_map(params![&m.msg_id], |r| {
                        Ok(RawTrRow {
                            target_lang: r.get(0)?,
                            provider: r.get(1)?,
                            enc_translated: r.get(2)?,
                            detected_source_lang: r.get(3)?,
                            skipped: r.get(4)?,
                            updated_at: r.get(5)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for r in rows {
                    if let Ok(plain) = self
                        .cipher
                        .decrypt_str(&translation_aad(&m.msg_id, &r.target_lang), &r.enc_translated)
                    {
                        translations.push(HistoryTranslationRecord {
                            msg_id: m.msg_id.clone(),
                            target_lang: r.target_lang,
                            provider: r.provider,
                            translated_text: plain,
                            detected_source_lang: r.detected_source_lang,
                            skipped: r.skipped != 0,
                            updated_at: r.updated_at,
                        });
                    }
                }
            }
        }

        // Export cloud-synced media links ordered deterministically by MAX(cloud_synced_ts, updated_ts),
        // including sibling tie rows at the boundary millisecond whenever hitting the batch cap.
        struct RawCloudLinkRow {
            thread_id: String,
            msg_id: String,
            variant: String,
            url_hash: String,
            sha256: String,
            enc_orig_name: Option<String>,
            kind: String,
            mime: String,
            size_bytes: i64,
            cloud_provider: Option<String>,
            cloud_file_id: String,
            cloud_key_ver: i32,
            cloud_synced_ts: i64,
            cursor_ts: i64,
        }
        let decode_cm_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<RawCloudLinkRow> {
            Ok(RawCloudLinkRow {
                thread_id: r.get(0)?,
                msg_id: r.get(1)?,
                variant: r.get(2)?,
                url_hash: r.get(3)?,
                sha256: r.get(4)?,
                enc_orig_name: r.get(5)?,
                kind: r.get(6)?,
                mime: r.get(7)?,
                size_bytes: r.get(8)?,
                cloud_provider: r.get(9)?,
                cloud_file_id: r.get(10)?,
                cloud_key_ver: r.get(11)?,
                cloud_synced_ts: r.get(12)?,
                cursor_ts: r.get(13)?,
            })
        };
        let mut cm_stmt = self.conn.prepare(
            "SELECT mm.thread_id, mm.msg_id, mm.variant, mm.url_hash, mo.sha256, mm.original_name,
                    mo.kind, mo.mime, mo.size_bytes, mo.cloud_provider, mo.cloud_file_id,
                    mo.cloud_key_ver, COALESCE(mo.cloud_synced_ts, 0),
                    MAX(COALESCE(mo.cloud_synced_ts, 0), mm.updated_ts) AS cursor_ts
             FROM message_media mm
             INNER JOIN media_objects mo ON mo.sha256 = mm.sha256
             WHERE mo.cloud_file_id IS NOT NULL
               AND mo.cloud_file_id != ''
               AND MAX(COALESCE(mo.cloud_synced_ts, 0), mm.updated_ts) >= ?1
             ORDER BY cursor_ts ASC, mm.thread_id ASC, mm.msg_id ASC, mm.variant ASC
             LIMIT ?2",
        )?;
        let mut cm_rows = cm_stmt
            .query_map(
                params![since_mutation_ts, (HISTORY_CLOUD_MEDIA_BATCH_LIMIT + 1) as i64],
                decode_cm_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut cloud_media_truncated = false;
        if cm_rows.len() > HISTORY_CLOUD_MEDIA_BATCH_LIMIT {
            cm_rows.truncate(HISTORY_CLOUD_MEDIA_BATCH_LIMIT);
            if let Some(boundary_cm_ts) = cm_rows.last().map(|r| r.cursor_ts) {
                let mut existing_cm: std::collections::HashSet<(String, String, String)> = cm_rows
                    .iter()
                    .filter(|r| r.cursor_ts == boundary_cm_ts)
                    .map(|r| (r.thread_id.clone(), r.msg_id.clone(), r.variant.clone()))
                    .collect();
                let mut tie_stmt = self.conn.prepare(
                    "SELECT mm.thread_id, mm.msg_id, mm.variant, mm.url_hash, mo.sha256, mm.original_name,
                            mo.kind, mo.mime, mo.size_bytes, mo.cloud_provider, mo.cloud_file_id,
                            mo.cloud_key_ver, COALESCE(mo.cloud_synced_ts, 0),
                            MAX(COALESCE(mo.cloud_synced_ts, 0), mm.updated_ts) AS cursor_ts
                     FROM message_media mm
                     INNER JOIN media_objects mo ON mo.sha256 = mm.sha256
                     WHERE mo.cloud_file_id IS NOT NULL
                       AND mo.cloud_file_id != ''
                       AND MAX(COALESCE(mo.cloud_synced_ts, 0), mm.updated_ts) = ?1
                     ORDER BY mm.thread_id ASC, mm.msg_id ASC, mm.variant ASC",
                )?;
                let tie_rows = tie_stmt
                    .query_map(params![boundary_cm_ts], decode_cm_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for tr in tie_rows {
                    if existing_cm.insert((
                        tr.thread_id.clone(),
                        tr.msg_id.clone(),
                        tr.variant.clone(),
                    )) {
                        cm_rows.push(tr);
                    }
                }
                cloud_media_truncated = self.conn.query_row(
                    "SELECT EXISTS(
                         SELECT 1
                         FROM message_media mm
                         INNER JOIN media_objects mo ON mo.sha256 = mm.sha256
                         WHERE mo.cloud_file_id IS NOT NULL
                           AND mo.cloud_file_id != ''
                           AND MAX(COALESCE(mo.cloud_synced_ts, 0), mm.updated_ts) > ?1
                     )",
                    params![boundary_cm_ts],
                    |r| r.get(0),
                )?;
            }
        }

        let max_cloud_media_cursor_ts = cm_rows.iter().map(|r| r.cursor_ts).max();
        let mut cloud_media = Vec::with_capacity(cm_rows.len());
        for r in cm_rows {
            let aad = message_media_aad(&r.thread_id, &r.msg_id, &r.variant);
            let original_name = r
                .enc_orig_name
                .and_then(|s| self.cipher.decrypt_or_legacy(&aad, &s).ok());
            cloud_media.push(HistoryCloudMediaRecord {
                thread_id: r.thread_id,
                msg_id: r.msg_id,
                variant: r.variant,
                url_hash: r.url_hash,
                sha256: r.sha256,
                kind: r.kind,
                mime: r.mime,
                size_bytes: r.size_bytes.max(0) as u64,
                original_name,
                cloud_provider: r.cloud_provider.unwrap_or_else(|| "gdrive".to_string()),
                cloud_file_id: r.cloud_file_id,
                cloud_key_ver: r.cloud_key_ver,
                cloud_synced_ts: r.cloud_synced_ts,
            });
        }

        Ok(HistoryDeltaBatch {
            version: HistoryDeltaBatch::CURRENT_VERSION,
            uid: uid.to_string(),
            device_id: device_id.to_string(),
            exported_ts,
            threads,
            messages,
            folders,
            thread_folders,
            translations,
            cloud_media,
            deleted_messages,
            deleted_threads,
            messages_truncated,
            cloud_media_truncated,
            mutations_truncated,
            max_message_ts_cursor,
            max_cloud_media_cursor_ts,
            max_mutation_ts_cursor,
        })
    }

    pub fn import_history_delta_idempotent(&self, batch: &HistoryDeltaBatch) -> Result<usize> {
        Ok(self.import_history_delta_idempotent_detailed(batch)?.applied_messages)
    }

    pub fn import_history_delta_idempotent_detailed(
        &self,
        batch: &HistoryDeltaBatch,
    ) -> Result<HistoryDeltaImportReport> {
        let tx = self.conn.unchecked_transaction()?;
        let mut report = HistoryDeltaImportReport::default();
        let mut fts_deletes: Vec<(String, Option<String>)> = Vec::new();

        // 0A. Apply thread deletion tombstones first (with `local_mutation_ts = 0` to prevent re-export ping-pong).
        for dt in &batch.deleted_threads {
            let tid = dt.thread_id.trim();
            if tid.is_empty() {
                continue;
            }
            let del_ts = normalize_ts_ms(dt.deleted_ts);
            tx.execute(
                "INSERT INTO gdrive_deleted_threads (thread_id, deleted_ts, local_mutation_ts)
                 VALUES (?1, ?2, 0)
                 ON CONFLICT(thread_id) DO UPDATE SET
                     deleted_ts = MAX(gdrive_deleted_threads.deleted_ts, excluded.deleted_ts)",
                params![tid, del_ts],
            )?;

            // Collect & remove messages belonging to this thread up to `del_ts`.
            let doomed_msg_ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT msg_id FROM messages WHERE thread_id = ?1 AND ts <= ?2",
                )?;
                let out = stmt
                    .query_map(params![tid, del_ts], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                out
            };
            let mut shas_to_check: Vec<String> = Vec::new();
            for mid in &doomed_msg_ids {
                tx.execute(
                    "DELETE FROM message_translations
                     WHERE msg_id = ?1
                       AND msg_id NOT IN (SELECT msg_id FROM messages WHERE thread_id != ?2)",
                    params![mid, tid],
                )?;
                let mut mstmt = tx.prepare(
                    "SELECT DISTINCT sha256 FROM message_media
                     WHERE thread_id = ?1 AND msg_id = ?2 AND sha256 IS NOT NULL AND sha256 != ''",
                )?;
                let mshas = mstmt
                    .query_map(params![tid, mid], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                shas_to_check.extend(mshas);
                tx.execute(
                    "DELETE FROM message_media WHERE thread_id = ?1 AND msg_id = ?2",
                    params![tid, mid],
                )?;
                tx.execute(
                    "DELETE FROM messages WHERE thread_id = ?1 AND msg_id = ?2",
                    params![tid, mid],
                )?;
                fts_deletes.push((tid.to_string(), Some(mid.clone())));
            }
            if !shas_to_check.is_empty() {
                shas_to_check.sort();
                shas_to_check.dedup();
                let orphans = Self::cleanup_orphaned_media_for_shas_in_tx(
                    &tx,
                    shas_to_check,
                    Some("remote_thread_deleted"),
                )?;
                report.orphaned_local_paths.extend(orphans);
            }

            let has_newer_msgs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE thread_id = ?1)",
                params![tid],
                |r| r.get(0),
            )?;
            let cur_last_ts: Option<i64> = tx
                .query_row(
                    "SELECT last_ts FROM threads WHERE id = ?1 LIMIT 1",
                    params![tid],
                    |r| r.get(0),
                )
                .optional()?;
            if !has_newer_msgs && cur_last_ts.is_none_or(|lt| lt <= del_ts) {
                // Also remove any lingering thread media refs if messages table was already empty
                let extra_shas: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT DISTINCT sha256 FROM message_media
                         WHERE thread_id = ?1 AND sha256 IS NOT NULL AND sha256 != ''",
                    )?;
                    let out = stmt
                        .query_map(params![tid], |r| r.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    out
                };
                if !extra_shas.is_empty() {
                    tx.execute("DELETE FROM message_media WHERE thread_id = ?1", params![tid])?;
                    let orphans = Self::cleanup_orphaned_media_for_shas_in_tx(
                        &tx,
                        extra_shas,
                        Some("remote_thread_deleted"),
                    )?;
                    report.orphaned_local_paths.extend(orphans);
                }
                let deleted_t = tx.execute("DELETE FROM threads WHERE id = ?1", params![tid])?;
                if deleted_t > 0 || !doomed_msg_ids.is_empty() {
                    report.deleted_threads += 1;
                }
                fts_deletes.push((tid.to_string(), None));

                // Scrub thread from local chat_folders
                let folder_rows: Vec<(String, String)> = {
                    let mut stmt = tx.prepare("SELECT id, config_json FROM chat_folders")?;
                    let out = stmt
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    out
                };
                for (fid, enc_json) in folder_rows {
                    let aad = folder_aad(&fid);
                    if let Ok(raw_json) = self.cipher.decrypt_str(&aad, &enc_json) {
                        if let Ok(mut dto) = serde_json::from_str::<ChatFolderDto>(&raw_json) {
                            let prev_inc = dto.thread_ids.len();
                            let prev_exc = dto.excluded_thread_ids.len();
                            dto.thread_ids.retain(|id| id != tid);
                            dto.excluded_thread_ids.retain(|id| id != tid);
                            if dto.thread_ids.len() != prev_inc
                                || dto.excluded_thread_ids.len() != prev_exc
                            {
                                let updated_json = serde_json::to_string(&dto)?;
                                let updated_enc = self.cipher.encrypt_str(&aad, &updated_json)?;
                                tx.execute(
                                    "UPDATE chat_folders SET config_json = ?2 WHERE id = ?1",
                                    params![fid, updated_enc],
                                )?;
                            }
                        }
                    }
                }
            }
        }

        // 0B. Apply message deletion tombstones (with `local_mutation_ts = 0` to prevent re-export ping-pong).
        for dm in &batch.deleted_messages {
            let tid = dm.thread_id.trim();
            let mid = dm.msg_id.trim();
            if tid.is_empty() || mid.is_empty() {
                continue;
            }
            let del_ts = normalize_ts_ms(dm.deleted_ts);
            tx.execute(
                "INSERT INTO gdrive_deleted_messages (thread_id, msg_id, deleted_ts, local_mutation_ts)
                 VALUES (?1, ?2, ?3, 0)
                 ON CONFLICT(thread_id, msg_id) DO UPDATE SET
                     deleted_ts = MAX(gdrive_deleted_messages.deleted_ts, excluded.deleted_ts)",
                params![tid, mid, del_ts],
            )?;
            let orphans = Self::cleanup_orphaned_media_for_msg_in_tx(
                &tx,
                tid,
                mid,
                Some("remote_msg_deleted"),
            )?;
            report.orphaned_local_paths.extend(orphans);
            let del_cnt = tx.execute(
                "DELETE FROM messages WHERE thread_id = ?1 AND msg_id = ?2",
                params![tid, mid],
            )?;
            tx.execute(
                "DELETE FROM message_translations WHERE msg_id = ?1",
                params![mid],
            )?;
            if del_cnt > 0 {
                report.deleted_messages += 1;
                fts_deletes.push((tid.to_string(), Some(mid.to_string())));
            }
        }

        // 1. Upsert threads without regressing newer local timestamps, previews, or read watermarks,
        // and without resurrecting threads covered by a deletion tombstone.
        for t in &batch.threads {
            let last_ts = normalize_ts_ms(t.last_ts);
            let tombstoned_ts: Option<i64> = tx
                .query_row(
                    "SELECT deleted_ts FROM gdrive_deleted_threads WHERE thread_id = ?1 LIMIT 1",
                    params![&t.id],
                    |r| r.get(0),
                )
                .optional()?;
            if tombstoned_ts.is_some_and(|del_ts| del_ts >= last_ts) {
                continue;
            }
            let kind_str = match t.kind {
                ThreadKind::Direct => "direct",
                ThreadKind::Group => "group",
                ThreadKind::Cloud => "cloud",
            };
            let aad = thread_aad(&t.id);
            let enc_title = self.cipher.encrypt_str(&aad, &t.title)?;
            let enc_avatar = self.cipher.encrypt_str(&aad, &t.avatar)?;
            let enc_preview = self.cipher.encrypt_str(&aad, &t.last_preview)?;
            let title_fallback = (t.title.trim().is_empty() || t.title == t.id) as i64;
            let avatar_empty = t.avatar.is_empty() as i64;
            let preview_empty = t.last_preview.is_empty() as i64;
            let last_read_in_ts = normalize_ts_ms(t.last_read_in_ts);
            let peer_read_ts = normalize_ts_ms(t.peer_read_ts);
            let muted_until_ts = normalize_ts_ms(t.muted_until_ts);

            tx.execute(
                r#"
                INSERT INTO threads (id, title, title_norm, avatar, kind, last_preview, last_ts, unread_count, is_pinned, is_muted, last_read_in_ts, peer_read_ts, muted_until_ts, is_archived)
                VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                ON CONFLICT(id) DO UPDATE SET
                    title = CASE WHEN ?14 = 1 THEN threads.title ELSE excluded.title END,
                    title_norm = '',
                    avatar = CASE WHEN ?15 = 1 THEN threads.avatar ELSE excluded.avatar END,
                    kind = excluded.kind,
                    last_preview = CASE
                        WHEN ?16 = 0 AND excluded.last_ts >= threads.last_ts THEN excluded.last_preview
                        ELSE threads.last_preview
                    END,
                    last_ts = MAX(threads.last_ts, excluded.last_ts),
                    unread_count = CASE
                        WHEN excluded.last_ts > threads.last_ts THEN excluded.unread_count
                        WHEN excluded.last_read_in_ts >= threads.last_ts AND threads.last_ts > 0 THEN 0
                        ELSE threads.unread_count
                    END,
                    is_pinned = MAX(threads.is_pinned, excluded.is_pinned),
                    is_muted = MAX(threads.is_muted, excluded.is_muted),
                    last_read_in_ts = MAX(threads.last_read_in_ts, excluded.last_read_in_ts),
                    peer_read_ts = MAX(threads.peer_read_ts, excluded.peer_read_ts),
                    muted_until_ts = MAX(threads.muted_until_ts, excluded.muted_until_ts),
                    is_archived = MAX(threads.is_archived, excluded.is_archived)
                "#,
                params![
                    t.id,
                    enc_title,
                    enc_avatar,
                    kind_str,
                    enc_preview,
                    last_ts,
                    t.unread_count,
                    t.is_pinned as i64,
                    t.is_muted as i64,
                    last_read_in_ts,
                    peer_read_ts,
                    muted_until_ts,
                    t.is_archived as i64,
                    title_fallback,
                    avatar_empty,
                    preview_empty,
                ],
            )?;
        }

        // 2. Idempotently merge folders + thread_folders without discarding existing local thread_ids
        // and without re-adding threads that are deleted (tombstoned and absent from `threads`).
        if !batch.folders.is_empty() || !batch.thread_folders.is_empty() {
            let dead_tombstoned_threads: std::collections::HashSet<String> = {
                let mut stmt = tx.prepare(
                    "SELECT dt.thread_id
                     FROM gdrive_deleted_threads dt
                     WHERE NOT EXISTS (SELECT 1 FROM threads t WHERE t.id = dt.thread_id)",
                )?;
                let out = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?;
                out
            };

            let mut extra_links: HashMap<&str, Vec<&str>> = HashMap::new();
            for (fid, tid) in &batch.thread_folders {
                if !dead_tombstoned_threads.contains(tid.as_str()) {
                    extra_links.entry(fid.as_str()).or_default().push(tid.as_str());
                }
            }

            let mut existing_folders: HashMap<String, ChatFolderDto> = HashMap::new();
            {
                let mut fstmt = tx.prepare("SELECT id, config_json FROM chat_folders")?;
                let rows = fstmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (id, enc_json) in rows {
                    if let Ok(raw) = self.cipher.decrypt_str(&folder_aad(&id), &enc_json) {
                        if let Ok(dto) = serde_json::from_str::<ChatFolderDto>(&raw) {
                            existing_folders.insert(id, dto);
                        }
                    }
                }
            }

            let mut touched_folder_ids = std::collections::HashSet::new();
            for incoming in &batch.folders {
                let mut merged = match existing_folders.get(&incoming.id) {
                    Some(local) => {
                        let mut m = incoming.clone();
                        for tid in &local.thread_ids {
                            if !m.thread_ids.iter().any(|x| x == tid) {
                                m.thread_ids.push(tid.clone());
                            }
                        }
                        for tid in &local.excluded_thread_ids {
                            if !m.excluded_thread_ids.iter().any(|x| x == tid) {
                                m.excluded_thread_ids.push(tid.clone());
                            }
                        }
                        if m.remote_label_id.is_none() {
                            m.remote_label_id = local.remote_label_id.clone();
                        }
                        m
                    }
                    None => incoming.clone(),
                };
                if let Some(tids) = extra_links.get(merged.id.as_str()) {
                    for &tid in tids {
                        if !merged.thread_ids.iter().any(|x| x == tid) {
                            merged.thread_ids.push(tid.to_string());
                        }
                    }
                }
                merged
                    .thread_ids
                    .retain(|tid| !dead_tombstoned_threads.contains(tid.as_str()));
                merged
                    .excluded_thread_ids
                    .retain(|tid| !dead_tombstoned_threads.contains(tid.as_str()));
                let aad = folder_aad(&merged.id);
                let enc_name = self.cipher.encrypt_str(&aad, &merged.name)?;
                let raw_json = serde_json::to_string(&merged)?;
                let enc_json = self.cipher.encrypt_str(&aad, &raw_json)?;
                tx.execute(
                    "INSERT INTO chat_folders (id, name, icon, config_json, position)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(id) DO UPDATE SET
                         name = excluded.name,
                         icon = excluded.icon,
                         config_json = excluded.config_json,
                         position = excluded.position",
                    params![merged.id, enc_name, merged.icon, enc_json, merged.position],
                )?;
                touched_folder_ids.insert(merged.id.clone());
                existing_folders.insert(merged.id.clone(), merged);
            }

            for (fid, tids) in extra_links {
                if touched_folder_ids.contains(fid) {
                    continue;
                }
                if let Some(local) = existing_folders.get_mut(fid) {
                    let mut changed = false;
                    for tid in tids {
                        if !local.thread_ids.iter().any(|x| x == tid) {
                            local.thread_ids.push(tid.to_string());
                            changed = true;
                        }
                    }
                    if changed {
                        let aad = folder_aad(&local.id);
                        let enc_name = self.cipher.encrypt_str(&aad, &local.name)?;
                        let raw_json = serde_json::to_string(local)?;
                        let enc_json = self.cipher.encrypt_str(&aad, &raw_json)?;
                        tx.execute(
                            "UPDATE chat_folders SET name = ?2, config_json = ?3 WHERE id = ?1",
                            params![local.id, enc_name, enc_json],
                        )?;
                    }
                }
            }
        }

        // 3. Idempotently merge messages without un-recalling, duplicating, or resurrecting tombstoned items.
        struct ExistingMsgMeta {
            is_recalled: i64,
            is_pinned: i64,
            quote_json: Option<String>,
            reactions_json: String,
            attachment_url: Option<String>,
            attachment_name: Option<String>,
            cli_msg_id: String,
        }
        let mut applied_count = 0usize;
        let mut fts_updates: Vec<(String, String, i64, String, String)> = Vec::new();
        let mut msg_text_map: HashMap<&str, &str> = HashMap::new();

        for m in &batch.messages {
            let ts = normalize_ts_ms(m.ts);

            // Never resurrect a message covered by a message or thread deletion tombstone.
            let msg_tombstoned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM gdrive_deleted_messages WHERE thread_id = ?1 AND msg_id = ?2)",
                params![&m.thread_id, &m.msg_id],
                |r| r.get(0),
            )?;
            if msg_tombstoned {
                continue;
            }
            let thread_tombstoned_ts: Option<i64> = tx
                .query_row(
                    "SELECT deleted_ts FROM gdrive_deleted_threads WHERE thread_id = ?1 LIMIT 1",
                    params![&m.thread_id],
                    |r| r.get(0),
                )
                .optional()?;
            if thread_tombstoned_ts.is_some_and(|del_ts| del_ts >= ts) {
                continue;
            }

            if !m.is_recalled {
                msg_text_map.insert(m.msg_id.as_str(), m.text.as_str());
            }
            let aad = msg_aad(&m.thread_id, &m.msg_id);
            let incoming_cli_msg_id = m
                .cli_msg_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != "0")
                .unwrap_or("");
            let existing: Option<ExistingMsgMeta> = tx
                .query_row(
                    "SELECT is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                     FROM messages
                     WHERE thread_id = ?1 AND msg_id = ?2
                     LIMIT 1",
                    params![&m.thread_id, &m.msg_id],
                    |r| {
                        Ok(ExistingMsgMeta {
                            is_recalled: r.get(0)?,
                            is_pinned: r.get(1)?,
                            quote_json: r.get(2)?,
                            reactions_json: r.get(3)?,
                            attachment_url: r.get(4)?,
                            attachment_name: r.get(5)?,
                            cli_msg_id: r.get(6).unwrap_or_default(),
                        })
                    },
                )
                .optional()?;

            match existing {
                None => {
                    let quote_json = m
                        .quote
                        .as_ref()
                        .and_then(|q| serde_json::to_string(q).ok());
                    let reactions_json =
                        serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".to_string());
                    let enc_sender_id = self.cipher.encrypt_str(&aad, &m.sender_id)?;
                    let enc_sender_name = self.cipher.encrypt_str(&aad, &m.sender_name)?;
                    let enc_text = self.cipher.encrypt_str(&aad, &m.text)?;
                    let enc_quote = self.cipher.encrypt_opt(&aad, quote_json.as_deref())?;
                    let enc_reactions = self.cipher.encrypt_str(&aad, &reactions_json)?;
                    let enc_att_url = if m.is_recalled {
                        None
                    } else {
                        self.cipher.encrypt_opt(&aad, m.attachment_url.as_deref())?
                    };
                    let enc_att_name = if m.is_recalled {
                        None
                    } else {
                        self.cipher.encrypt_opt(&aad, m.attachment_name.as_deref())?
                    };
                    tx.execute(
                        "INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id, updated_ts)
                         VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0)",
                        params![
                            m.msg_id,
                            m.thread_id,
                            enc_sender_id,
                            enc_sender_name,
                            enc_text,
                            m.msg_type,
                            ts,
                            m.is_outgoing as i64,
                            m.is_recalled as i64,
                            m.is_pinned as i64,
                            enc_quote,
                            enc_reactions,
                            enc_att_url,
                            enc_att_name,
                            incoming_cli_msg_id,
                        ],
                    )?;
                    if !m.is_outgoing {
                        tx.execute(
                            "UPDATE threads SET peer_read_ts = MAX(peer_read_ts, ?2) WHERE id = ?1",
                            params![m.thread_id, ts],
                        )?;
                    }
                    fts_updates.push((
                        m.msg_id.clone(),
                        m.thread_id.clone(),
                        ts,
                        strip_vi_accents(&m.sender_name),
                        strip_vi_accents(&m.text),
                    ));
                    applied_count += 1;
                }
                Some(cur) => {
                    if cur.is_recalled != 0 {
                        // Never un-recall a locally recalled message.
                        continue;
                    }
                    if m.is_recalled {
                        let enc_text = self.cipher.encrypt_str(&aad, &m.text)?;
                        tx.execute(
                            "UPDATE messages
                             SET is_recalled = 1, text = ?3, attachment_url = NULL, attachment_name = NULL
                             WHERE thread_id = ?1 AND msg_id = ?2",
                            params![m.thread_id, m.msg_id, enc_text],
                        )?;
                        tx.execute(
                            "DELETE FROM message_translations WHERE msg_id = ?1",
                            params![m.msg_id],
                        )?;
                        let orphans = Self::cleanup_orphaned_media_for_msg_in_tx(
                            &tx,
                            &m.thread_id,
                            &m.msg_id,
                            Some("remote_msg_recalled"),
                        )?;
                        report.orphaned_local_paths.extend(orphans);
                        fts_updates.push((
                            m.msg_id.clone(),
                            m.thread_id.clone(),
                            ts,
                            strip_vi_accents(&m.sender_name),
                            strip_vi_accents(&m.text),
                        ));
                        applied_count += 1;
                        continue;
                    }

                    let pin_upgraded = m.is_pinned && cur.is_pinned == 0;
                    let fill_quote = cur.quote_json.is_none() && m.quote.is_some();
                    let fill_att_url = cur.attachment_url.is_none() && m.attachment_url.is_some();
                    let fill_att_name =
                        cur.attachment_name.is_none() && m.attachment_name.is_some();
                    let fill_cli_msg_id =
                        cur.cli_msg_id.trim().is_empty() && !incoming_cli_msg_id.is_empty();
                    let cur_reactions = self
                        .cipher
                        .decrypt_or_legacy(&aad, &cur.reactions_json)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Vec<ReactionSummary>>(&s).ok())
                        .unwrap_or_default();
                    let cur_reaction_total: u32 = cur_reactions.iter().map(|r| r.count).sum();
                    let incoming_reaction_total: u32 = m.reactions.iter().map(|r| r.count).sum();
                    let upgrade_reactions = (cur_reactions.is_empty() && !m.reactions.is_empty())
                        || incoming_reaction_total > cur_reaction_total;

                    // Merge poll JSON updates monotonically for `chat.poll` messages.
                    let merged_poll_att_name: Option<String> = if m
                        .msg_type
                        .eq_ignore_ascii_case("chat.poll")
                    {
                        let incoming_poll =
                            parse_poll_dto_from_attachment_name(m.attachment_name.as_deref());
                        if incoming_poll.is_some() {
                            let cur_att_plain = cur
                                .attachment_name
                                .as_deref()
                                .and_then(|s| self.cipher.decrypt_or_legacy(&aad, s).ok());
                            let cur_poll =
                                parse_poll_dto_from_attachment_name(cur_att_plain.as_deref());
                            if let Some(folded) = fold_poll_dto(cur_poll, incoming_poll) {
                                if let Ok(json) = serde_json::to_string(&folded) {
                                    let candidate = format!("teza-poll:{json}");
                                    (cur_att_plain.as_deref() != Some(candidate.as_str()))
                                        .then_some(candidate)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if pin_upgraded
                        || fill_quote
                        || fill_att_url
                        || fill_att_name
                        || upgrade_reactions
                        || fill_cli_msg_id
                        || merged_poll_att_name.is_some()
                    {
                        let new_pinned = (cur.is_pinned != 0 || m.is_pinned) as i64;
                        let new_quote = if fill_quote {
                            let qj = m
                                .quote
                                .as_ref()
                                .and_then(|q| serde_json::to_string(q).ok());
                            self.cipher.encrypt_opt(&aad, qj.as_deref())?
                        } else {
                            cur.quote_json
                        };
                        let new_reactions = if upgrade_reactions {
                            let rj = serde_json::to_string(&m.reactions)
                                .unwrap_or_else(|_| "[]".to_string());
                            self.cipher.encrypt_str(&aad, &rj)?
                        } else {
                            cur.reactions_json
                        };
                        let new_att_url = if fill_att_url {
                            self.cipher.encrypt_opt(&aad, m.attachment_url.as_deref())?
                        } else {
                            cur.attachment_url
                        };
                        let new_att_name = if let Some(ref poll_att) = merged_poll_att_name {
                            self.cipher.encrypt_opt(&aad, Some(poll_att.as_str()))?
                        } else if fill_att_name {
                            self.cipher
                                .encrypt_opt(&aad, m.attachment_name.as_deref())?
                        } else {
                            cur.attachment_name
                        };
                        let new_cli_msg_id = if fill_cli_msg_id {
                            incoming_cli_msg_id.to_string()
                        } else {
                            cur.cli_msg_id
                        };
                        tx.execute(
                            "UPDATE messages
                             SET is_pinned = ?3, quote_json = ?4, reactions_json = ?5,
                                 attachment_url = ?6, attachment_name = ?7, cli_msg_id = ?8
                             WHERE thread_id = ?1 AND msg_id = ?2",
                            params![
                                m.thread_id,
                                m.msg_id,
                                new_pinned,
                                new_quote,
                                new_reactions,
                                new_att_url,
                                new_att_name,
                                new_cli_msg_id,
                            ],
                        )?;
                        applied_count += 1;
                    }
                }
            }
        }

        // 4. Import translations (re-computing keyed MAC with local DB cipher)
        for tr in &batch.translations {
            let lang = tr.target_lang.trim().to_ascii_uppercase();
            if lang.is_empty() {
                continue;
            }
            let Some(&orig_text) = msg_text_map.get(tr.msg_id.as_str()) else {
                continue;
            };
            let text_hash = self.cipher.keyed_translation_mac(&tr.msg_id, orig_text);
            let enc_text = self
                .cipher
                .encrypt_str(&translation_aad(&tr.msg_id, &lang), &tr.translated_text)?;
            tx.execute(
                "INSERT OR IGNORE INTO message_translations
                 (msg_id, target_lang, text_hash, provider, translated_text, detected_source_lang, skipped, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    tr.msg_id,
                    lang,
                    text_hash,
                    tr.provider,
                    enc_text,
                    tr.detected_source_lang.as_deref(),
                    tr.skipped as i64,
                    tr.updated_at,
                ],
            )?;
        }

        // 5. Import cloud media metadata (skipping recalled or tombstoned messages and respecting `cloud_excluded = 1`).
        for cm in &batch.cloud_media {
            if cm.sha256.is_empty() || cm.cloud_file_id.is_empty() {
                continue;
            }
            if !cm.thread_id.is_empty() && cm.thread_id != "__url__" {
                let msg_tombstoned: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM gdrive_deleted_messages WHERE thread_id = ?1 AND msg_id = ?2)",
                    params![&cm.thread_id, &cm.msg_id],
                    |r| r.get(0),
                )?;
                if msg_tombstoned {
                    continue;
                }
                let msg_state: Option<(i64, i64)> = tx
                    .query_row(
                        "SELECT is_recalled, ts FROM messages WHERE thread_id = ?1 AND msg_id = ?2 LIMIT 1",
                        params![&cm.thread_id, &cm.msg_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((is_recalled, _)) = msg_state {
                    if is_recalled != 0 {
                        continue;
                    }
                } else {
                    let has_thread_tombstone: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM gdrive_deleted_threads WHERE thread_id = ?1)",
                        params![&cm.thread_id],
                        |r| r.get(0),
                    )?;
                    if has_thread_tombstone {
                        continue;
                    }
                }
            }

            let size_i64 = i64::try_from(cm.size_bytes).unwrap_or(i64::MAX);
            tx.execute(
                r#"
                INSERT INTO media_objects (
                    sha256, kind, mime, size_bytes, local_state, is_encrypted, pin_count,
                    created_ts, last_accessed_ts, cloud_provider, cloud_file_id, cloud_encrypted,
                    cloud_key_ver, cloud_synced_ts
                )
                VALUES (?1, ?2, ?3, ?4, 0, 1, 0, ?5, ?5, ?6, ?7, 1, ?8, ?9)
                ON CONFLICT(sha256) DO UPDATE SET
                    cloud_provider = CASE WHEN media_objects.cloud_excluded = 1 THEN media_objects.cloud_provider ELSE COALESCE(media_objects.cloud_provider, excluded.cloud_provider) END,
                    cloud_file_id = CASE WHEN media_objects.cloud_excluded = 1 THEN media_objects.cloud_file_id ELSE COALESCE(media_objects.cloud_file_id, excluded.cloud_file_id) END,
                    cloud_key_ver = MAX(media_objects.cloud_key_ver, excluded.cloud_key_ver),
                    cloud_synced_ts = CASE WHEN media_objects.cloud_excluded = 1 THEN media_objects.cloud_synced_ts ELSE COALESCE(media_objects.cloud_synced_ts, excluded.cloud_synced_ts) END
                "#,
                params![
                    cm.sha256,
                    cm.kind,
                    cm.mime,
                    size_i64,
                    cm.cloud_synced_ts,
                    cm.cloud_provider,
                    cm.cloud_file_id,
                    cm.cloud_key_ver,
                    cm.cloud_synced_ts,
                ],
            )?;
            let variant = if cm.variant.trim().is_empty() {
                "primary"
            } else {
                cm.variant.trim()
            };
            let aad = message_media_aad(&cm.thread_id, &cm.msg_id, variant);
            let enc_orig_name = self.cipher.encrypt_opt(&aad, cm.original_name.as_deref())?;
            tx.execute(
                r#"
                INSERT INTO message_media (
                    thread_id, msg_id, variant, url_hash, sha256, original_name,
                    saved_user_path, cdn_status, updated_ts
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, ?7)
                ON CONFLICT(thread_id, msg_id, variant) DO UPDATE SET
                    url_hash = CASE WHEN message_media.url_hash = '' THEN excluded.url_hash ELSE message_media.url_hash END,
                    sha256 = COALESCE(message_media.sha256, excluded.sha256),
                    original_name = COALESCE(message_media.original_name, excluded.original_name),
                    updated_ts = MAX(message_media.updated_ts, excluded.updated_ts)
                "#,
                params![
                    cm.thread_id,
                    cm.msg_id,
                    variant,
                    cm.url_hash,
                    cm.sha256,
                    enc_orig_name,
                    cm.cloud_synced_ts,
                ],
            )?;
            tx.execute(
                "DELETE FROM gdrive_pending_cloud_deletes WHERE sha256 = ?1",
                params![&cm.sha256],
            )?;
        }

        tx.commit()?;

        if !fts_deletes.is_empty() || !fts_updates.is_empty() {
            let fts_tx = self.fts_conn.unchecked_transaction()?;
            for (tid, mid_opt) in fts_deletes {
                if let Some(mid) = mid_opt {
                    fts_tx.execute(
                        "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                        params![tid, mid],
                    )?;
                } else {
                    fts_tx.execute(
                        "DELETE FROM messages_fts WHERE thread_id = ?1",
                        params![tid],
                    )?;
                }
            }
            for (mid, tid, ts, sender_norm, text_norm) in fts_updates {
                fts_tx.execute(
                    "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
                    params![tid, mid],
                )?;
                fts_tx.execute(
                    "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![mid, tid, ts, sender_norm, text_norm],
                )?;
            }
            fts_tx.commit()?;
        }

        report.applied_messages = applied_count;
        report.orphaned_local_paths.sort();
        report.orphaned_local_paths.dedup();
        Ok(report)
    }

    // -------------------------------------------------------------------------
    // Item 1: Local AI Chat History, Encrypted Sessions & Unread Summary Query
    // -------------------------------------------------------------------------

    fn normalize_ai_single_line(raw: &str) -> String {
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn truncate_ai_text_with_ellipsis(raw: &str, max_chars: usize) -> String {
        let cleaned = Self::normalize_ai_single_line(raw);
        if cleaned.is_empty() || max_chars == 0 {
            return String::new();
        }
        let mut chars = cleaned.chars();
        let prefix: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_none() {
            return prefix;
        }
        if max_chars == 1 {
            return "…".to_string();
        }
        let head: String = cleaned.chars().take(max_chars - 1).collect();
        format!("{}…", head.trim_end())
    }

    fn truncate_ai_title_plain(raw: &str, max_chars: usize) -> String {
        let cleaned = Self::normalize_ai_single_line(raw);
        if cleaned.chars().count() <= max_chars {
            return cleaned;
        }
        cleaned
            .chars()
            .take(max_chars)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn derive_ai_session_auto_title(content: &str, fallback: &str) -> String {
        let derived = Self::truncate_ai_text_with_ellipsis(content, AI_SESSION_AUTO_TITLE_CHARS);
        if derived.is_empty() {
            fallback.to_string()
        } else {
            derived
        }
    }

    fn validate_ai_session_id(id: &str) -> Result<&str> {
        let clean = id.trim();
        if clean.is_empty() {
            bail!("session_id cuộc trò chuyện AI không được để trống");
        }
        if clean.chars().count() > AI_SESSION_MAX_ID_CHARS {
            bail!("session_id cuộc trò chuyện AI tối đa {AI_SESSION_MAX_ID_CHARS} ký tự");
        }
        Ok(clean)
    }

    fn backfill_orphan_ai_sessions(&self) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT m.session_id, MIN(m.ts), MAX(m.ts)
             FROM ai_messages m
             LEFT JOIN ai_sessions s ON s.id = m.session_id
             WHERE s.id IS NULL AND TRIM(m.session_id) != ''
             GROUP BY m.session_id",
        )?;
        let orphans: Vec<(String, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        if orphans.is_empty() {
            return Ok(());
        }

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for (session_id, min_ts, max_ts) in orphans {
            let first_user_msg: Option<(String, String)> = tx
                .query_row(
                    "SELECT id, content
                     FROM ai_messages
                     WHERE session_id = ?1 AND role = 'user'
                     ORDER BY ts ASC, id ASC
                     LIMIT 1",
                    params![&session_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let title = match first_user_msg {
                Some((msg_id, enc_content)) => {
                    let plain = self.cipher.decrypt_str(&ai_msg_aad(&msg_id), &enc_content)?;
                    Self::derive_ai_session_auto_title(&plain, AI_SESSION_LEGACY_FALLBACK_TITLE)
                }
                None => AI_SESSION_LEGACY_FALLBACK_TITLE.to_string(),
            };
            let created_ts = normalize_ts_ms(min_ts);
            let updated_ts = normalize_ts_ms(max_ts).max(created_ts);
            let enc_title = self
                .cipher
                .encrypt_str(&ai_session_aad(&session_id), &title)?;
            tx.execute(
                "INSERT OR IGNORE INTO ai_sessions (id, title, created_ts, updated_ts)
                 VALUES (?1, ?2, ?3, ?4)",
                params![session_id, enc_title, created_ts, updated_ts],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn load_ai_session_stats(&self, session_id: &str) -> Result<(usize, String)> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ai_messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        let message_count = usize::try_from(count).unwrap_or(0);
        if message_count == 0 {
            return Ok((0, String::new()));
        }
        let last_msg: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, content
                 FROM ai_messages
                 WHERE session_id = ?1
                 ORDER BY ts DESC, id DESC
                 LIMIT 1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let last_preview = match last_msg {
            Some((msg_id, enc_content)) => {
                let plain = self.cipher.decrypt_str(&ai_msg_aad(&msg_id), &enc_content)?;
                Self::truncate_ai_text_with_ellipsis(&plain, AI_SESSION_PREVIEW_CHARS)
            }
            None => String::new(),
        };
        Ok((message_count, last_preview))
    }

    pub fn list_ai_sessions(&self, limit: usize) -> Result<Vec<AiSessionDto>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.backfill_orphan_ai_sessions()?;

        let safe_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_ts, updated_ts
             FROM ai_sessions
             ORDER BY updated_ts DESC, id DESC
             LIMIT ?1",
        )?;
        let rows: Vec<(String, String, i64, i64)> = stmt
            .query_map(params![safe_limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut out = Vec::with_capacity(rows.len());
        for (id, enc_title, created_ts, updated_ts) in rows {
            let title = self.cipher.decrypt_str(&ai_session_aad(&id), &enc_title)?;
            let (message_count, last_preview) = self.load_ai_session_stats(&id)?;
            out.push(AiSessionDto {
                id,
                title,
                created_ts,
                updated_ts,
                message_count,
                last_preview,
            });
        }
        Ok(out)
    }

    pub fn create_ai_session(&self, id: &str, title: &str, now_ts: i64) -> Result<AiSessionDto> {
        let clean_id = Self::validate_ai_session_id(id)?;
        let clean_title = Self::truncate_ai_title_plain(title, AI_SESSION_MAX_TITLE_CHARS);
        let has_custom_title = !clean_title.is_empty();
        let final_title = if has_custom_title {
            clean_title
        } else {
            AI_SESSION_DEFAULT_TITLE.to_string()
        };
        let ts = normalize_ts_ms(now_ts);
        let enc_title = self
            .cipher
            .encrypt_str(&ai_session_aad(clean_id), &final_title)?;

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO ai_sessions (id, title, created_ts, updated_ts)
             VALUES (?1, ?2, ?3, ?3)",
            params![clean_id, enc_title, ts],
        )?;
        let (stored_title, created_ts, updated_ts) = if inserted > 0 {
            (final_title, ts, ts)
        } else {
            let (existing_enc_title, existing_created_ts, existing_updated_ts): (String, i64, i64) =
                tx.query_row(
                    "SELECT title, created_ts, updated_ts FROM ai_sessions WHERE id = ?1 LIMIT 1",
                    params![clean_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
            if has_custom_title {
                let next_updated_ts = existing_updated_ts.max(ts);
                tx.execute(
                    "UPDATE ai_sessions SET title = ?1, updated_ts = ?2 WHERE id = ?3",
                    params![enc_title, next_updated_ts, clean_id],
                )?;
                (final_title, existing_created_ts, next_updated_ts)
            } else {
                let existing_title = self
                    .cipher
                    .decrypt_str(&ai_session_aad(clean_id), &existing_enc_title)?;
                (existing_title, existing_created_ts, existing_updated_ts)
            }
        };
        tx.commit()?;

        let (message_count, last_preview) = self.load_ai_session_stats(clean_id)?;
        Ok(AiSessionDto {
            id: clean_id.to_string(),
            title: stored_title,
            created_ts,
            updated_ts,
            message_count,
            last_preview,
        })
    }

    pub fn rename_ai_session(&self, id: &str, title: &str) -> Result<AiSessionDto> {
        let clean_id = Self::validate_ai_session_id(id)?;
        let clean_title = Self::normalize_ai_single_line(title);
        if clean_title.is_empty() {
            bail!("Tên cuộc trò chuyện AI không được để trống");
        }
        if clean_title.chars().count() > AI_SESSION_MAX_TITLE_CHARS {
            bail!("Tên cuộc trò chuyện AI tối đa {AI_SESSION_MAX_TITLE_CHARS} ký tự");
        }

        let mut existing: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT created_ts, updated_ts FROM ai_sessions WHERE id = ?1 LIMIT 1",
                params![clean_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if existing.is_none() {
            self.backfill_orphan_ai_sessions()?;
            existing = self
                .conn
                .query_row(
                    "SELECT created_ts, updated_ts FROM ai_sessions WHERE id = ?1 LIMIT 1",
                    params![clean_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
        }
        let Some((created_ts, updated_ts)) = existing else {
            bail!("Không tìm thấy cuộc trò chuyện AI: {clean_id}");
        };

        let enc_title = self
            .cipher
            .encrypt_str(&ai_session_aad(clean_id), &clean_title)?;
        self.conn.execute(
            "UPDATE ai_sessions SET title = ?1 WHERE id = ?2",
            params![enc_title, clean_id],
        )?;

        let (message_count, last_preview) = self.load_ai_session_stats(clean_id)?;
        Ok(AiSessionDto {
            id: clean_id.to_string(),
            title: clean_title,
            created_ts,
            updated_ts,
            message_count,
            last_preview,
        })
    }

    pub fn delete_ai_session(&self, id: &str) -> Result<usize> {
        let clean_id = id.trim();
        if clean_id.is_empty() {
            return Ok(0);
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let deleted_messages = tx.execute(
            "DELETE FROM ai_messages WHERE session_id = ?1",
            params![clean_id],
        )?;
        tx.execute("DELETE FROM ai_sessions WHERE id = ?1", params![clean_id])?;
        tx.commit()?;
        Ok(deleted_messages)
    }

    pub fn insert_ai_message(&self, msg: &AiStoredMessageDto) -> Result<()> {
        let id = msg.id.trim();
        if id.is_empty() {
            bail!("ID tin nhắn AI không được để trống");
        }
        let session_id = Self::validate_ai_session_id(&msg.session_id)?;
        let role = match msg.role.trim().to_ascii_lowercase().as_str() {
            "user" => "user",
            "model" | "assistant" => "model",
            other => bail!("Vai trò tin nhắn AI không hợp lệ: {other}"),
        };
        let ts = normalize_ts_ms(msg.ts);
        let enc_content = self.cipher.encrypt_str(&ai_msg_aad(id), &msg.content)?;

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR REPLACE INTO ai_messages (id, session_id, role, content, model, backend, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session_id, role, enc_content, msg.model, msg.backend, ts],
        )?;

        let existing_session: Option<(String, i64)> = tx
            .query_row(
                "SELECT title, updated_ts FROM ai_sessions WHERE id = ?1 LIMIT 1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        match existing_session {
            None => {
                let (min_ts, max_ts): (i64, i64) = tx.query_row(
                    "SELECT MIN(ts), MAX(ts) FROM ai_messages WHERE session_id = ?1",
                    params![session_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let created_ts = normalize_ts_ms(min_ts).min(ts);
                let updated_ts = normalize_ts_ms(max_ts).max(ts);
                let first_user_msg: Option<(String, String)> = tx
                    .query_row(
                        "SELECT id, content
                         FROM ai_messages
                         WHERE session_id = ?1 AND role = 'user'
                         ORDER BY ts ASC, id ASC
                         LIMIT 1",
                        params![session_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let auto_title = match first_user_msg {
                    Some((first_id, first_enc)) => {
                        let plain = self
                            .cipher
                            .decrypt_str(&ai_msg_aad(&first_id), &first_enc)?;
                        Self::derive_ai_session_auto_title(&plain, AI_SESSION_DEFAULT_TITLE)
                    }
                    None => AI_SESSION_DEFAULT_TITLE.to_string(),
                };
                let enc_title = self
                    .cipher
                    .encrypt_str(&ai_session_aad(session_id), &auto_title)?;
                tx.execute(
                    "INSERT INTO ai_sessions (id, title, created_ts, updated_ts)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![session_id, enc_title, created_ts, updated_ts],
                )?;
            }
            Some((enc_existing_title, existing_updated_ts)) => {
                let existing_title = self
                    .cipher
                    .decrypt_str(&ai_session_aad(session_id), &enc_existing_title)?;
                let next_updated_ts = existing_updated_ts.max(ts);
                let is_placeholder_title = {
                    let trimmed = existing_title.trim();
                    trimmed.is_empty() || trimmed == AI_SESSION_DEFAULT_TITLE
                };
                let upgraded_title = if is_placeholder_title && role == "user" {
                    let derived = Self::truncate_ai_text_with_ellipsis(
                        &msg.content,
                        AI_SESSION_AUTO_TITLE_CHARS,
                    );
                    if derived.is_empty() {
                        None
                    } else {
                        Some(derived)
                    }
                } else {
                    None
                };
                if let Some(new_title) = upgraded_title {
                    let enc_new_title = self
                        .cipher
                        .encrypt_str(&ai_session_aad(session_id), &new_title)?;
                    tx.execute(
                        "UPDATE ai_sessions SET title = ?1, updated_ts = ?2 WHERE id = ?3",
                        params![enc_new_title, next_updated_ts, session_id],
                    )?;
                } else if next_updated_ts != existing_updated_ts {
                    tx.execute(
                        "UPDATE ai_sessions SET updated_ts = ?1 WHERE id = ?2",
                        params![next_updated_ts, session_id],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn list_ai_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<AiStoredMessageDto>> {
        let clean_id = session_id.trim();
        if limit == 0 || clean_id.is_empty() {
            return Ok(Vec::new());
        }
        let safe_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, model, backend, ts
             FROM ai_messages
             WHERE session_id = ?1
             ORDER BY ts DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![clean_id, safe_limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, sid, role, enc_content, model, backend, ts) = row?;
            let content = self.cipher.decrypt_str(&ai_msg_aad(&id), &enc_content)?;
            out.push(AiStoredMessageDto {
                id,
                session_id: sid,
                role,
                content,
                model,
                backend,
                ts,
            });
        }
        out.reverse();
        Ok(out)
    }

    pub fn clear_ai_messages(&self, session_id: &str) -> Result<usize> {
        let clean_id = session_id.trim();
        if clean_id.is_empty() {
            return Ok(0);
        }
        let affected = self.conn.execute(
            "DELETE FROM ai_messages WHERE session_id = ?1",
            params![clean_id],
        )?;
        Ok(affected)
    }

    /// Fetches unread incoming (and interleaved context) messages in `thread_id` with
    /// `ts > threads.last_read_in_ts` (when `last_read_in_ts > 0`), capped to the latest
    /// `max_count` in chronological order. When `last_read_in_ts == 0`, or when
    /// `unread_count > 0` yields no incoming messages after `last_read_in_ts`, falls back to
    /// the latest `min(max(unread_count, 10), max_count)` incoming messages in chronological order.
    pub fn list_unread_messages_for_summary(
        &self,
        thread_id: &str,
        max_count: usize,
    ) -> Result<Vec<MessageDto>> {
        if max_count == 0 {
            return Ok(Vec::new());
        }
        let thread_meta: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT last_read_in_ts, unread_count FROM threads WHERE id = ?1 LIMIT 1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (last_read_in_ts, unread_count) = thread_meta
            .map(|(lr, uc)| (lr, uc.max(0) as usize))
            .unwrap_or((0, 0));

        if last_read_in_ts > 0 {
            let mut msgs = self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
                 FROM messages
                 WHERE thread_id = ?1 AND is_recalled = 0 AND ts > ?2
                 ORDER BY ts DESC, msg_id DESC
                 LIMIT ?3",
                params![thread_id, last_read_in_ts, max_count as i64],
            )?;
            if msgs.iter().any(|m| !m.is_outgoing) || unread_count == 0 {
                msgs.reverse();
                return Ok(msgs);
            }
        }

        let fallback_limit = unread_count.max(10).min(max_count) as i64;
        let mut incoming = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages
             WHERE thread_id = ?1 AND is_recalled = 0 AND is_outgoing = 0
             ORDER BY ts DESC, msg_id DESC
             LIMIT ?2",
            params![thread_id, fallback_limit],
        )?;
        incoming.reverse();
        Ok(incoming)
    }

    // -------------------------------------------------------------------------
    // Item 2: Scheduled Messages
    // -------------------------------------------------------------------------

    fn lookup_thread_title_and_kind(&self, thread_id: &str) -> Result<Option<(String, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT title, kind FROM threads WHERE id = ?1 LIMIT 1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((enc_title, kind)) = row else {
            return Ok(None);
        };
        let title = self
            .cipher
            .decrypt_or_legacy(&thread_aad(thread_id), &enc_title)
            .unwrap_or_else(|_| thread_id.to_string());
        Ok(Some((title, kind)))
    }

    const SCHEDULED_MSG_SELECT_COLS: &'static str = "id, thread_id, thread_title, thread_kind, text, attachment_path,
                    send_at_ts, recurrence, misfire_policy, misfire_grace_secs,
                    status, retry_count, last_error, created_at, updated_at, sent_at_ts,
                    next_attempt_ts, bypass_misfire, anchor_day";

    fn decode_scheduled_message_row(
        &self,
        row: &rusqlite::Row<'_>,
    ) -> Result<ScheduledMessageDto> {
        let id: String = row.get(0)?;
        let thread_id: String = row.get(1)?;
        let enc_thread_title: String = row.get(2)?;
        let thread_kind: String = row.get(3)?;
        let enc_text: String = row.get(4)?;
        let enc_attachment_path: String = row.get(5)?;
        let send_at_ts: i64 = row.get(6)?;
        let recurrence: String = row.get(7)?;
        let misfire_policy: String = row.get(8)?;
        let misfire_grace_secs: i64 = row.get(9)?;
        let status: String = row.get(10)?;
        let retry_count: i64 = row.get(11)?;
        let enc_last_error: String = row.get(12)?;
        let created_at: i64 = row.get(13)?;
        let updated_at: i64 = row.get(14)?;
        let sent_at_ts: i64 = row.get(15)?;
        let next_attempt_ts: Option<i64> = row.get(16)?;
        let bypass_misfire: i64 = row.get(17)?;
        let raw_anchor_day: Option<i64> = row.get(18)?;
        let anchor_day = raw_anchor_day
            .and_then(|d| u32::try_from(d).ok())
            .filter(|&d| (1..=31).contains(&d))
            .or_else(|| local_day_of_month(send_at_ts));

        let aad = sched_aad(&id);
        let thread_title = self.cipher.decrypt_str(&aad, &enc_thread_title)?;
        let text = self.cipher.decrypt_str(&aad, &enc_text)?;
        let attachment_path = self.cipher.decrypt_str(&aad, &enc_attachment_path)?;
        let last_error = self.cipher.decrypt_str(&aad, &enc_last_error)?;

        Ok(ScheduledMessageDto {
            id,
            thread_id,
            thread_title,
            thread_kind,
            text,
            attachment_path,
            send_at_ts,
            next_attempt_ts,
            bypass_misfire: bypass_misfire != 0,
            anchor_day,
            recurrence,
            misfire_policy,
            misfire_grace_secs,
            status,
            retry_count: retry_count.max(0) as u32,
            last_error,
            created_at,
            updated_at,
            sent_at_ts,
        })
    }

    pub fn create_scheduled_message(
        &self,
        input: &NewScheduledMessageInput,
    ) -> Result<ScheduledMessageDto> {
        let thread_id = input.thread_id.trim();
        if thread_id.is_empty() {
            bail!("thread_id không được để trống");
        }
        if input.text.trim().is_empty() && input.attachment_path.trim().is_empty() {
            bail!("Cần nhập nội dung tin nhắn hoặc tệp đính kèm");
        }
        let send_at_ts = normalize_ts_ms(input.send_at_ts);
        if send_at_ts <= 0 {
            bail!("Thời gian gửi hẹn giờ không hợp lệ");
        }
        let anchor_day = local_day_of_month(send_at_ts);

        let thread_meta = self.lookup_thread_title_and_kind(thread_id)?;
        let thread_title = if !input.thread_title.trim().is_empty() {
            input.thread_title.trim().to_string()
        } else if let Some((ref t, _)) = thread_meta {
            if t.trim().is_empty() {
                thread_id.to_string()
            } else {
                t.clone()
            }
        } else {
            thread_id.to_string()
        };
        let thread_kind = if input.thread_kind.trim().is_empty() {
            match thread_meta.as_ref().map(|(_, k)| k.as_str()) {
                Some("group") => "group",
                Some("cloud") => "cloud",
                _ => "direct",
            }
        } else {
            normalize_sched_thread_kind(&input.thread_kind)?
        };
        let recurrence = normalize_sched_recurrence(&input.recurrence)?;
        let misfire_policy = normalize_sched_misfire_policy(&input.misfire_policy)?;
        let misfire_grace_secs = input.misfire_grace_secs.max(0);

        let id = generate_prefixed_id("sched")?;
        let now_ms = Utc::now().timestamp_millis();
        let attachment_path = input.attachment_path.trim().to_string();
        let aad = sched_aad(&id);
        let enc_title = self.cipher.encrypt_str(&aad, &thread_title)?;
        let enc_text = self.cipher.encrypt_str(&aad, &input.text)?;
        let enc_att = self.cipher.encrypt_str(&aad, &attachment_path)?;
        let enc_err = self.cipher.encrypt_str(&aad, "")?;

        self.conn.execute(
            "INSERT INTO scheduled_messages (
                id, thread_id, thread_title, thread_kind, text, attachment_path,
                send_at_ts, next_attempt_ts, bypass_misfire, anchor_day,
                recurrence, misfire_policy, misfire_grace_secs,
                status, retry_count, last_error, created_at, updated_at, sent_at_ts
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0, ?8, ?9, ?10, ?11, 'pending', 0, ?12, ?13, ?13, 0)",
            params![
                id,
                thread_id,
                enc_title,
                thread_kind,
                enc_text,
                enc_att,
                send_at_ts,
                anchor_day.map(i64::from),
                recurrence,
                misfire_policy,
                misfire_grace_secs,
                enc_err,
                now_ms,
            ],
        )?;

        Ok(ScheduledMessageDto {
            id,
            thread_id: thread_id.to_string(),
            thread_title,
            thread_kind: thread_kind.to_string(),
            text: input.text.clone(),
            attachment_path,
            send_at_ts,
            next_attempt_ts: None,
            bypass_misfire: false,
            anchor_day,
            recurrence: recurrence.to_string(),
            misfire_policy: misfire_policy.to_string(),
            misfire_grace_secs,
            status: "pending".to_string(),
            retry_count: 0,
            last_error: String::new(),
            created_at: now_ms,
            updated_at: now_ms,
            sent_at_ts: 0,
        })
    }

    pub fn update_scheduled_message(
        &self,
        id: &str,
        input: &UpdateScheduledMessageInput,
    ) -> Result<Option<ScheduledMessageDto>> {
        let Some(mut cur) = self.get_scheduled_message(id)? else {
            return Ok(None);
        };
        if cur.status == "sending" {
            bail!("Không thể sửa tin nhắn hẹn giờ đang trong quá trình gửi");
        }
        if cur.status == "sent" {
            bail!("Không thể sửa tin nhắn hẹn giờ đã gửi");
        }

        if let Some(ref title) = input.thread_title {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                cur.thread_title = trimmed.to_string();
            }
        }
        if let Some(ref kind) = input.thread_kind {
            cur.thread_kind = normalize_sched_thread_kind(kind)?.to_string();
        }
        if let Some(ref text) = input.text {
            cur.text = text.clone();
        }
        if let Some(ref att) = input.attachment_path {
            cur.attachment_path = att.trim().to_string();
        }
        if cur.text.trim().is_empty() && cur.attachment_path.trim().is_empty() {
            bail!("Cần nhập nội dung tin nhắn hoặc tệp đính kèm");
        }
        let mut send_at_changed = false;
        let mut recurrence_changed = false;
        if let Some(raw_ts) = input.send_at_ts {
            let send_at_ts = normalize_ts_ms(raw_ts);
            if send_at_ts <= 0 {
                bail!("Thời gian gửi hẹn giờ không hợp lệ");
            }
            if send_at_ts != cur.send_at_ts {
                send_at_changed = true;
            }
            cur.send_at_ts = send_at_ts;
            cur.anchor_day = local_day_of_month(send_at_ts);
            if cur.status != "pending" {
                cur.status = "pending".to_string();
                cur.retry_count = 0;
                cur.last_error.clear();
                cur.next_attempt_ts = None;
                cur.bypass_misfire = false;
            }
        }
        if let Some(ref rec) = input.recurrence {
            let normalized = normalize_sched_recurrence(rec)?;
            if normalized != cur.recurrence {
                recurrence_changed = true;
            }
            cur.recurrence = normalized.to_string();
        }
        if cur.status == "pending" && (send_at_changed || recurrence_changed) {
            cur.retry_count = 0;
            cur.last_error.clear();
            cur.next_attempt_ts = None;
            cur.bypass_misfire = false;
        }
        if let Some(ref policy) = input.misfire_policy {
            cur.misfire_policy = normalize_sched_misfire_policy(policy)?.to_string();
        }
        if let Some(grace) = input.misfire_grace_secs {
            cur.misfire_grace_secs = grace.max(0);
        }

        cur.updated_at = Utc::now().timestamp_millis();
        let aad = sched_aad(&cur.id);
        let enc_title = self.cipher.encrypt_str(&aad, &cur.thread_title)?;
        let enc_text = self.cipher.encrypt_str(&aad, &cur.text)?;
        let enc_att = self.cipher.encrypt_str(&aad, &cur.attachment_path)?;
        let enc_err = self.cipher.encrypt_str(&aad, &cur.last_error)?;

        let affected = self.conn.execute(
            "UPDATE scheduled_messages
             SET thread_title = ?2,
                 thread_kind = ?3,
                 text = ?4,
                 attachment_path = ?5,
                 send_at_ts = ?6,
                 next_attempt_ts = ?7,
                 bypass_misfire = ?8,
                 anchor_day = ?9,
                 recurrence = ?10,
                 misfire_policy = ?11,
                 misfire_grace_secs = ?12,
                 status = ?13,
                 retry_count = ?14,
                 last_error = ?15,
                 updated_at = ?16
             WHERE id = ?1 AND status NOT IN ('sending', 'sent')",
            params![
                cur.id,
                enc_title,
                cur.thread_kind,
                enc_text,
                enc_att,
                cur.send_at_ts,
                cur.next_attempt_ts,
                cur.bypass_misfire as i64,
                cur.anchor_day.map(i64::from),
                cur.recurrence,
                cur.misfire_policy,
                cur.misfire_grace_secs,
                cur.status,
                i64::from(cur.retry_count),
                enc_err,
                cur.updated_at,
            ],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(cur))
    }

    pub fn list_scheduled_messages(
        &self,
        status_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ScheduledMessageDto>> {
        self.list_scheduled_messages_filtered(status_filter, None, limit)
    }

    /// Lists scheduled messages filtered by optional status CSV and optional `allowed_thread_ids`
    /// BEFORE applying `LIMIT`.
    ///
    /// Following [`ApiClientDto::allowed_thread_ids`] semantics, `None` or an empty slice means
    /// unrestricted (all threads); a non-empty slice restricts results via `thread_id IN (...)`.
    pub fn list_scheduled_messages_filtered(
        &self,
        status_filter: Option<&str>,
        allowed_thread_ids: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<ScheduledMessageDto>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let safe_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let statuses: Vec<&'static str> = match status_filter.map(str::trim) {
            None | Some("") | Some("all") => Vec::new(),
            Some(raw) => {
                let mut parsed = Vec::new();
                for part in raw.split(',') {
                    if let Some(st) = normalize_sched_status_token(part) {
                        if !parsed.contains(&st) {
                            parsed.push(st);
                        }
                    }
                }
                if parsed.is_empty() {
                    return Ok(Vec::new());
                }
                parsed
            }
        };
        let allowed_threads: Vec<String> = match allowed_thread_ids {
            Some(ids) => sanitize_allowed_thread_ids(ids),
            None => Vec::new(),
        };

        let mut sql = format!(
            "SELECT {} FROM scheduled_messages",
            Self::SCHEDULED_MSG_SELECT_COLS
        );
        let mut clauses: Vec<String> = Vec::new();
        let mut bound: Vec<rusqlite::types::Value> =
            Vec::with_capacity(statuses.len() + allowed_threads.len() + 1);
        if !statuses.is_empty() {
            let placeholders = vec!["?"; statuses.len()].join(", ");
            clauses.push(format!("status IN ({placeholders})"));
            for st in &statuses {
                bound.push(rusqlite::types::Value::Text((*st).to_string()));
            }
        }
        if !allowed_threads.is_empty() {
            let placeholders = vec!["?"; allowed_threads.len()].join(", ");
            clauses.push(format!("thread_id IN ({placeholders})"));
            for tid in &allowed_threads {
                bound.push(rusqlite::types::Value::Text(tid.clone()));
            }
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        let only_active = !statuses.is_empty()
            && statuses.iter().all(|s| matches!(*s, "pending" | "sending"));
        let only_history = !statuses.is_empty()
            && statuses
                .iter()
                .all(|s| !matches!(*s, "pending" | "sending"));
        if only_active {
            sql.push_str(
                " ORDER BY COALESCE(next_attempt_ts, send_at_ts) ASC, send_at_ts ASC, created_at ASC, id ASC LIMIT ?",
            );
        } else if only_history {
            sql.push_str(" ORDER BY updated_at DESC, send_at_ts DESC, id DESC LIMIT ?");
        } else {
            sql.push_str(
                " ORDER BY CASE WHEN status IN ('pending', 'sending') THEN 0 ELSE 1 END ASC,
                           CASE WHEN status IN ('pending', 'sending') THEN COALESCE(next_attempt_ts, send_at_ts) ELSE -updated_at END ASC,
                           id ASC
                  LIMIT ?",
            );
        }
        bound.push(rusqlite::types::Value::Integer(safe_limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(bound))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(self.decode_scheduled_message_row(row)?);
        }
        Ok(out)
    }

    pub fn get_scheduled_message(&self, id: &str) -> Result<Option<ScheduledMessageDto>> {
        let sql = format!(
            "SELECT {} FROM scheduled_messages WHERE id = ?1 LIMIT 1",
            Self::SCHEDULED_MSG_SELECT_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(self.decode_scheduled_message_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn cancel_scheduled_message(&self, id: &str) -> Result<bool> {
        let now_ms = Utc::now().timestamp_millis();
        let affected = self.conn.execute(
            "UPDATE scheduled_messages
             SET status = 'cancelled',
                 next_attempt_ts = NULL,
                 bypass_misfire = 0,
                 updated_at = ?2
             WHERE id = ?1 AND status IN ('pending', 'missed', 'failed')",
            params![id, now_ms],
        )?;
        Ok(affected > 0)
    }

    pub fn delete_scheduled_message(&self, id: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "DELETE FROM scheduled_messages WHERE id = ?1 AND status != 'sending'",
            params![id],
        )?;
        Ok(affected > 0)
    }

    pub fn next_pending_schedule_ts(&self) -> Result<Option<i64>> {
        let next_ts: Option<i64> = self.conn.query_row(
            "SELECT MIN(COALESCE(next_attempt_ts, send_at_ts))
             FROM scheduled_messages
             WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(next_ts)
    }

    /// Recovers crash-interrupted `'sending'` rows on startup: rows with `retry_count >= 2`
    /// transition to `'failed'`, while earlier attempts reset to `'pending'` with incremented
    /// `retry_count`.
    pub fn recover_stuck_sending_schedules(&self, now_ms: i64) -> Result<usize> {
        let now_ms = normalize_ts_ms(now_ms);
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<usize> {
            let stuck: Vec<(String, i64)> = {
                let mut stmt = self.conn.prepare(
                    "SELECT id, retry_count FROM scheduled_messages WHERE status = 'sending'",
                )?;
                let out = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                out
            };
            let mut recovered = 0usize;
            for (id, retry_count) in stuck {
                let aad = sched_aad(&id);
                let next_retry = retry_count.max(0).saturating_add(1);
                if retry_count >= 2 {
                    let enc_err = self.cipher.encrypt_str(
                        &aad,
                        "Gián đoạn khi đang gửi (ứng dụng bị đóng đột ngột)",
                    )?;
                    recovered += self.conn.execute(
                        "UPDATE scheduled_messages
                         SET status = 'failed',
                             next_attempt_ts = NULL,
                             bypass_misfire = 0,
                             retry_count = ?2,
                             last_error = ?3,
                             updated_at = ?4
                         WHERE id = ?1 AND status = 'sending'",
                        params![id, next_retry, enc_err, now_ms],
                    )?;
                } else {
                    let enc_err = self.cipher.encrypt_str(
                        &aad,
                        "Khôi phục sau khi ứng dụng bị gián đoạn",
                    )?;
                    recovered += self.conn.execute(
                        "UPDATE scheduled_messages
                         SET status = 'pending',
                             next_attempt_ts = NULL,
                             bypass_misfire = 0,
                             retry_count = ?2,
                             last_error = ?3,
                             updated_at = ?4
                         WHERE id = ?1 AND status = 'sending'",
                        params![id, next_retry, enc_err, now_ms],
                    )?;
                }
            }
            Ok(recovered)
        })();
        match res {
            Ok(count) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(count)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Atomically claims up to `max_batch` due `'pending'` rows
    /// (`COALESCE(next_attempt_ts, send_at_ts) <= now_ms`) by marking them `'sending'` inside
    /// an `IMMEDIATE` transaction so double-send is impossible.
    pub fn claim_due_scheduled_messages(
        &self,
        now_ms: i64,
        max_batch: usize,
    ) -> Result<Vec<ScheduledMessageDto>> {
        if max_batch == 0 {
            return Ok(Vec::new());
        }
        let now_ms = normalize_ts_ms(now_ms);
        let safe_limit = i64::try_from(max_batch).unwrap_or(i64::MAX);
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<Vec<ScheduledMessageDto>> {
            let candidates = {
                let sql = format!(
                    "SELECT {} FROM scheduled_messages
                     WHERE status = 'pending' AND COALESCE(next_attempt_ts, send_at_ts) <= ?1
                     ORDER BY COALESCE(next_attempt_ts, send_at_ts) ASC, id ASC
                     LIMIT ?2",
                    Self::SCHEDULED_MSG_SELECT_COLS
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let mut rows = stmt.query(params![now_ms, safe_limit])?;
                let mut items = Vec::new();
                while let Some(row) = rows.next()? {
                    items.push(self.decode_scheduled_message_row(row)?);
                }
                items
            };

            let mut claimed = Vec::with_capacity(candidates.len());
            for mut item in candidates {
                let affected = self.conn.execute(
                    "UPDATE scheduled_messages
                     SET status = 'sending', updated_at = ?2
                     WHERE id = ?1 AND status = 'pending'",
                    params![item.id, now_ms],
                )?;
                if affected == 1 {
                    item.status = "sending".to_string();
                    item.updated_at = now_ms;
                    claimed.push(item);
                }
            }
            Ok(claimed)
        })();
        match res {
            Ok(claimed) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(claimed)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Marks a scheduled message as completed. For recurring schedules (`"daily"`, `"weekly"`,
    /// `"monthly"`), computes the next occurrence from canonical `send_at_ts` (preserving
    /// `anchor_day`), clears `next_attempt_ts` / `bypass_misfire`, and resets
    /// `status = 'pending'`, `retry_count = 0`, `last_error = ''`, `sent_at_ts = now_ms`.
    /// For `"once"`, transitions to `status = 'sent'`.
    pub fn complete_scheduled_message(
        &self,
        id: &str,
        now_ms: i64,
    ) -> Result<Option<ScheduledMessageDto>> {
        let now_ms = normalize_ts_ms(now_ms);
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<Option<ScheduledMessageDto>> {
            let Some(mut cur) = self.get_scheduled_message(id)? else {
                return Ok(None);
            };
            let enc_empty_err = self.cipher.encrypt_str(&sched_aad(&cur.id), "")?;
            cur.last_error.clear();
            cur.updated_at = now_ms;
            cur.sent_at_ts = now_ms;
            cur.next_attempt_ts = None;
            cur.bypass_misfire = false;
            let anchor_day = cur
                .anchor_day
                .or_else(|| local_day_of_month(cur.send_at_ts));
            cur.anchor_day = anchor_day;

            if let Some(next_ts) = compute_next_occurrence_ts_with_anchor(
                cur.send_at_ts,
                anchor_day,
                &cur.recurrence,
                now_ms,
            ) {
                cur.send_at_ts = next_ts;
                cur.status = "pending".to_string();
                cur.retry_count = 0;
                self.conn.execute(
                    "UPDATE scheduled_messages
                     SET status = 'pending',
                         send_at_ts = ?2,
                         next_attempt_ts = NULL,
                         bypass_misfire = 0,
                         anchor_day = ?3,
                         retry_count = 0,
                         last_error = ?4,
                         updated_at = ?5,
                         sent_at_ts = ?5
                     WHERE id = ?1",
                    params![
                        cur.id,
                        cur.send_at_ts,
                        anchor_day.map(i64::from),
                        enc_empty_err,
                        now_ms
                    ],
                )?;
            } else {
                cur.status = "sent".to_string();
                self.conn.execute(
                    "UPDATE scheduled_messages
                     SET status = 'sent',
                         next_attempt_ts = NULL,
                         bypass_misfire = 0,
                         last_error = ?2,
                         updated_at = ?3,
                         sent_at_ts = ?3
                     WHERE id = ?1",
                    params![cur.id, enc_empty_err, now_ms],
                )?;
            }
            Ok(Some(cur))
        })();
        match res {
            Ok(out) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Records a delivery failure. When `next_retry_ts` is `Some(retry_ts)`, requeues the
    /// item by setting `next_attempt_ts = retry_ts` WITHOUT overwriting canonical `send_at_ts`.
    /// When `next_retry_ts` is `None`, marks the schedule permanently `'failed'`.
    pub fn fail_or_retry_scheduled_message(
        &self,
        id: &str,
        error: &str,
        next_retry_ts: Option<i64>,
        now_ms: i64,
    ) -> Result<Option<ScheduledMessageDto>> {
        let now_ms = normalize_ts_ms(now_ms);
        let Some(mut cur) = self.get_scheduled_message(id)? else {
            return Ok(None);
        };
        let enc_err = self.cipher.encrypt_str(&sched_aad(&cur.id), error)?;
        cur.retry_count = cur.retry_count.saturating_add(1);
        cur.last_error = error.to_string();
        cur.updated_at = now_ms;

        if let Some(retry_ts) = next_retry_ts {
            let next_attempt = normalize_ts_ms(retry_ts);
            cur.next_attempt_ts = Some(next_attempt);
            cur.status = "pending".to_string();
            self.conn.execute(
                "UPDATE scheduled_messages
                 SET status = 'pending',
                     next_attempt_ts = ?2,
                     retry_count = ?3,
                     last_error = ?4,
                     updated_at = ?5
                 WHERE id = ?1",
                params![
                    cur.id,
                    next_attempt,
                    i64::from(cur.retry_count),
                    enc_err,
                    now_ms
                ],
            )?;
        } else {
            cur.status = "failed".to_string();
            cur.next_attempt_ts = None;
            cur.bypass_misfire = false;
            self.conn.execute(
                "UPDATE scheduled_messages
                 SET status = 'failed',
                     next_attempt_ts = NULL,
                     bypass_misfire = 0,
                     retry_count = ?2,
                     last_error = ?3,
                     updated_at = ?4
                 WHERE id = ?1",
                params![cur.id, i64::from(cur.retry_count), enc_err, now_ms],
            )?;
        }
        Ok(Some(cur))
    }

    /// Reverts a `'sending'` schedule back to `'pending'` without incrementing `retry_count`
    /// and clears `next_attempt_ts` / `bypass_misfire` (used when `SessionError::Expired`
    /// interrupts delivery so `misfire_policy` evaluates cleanly after re-authentication).
    pub fn revert_scheduled_message_to_pending(
        &self,
        id: &str,
    ) -> Result<Option<ScheduledMessageDto>> {
        let now_ms = Utc::now().timestamp_millis();
        let affected = self.conn.execute(
            "UPDATE scheduled_messages
             SET status = 'pending',
                 next_attempt_ts = NULL,
                 bypass_misfire = 0,
                 updated_at = ?2
             WHERE id = ?1 AND status = 'sending'",
            params![id, now_ms],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        self.get_scheduled_message(id)
    }

    /// Marks a misfired schedule as `'missed'`. For recurring schedules configured with
    /// `misfire_policy == "skip"`, skips the missed occurrence and advances `send_at_ts`
    /// to the next future occurrence (preserving `anchor_day`) with `status = 'pending'`.
    pub fn mark_scheduled_message_missed(
        &self,
        id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<Option<ScheduledMessageDto>> {
        let now_ms = normalize_ts_ms(now_ms);
        let Some(mut cur) = self.get_scheduled_message(id)? else {
            return Ok(None);
        };
        let enc_reason = self.cipher.encrypt_str(&sched_aad(&cur.id), reason)?;
        cur.last_error = reason.to_string();
        cur.updated_at = now_ms;
        cur.next_attempt_ts = None;
        cur.bypass_misfire = false;
        let anchor_day = cur
            .anchor_day
            .or_else(|| local_day_of_month(cur.send_at_ts));
        cur.anchor_day = anchor_day;

        if cur.misfire_policy == "skip" {
            if let Some(next_ts) = compute_next_occurrence_ts_with_anchor(
                cur.send_at_ts,
                anchor_day,
                &cur.recurrence,
                now_ms,
            ) {
                cur.send_at_ts = next_ts;
                cur.status = "pending".to_string();
                cur.retry_count = 0;
                self.conn.execute(
                    "UPDATE scheduled_messages
                     SET status = 'pending',
                         send_at_ts = ?2,
                         next_attempt_ts = NULL,
                         bypass_misfire = 0,
                         anchor_day = ?3,
                         retry_count = 0,
                         last_error = ?4,
                         updated_at = ?5
                     WHERE id = ?1",
                    params![
                        cur.id,
                        cur.send_at_ts,
                        anchor_day.map(i64::from),
                        enc_reason,
                        now_ms
                    ],
                )?;
                return Ok(Some(cur));
            }
        }

        cur.status = "missed".to_string();
        self.conn.execute(
            "UPDATE scheduled_messages
             SET status = 'missed',
                 next_attempt_ts = NULL,
                 bypass_misfire = 0,
                 last_error = ?2,
                 updated_at = ?3
             WHERE id = ?1",
            params![cur.id, enc_reason, now_ms],
        )?;
        Ok(Some(cur))
    }

    /// Re-queues a missed/failed/cancelled/pending schedule for immediate sending by setting
    /// `next_attempt_ts = now_ms` and `bypass_misfire = true` WITHOUT overwriting canonical
    /// `send_at_ts` so recurring cadences never drift.
    pub fn reschedule_now(&self, id: &str, now_ms: i64) -> Result<Option<ScheduledMessageDto>> {
        let now_ms = normalize_ts_ms(now_ms);
        let Some(mut cur) = self.get_scheduled_message(id)? else {
            return Ok(None);
        };
        if cur.status == "sending" {
            bail!("Tin nhắn hẹn giờ này đang được gửi");
        }
        if cur.status == "sent" {
            bail!("Tin nhắn hẹn giờ này đã được gửi trước đó");
        }
        let enc_empty_err = self.cipher.encrypt_str(&sched_aad(&cur.id), "")?;
        cur.status = "pending".to_string();
        cur.next_attempt_ts = Some(now_ms);
        cur.bypass_misfire = true;
        cur.retry_count = 0;
        cur.last_error.clear();
        cur.updated_at = now_ms;

        let affected = self.conn.execute(
            "UPDATE scheduled_messages
             SET status = 'pending',
                 next_attempt_ts = ?2,
                 bypass_misfire = 1,
                 retry_count = 0,
                 last_error = ?3,
                 updated_at = ?2
             WHERE id = ?1 AND status NOT IN ('sending', 'sent')",
            params![cur.id, now_ms, enc_empty_err],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(cur))
    }

    // -------------------------------------------------------------------------
    // Item 3: Local Automation API Clients, Audit Logs & Idempotency
    // -------------------------------------------------------------------------

    fn decode_api_client_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiClientDto> {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        let token_prefix: String = row.get(2)?;
        let scopes_csv: String = row.get(3)?;
        let allowed_json: String = row.get(4)?;
        let rate_limit_per_min: i64 = row.get(5)?;
        let enabled: i64 = row.get(6)?;
        let created_at: i64 = row.get(7)?;
        let last_used_at: i64 = row.get(8)?;

        let scopes = scopes_csv
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let allowed_thread_ids: Vec<String> =
            serde_json::from_str(&allowed_json).unwrap_or_default();

        Ok(ApiClientDto {
            id,
            name,
            token_prefix,
            scopes,
            allowed_thread_ids,
            rate_limit_per_min: rate_limit_per_min.max(1) as u32,
            enabled: enabled != 0,
            created_at,
            last_used_at,
        })
    }

    fn get_api_client_by_id(&self, id: &str) -> Result<Option<ApiClientDto>> {
        let client = self
            .conn
            .query_row(
                "SELECT id, name, token_prefix, scopes, allowed_thread_ids_json,
                        rate_limit_per_min, enabled, created_at, last_used_at
                 FROM api_clients
                 WHERE id = ?1
                 LIMIT 1",
                params![id],
                Self::decode_api_client_row,
            )
            .optional()?;
        Ok(client)
    }

    pub fn create_api_client(
        &self,
        name: &str,
        scopes: &[String],
        allowed_thread_ids: &[String],
        rate_limit_per_min: u32,
    ) -> Result<CreatedApiClientDto> {
        let clean_name = name.trim();
        if clean_name.is_empty() {
            bail!("Tên ứng dụng API không được để trống");
        }
        if clean_name.chars().count() > 100 {
            bail!("Tên ứng dụng API tối đa 100 ký tự");
        }
        let clean_scopes = sanitize_api_scopes(scopes)?;
        let clean_allowed = sanitize_allowed_thread_ids(allowed_thread_ids);
        let rate_limit = sanitize_rate_limit_per_min(rate_limit_per_min);

        let id = generate_prefixed_id("client")?;
        let raw_token = generate_api_token()?;
        let token_hash = hash_api_token(&raw_token);
        let token_prefix: String = raw_token.chars().take(12).collect();
        let scopes_csv = clean_scopes.join(",");
        let allowed_json = serde_json::to_string(&clean_allowed)?;
        let now_ms = Utc::now().timestamp_millis();

        self.conn.execute(
            "INSERT INTO api_clients (
                id, name, token_hash, token_prefix, scopes, allowed_thread_ids_json,
                rate_limit_per_min, enabled, created_at, last_used_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, 0)",
            params![
                id,
                clean_name,
                token_hash,
                token_prefix,
                scopes_csv,
                allowed_json,
                i64::from(rate_limit),
                now_ms,
            ],
        )?;

        Ok(CreatedApiClientDto {
            client: ApiClientDto {
                id,
                name: clean_name.to_string(),
                token_prefix,
                scopes: clean_scopes,
                allowed_thread_ids: clean_allowed,
                rate_limit_per_min: rate_limit,
                enabled: true,
                created_at: now_ms,
                last_used_at: 0,
            },
            raw_token,
        })
    }

    pub fn list_api_clients(&self) -> Result<Vec<ApiClientDto>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, token_prefix, scopes, allowed_thread_ids_json,
                    rate_limit_per_min, enabled, created_at, last_used_at
             FROM api_clients
             ORDER BY created_at DESC, id DESC",
        )?;
        let out = stmt
            .query_map([], Self::decode_api_client_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(out)
    }

    pub fn update_api_client(
        &self,
        id: &str,
        enabled: Option<bool>,
        scopes: Option<&[String]>,
        allowed_thread_ids: Option<&[String]>,
        rate_limit_per_min: Option<u32>,
    ) -> Result<Option<ApiClientDto>> {
        let Some(mut cur) = self.get_api_client_by_id(id)? else {
            return Ok(None);
        };
        if let Some(v) = enabled {
            cur.enabled = v;
        }
        if let Some(sc) = scopes {
            cur.scopes = sanitize_api_scopes(sc)?;
        }
        if let Some(ids) = allowed_thread_ids {
            cur.allowed_thread_ids = sanitize_allowed_thread_ids(ids);
        }
        if let Some(rl) = rate_limit_per_min {
            cur.rate_limit_per_min = sanitize_rate_limit_per_min(rl);
        }

        let scopes_csv = cur.scopes.join(",");
        let allowed_json = serde_json::to_string(&cur.allowed_thread_ids)?;
        let affected = self.conn.execute(
            "UPDATE api_clients
             SET enabled = ?2,
                 scopes = ?3,
                 allowed_thread_ids_json = ?4,
                 rate_limit_per_min = ?5
             WHERE id = ?1",
            params![
                cur.id,
                cur.enabled as i64,
                scopes_csv,
                allowed_json,
                i64::from(cur.rate_limit_per_min),
            ],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(Some(cur))
    }

    pub fn delete_api_client(&self, id: &str) -> Result<bool> {
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<bool> {
            self.conn.execute(
                "DELETE FROM api_idempotency WHERE client_id = ?1",
                params![id],
            )?;
            let affected = self
                .conn
                .execute("DELETE FROM api_clients WHERE id = ?1", params![id])?;
            Ok(affected > 0)
        })();
        match res {
            Ok(deleted) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(deleted)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    /// Verifies a raw Bearer token via SHA-256 hash lookup + constant-time hash comparison,
    /// ensures `enabled == 1`, and updates `last_used_at`.
    pub fn verify_api_token(&self, raw_token: &str, now_ms: i64) -> Result<Option<ApiClientDto>> {
        if raw_token.is_empty() || raw_token != raw_token.trim() {
            return Ok(None);
        }
        let candidate_hash = hash_api_token(raw_token);
        let row: Option<(String, ApiClientDto)> = self
            .conn
            .query_row(
                "SELECT id, name, token_prefix, scopes, allowed_thread_ids_json,
                        rate_limit_per_min, enabled, created_at, last_used_at, token_hash
                 FROM api_clients
                 WHERE token_hash = ?1
                 LIMIT 1",
                params![&candidate_hash],
                |r| {
                    let stored_hash: String = r.get(9)?;
                    let dto = Self::decode_api_client_row(r)?;
                    Ok((stored_hash, dto))
                },
            )
            .optional()?;
        let Some((stored_hash, mut client)) = row else {
            return Ok(None);
        };
        if !constant_time_eq(stored_hash.as_bytes(), candidate_hash.as_bytes()) || !client.enabled {
            return Ok(None);
        }
        let now_ms = normalize_ts_ms(now_ms);
        let next_last_used = client.last_used_at.max(now_ms);
        self.conn.execute(
            "UPDATE api_clients SET last_used_at = MAX(last_used_at, ?2) WHERE id = ?1",
            params![&client.id, next_last_used],
        )?;
        client.last_used_at = next_last_used;
        Ok(Some(client))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_api_audit_log(
        &self,
        client_id: &str,
        client_name: &str,
        method: &str,
        path: &str,
        target_thread_id: &str,
        status_code: u16,
        outcome: &str,
        detail: &str,
        now_ms: i64,
    ) -> Result<ApiAuditLogDto> {
        let now_ms = normalize_ts_ms(now_ms);
        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<ApiAuditLogDto> {
            self.conn.execute(
                "INSERT INTO api_audit_logs (
                    client_id, client_name, method, path, target_thread_id,
                    status_code, outcome, detail, created_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8)",
                params![
                    client_id,
                    client_name,
                    method,
                    path,
                    target_thread_id,
                    i64::from(status_code),
                    outcome,
                    now_ms,
                ],
            )?;
            let id = self.conn.last_insert_rowid();
            let enc_detail = self.cipher.encrypt_str(&api_audit_aad(id), detail)?;
            self.conn.execute(
                "UPDATE api_audit_logs SET detail = ?2 WHERE id = ?1",
                params![id, enc_detail],
            )?;
            self.conn.execute(
                "DELETE FROM api_audit_logs
                 WHERE id NOT IN (
                     SELECT id FROM api_audit_logs
                     ORDER BY created_at DESC, id DESC
                     LIMIT ?1
                 )",
                params![API_AUDIT_LOG_MAX_ROWS],
            )?;
            Ok(ApiAuditLogDto {
                id,
                client_id: client_id.to_string(),
                client_name: client_name.to_string(),
                method: method.to_string(),
                path: path.to_string(),
                target_thread_id: target_thread_id.to_string(),
                status_code,
                outcome: outcome.to_string(),
                detail: detail.to_string(),
                created_at: now_ms,
            })
        })();
        match res {
            Ok(dto) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(dto)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn list_api_audit_logs(&self, limit: usize) -> Result<Vec<ApiAuditLogDto>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let safe_limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, client_id, client_name, method, path, target_thread_id,
                    status_code, outcome, detail, created_at
             FROM api_audit_logs
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![safe_limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                client_id,
                client_name,
                method,
                path,
                target_thread_id,
                status_code,
                outcome,
                enc_detail,
                created_at,
            ) = row?;
            let detail = self
                .cipher
                .decrypt_or_legacy(&api_audit_aad(id), &enc_detail)?;
            out.push(ApiAuditLogDto {
                id,
                client_id,
                client_name,
                method,
                path,
                target_thread_id,
                status_code: u16::try_from(status_code).unwrap_or(0),
                outcome,
                detail,
                created_at,
            });
        }
        Ok(out)
    }

    pub fn clear_api_audit_logs(&self) -> Result<usize> {
        let affected = self.conn.execute("DELETE FROM api_audit_logs", [])?;
        Ok(affected)
    }

    pub fn get_api_idempotency_record(
        &self,
        client_id: &str,
        idem_key: &str,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<Option<ApiIdempotencyRecord>> {
        if client_id.trim().is_empty() || idem_key.trim().is_empty() {
            return Ok(None);
        }
        let now_ms = normalize_ts_ms(now_ms);
        let row: Option<(i64, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT status_code, response_json, request_hash, created_at
                 FROM api_idempotency
                 WHERE client_id = ?1 AND idem_key = ?2
                 LIMIT 1",
                params![client_id, idem_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((status_code, enc_response, request_hash, created_at)) = row else {
            return Ok(None);
        };
        if ttl_ms <= 0 || now_ms.saturating_sub(created_at) > ttl_ms {
            let _ = self.conn.execute(
                "DELETE FROM api_idempotency WHERE client_id = ?1 AND idem_key = ?2",
                params![client_id, idem_key],
            );
            return Ok(None);
        }
        let response_json = self
            .cipher
            .decrypt_str(&api_idem_aad(client_id, idem_key), &enc_response)?;
        Ok(Some(ApiIdempotencyRecord {
            status_code: u16::try_from(status_code).unwrap_or(200),
            response_json,
            request_hash,
        }))
    }

    pub fn get_api_idempotency(
        &self,
        client_id: &str,
        idem_key: &str,
        ttl_ms: i64,
        now_ms: i64,
    ) -> Result<Option<(u16, String)>> {
        Ok(self
            .get_api_idempotency_record(client_id, idem_key, ttl_ms, now_ms)?
            .map(|r| (r.status_code, r.response_json)))
    }

    pub fn put_api_idempotency_with_hash(
        &self,
        client_id: &str,
        idem_key: &str,
        status_code: u16,
        response_json: &str,
        request_hash: &str,
        now_ms: i64,
    ) -> Result<()> {
        if client_id.trim().is_empty() || idem_key.trim().is_empty() {
            bail!("client_id và idem_key không được để trống");
        }
        if idem_key.len() > 128 {
            bail!("idem_key tối đa 128 ký tự");
        }
        let now_ms = normalize_ts_ms(now_ms);
        let enc_response = self
            .cipher
            .encrypt_str(&api_idem_aad(client_id, idem_key), response_json)?;
        let clean_hash = request_hash.trim();
        let prune_cutoff = now_ms.saturating_sub(API_IDEMPOTENCY_RETENTION_MS);

        self.conn.execute_batch("BEGIN IMMEDIATE;")?;
        let res = (|| -> Result<()> {
            self.conn.execute(
                "INSERT OR REPLACE INTO api_idempotency (
                    client_id, idem_key, status_code, response_json, request_hash, created_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    client_id,
                    idem_key,
                    i64::from(status_code),
                    enc_response,
                    clean_hash,
                    now_ms,
                ],
            )?;
            self.conn.execute(
                "DELETE FROM api_idempotency WHERE created_at < ?1",
                params![prune_cutoff],
            )?;
            Ok(())
        })();
        match res {
            Ok(()) => {
                self.conn.execute_batch("COMMIT;")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }

    pub fn put_api_idempotency(
        &self,
        client_id: &str,
        idem_key: &str,
        status_code: u16,
        response_json: &str,
        now_ms: i64,
    ) -> Result<()> {
        self.put_api_idempotency_with_hash(client_id, idem_key, status_code, response_json, "", now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts5_finds_vietnamese_messages_without_accents() {
        let dir = std::env::temp_dir().join(format!("litechat-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("history.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        store
            .upsert_message(&MessageDto {
                msg_id: "m1".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "Nguyễn Văn An".into(),
                text: "Chiều nay họp tổng kết dự án nhé!".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_100,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        let hits = store.search_messages("tong ket du an", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].msg_id, "m1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stores_encrypted_rows_on_disk_and_rejects_wrong_key() {
        let dir = std::env::temp_dir().join(format!("litechat-store-enc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("secret.db");
        {
            let mut store = MessageStore::open(&db_path).unwrap();
            store
                .upsert_thread(&ThreadDto {
                    id: "t1".into(),
                    title: "Nhóm Tài Chính Mật".into(),
                    avatar: "https://s.zalo.me/ava.jpg".into(),
                    kind: ThreadKind::Group,
                    last_preview: "Chuyển khoản ngay".into(),
                    last_ts: 1000,
                    unread_count: 1,
                    is_pinned: false,
                    is_muted: false,
                    last_read_in_ts: 0,
                    peer_read_ts: 0,
                    muted_until_ts: 0,
                    is_archived: false,
                })
                .unwrap();
            store
                .upsert_message(&MessageDto {
                    msg_id: "m1".into(),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "Lê Minh".into(),
                    text: "Mật khẩu phòng họp là 987654".into(),
                    msg_type: "chat.text".into(),
                    ts: 1000,
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

        // Verify raw SQLite file contains zero plaintext message/thread strings and no FTS table.
        {
            let raw = Connection::open(&db_path).unwrap();
            let has_fts: bool = raw
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'messages_fts')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!has_fts);

            let (raw_text, raw_norm, raw_sender): (String, String, String) = raw
                .query_row(
                    "SELECT text, text_norm, sender_name FROM messages WHERE msg_id = 'm1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert!(raw_text.starts_with(ENC_PREFIX));
            assert!(!raw_text.contains("987654"));
            assert!(raw_norm.is_empty());
            assert!(raw_sender.starts_with(ENC_PREFIX));

            let (raw_title, raw_title_norm, raw_preview): (String, String, String) = raw
                .query_row(
                    "SELECT title, title_norm, last_preview FROM threads WHERE id = 't1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert!(raw_title.starts_with(ENC_PREFIX));
            assert!(!raw_title.contains("Tài Chính"));
            assert!(raw_title_norm.is_empty());
            assert!(raw_preview.starts_with(ENC_PREFIX));
        }

        // Re-opening with valid DPAPI key succeeds and searches cleanly.
        {
            let store = MessageStore::open(&db_path).unwrap();
            let hits = store.search_messages("mat khau phong hop", 10).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].text, "Mật khẩu phòng họp là 987654");
            let threads = store.list_threads(FolderFilter::All).unwrap();
            assert_eq!(threads[0].title, "Nhóm Tài Chính Mật");
        }

        // Replacing the .key file with another random key must fail cleanly.
        let key_path = DbCipher::key_path_for_db(&db_path);
        let wrong_sealed = dpapi::protect_secret(&B64.encode([7u8; 32])).unwrap();
        data_dir::write_private_atomic(&key_path, wrong_sealed.as_bytes()).unwrap();
        assert!(MessageStore::open(&db_path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fts5_backfills_legacy_messages_on_migration() {
        let dir = std::env::temp_dir().join(format!("litechat-store-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let db_path = dir.join("legacy.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE messages (
                    msg_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    sender_id TEXT NOT NULL,
                    sender_name TEXT NOT NULL,
                    text TEXT NOT NULL,
                    text_norm TEXT NOT NULL,
                    msg_type TEXT NOT NULL DEFAULT 'chat.text',
                    ts INTEGER NOT NULL,
                    is_outgoing INTEGER NOT NULL DEFAULT 0,
                    is_recalled INTEGER NOT NULL DEFAULT 0,
                    is_pinned INTEGER NOT NULL DEFAULT 0,
                    quote_json TEXT,
                    reactions_json TEXT NOT NULL DEFAULT '[]',
                    attachment_url TEXT,
                    attachment_name TEXT,
                    PRIMARY KEY (thread_id, msg_id)
                );
                INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, ts)
                VALUES ('old1', 't1', 'u9', 'Trần Thị Bình', 'Báo cáo tài chính quý', 'bao cao tai chinh quy', 1000);
                "#,
            )
            .unwrap();
        }

        let store = MessageStore::open(&db_path).unwrap();
        let hits = store.search_messages("tai chinh", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].msg_id, "old1");

        let by_sender = store.search_messages("thi binh", 10).unwrap();
        assert_eq!(by_sender.len(), 1);
        assert_eq!(by_sender[0].msg_id, "old1");
        drop(store);

        // Confirm legacy plaintext row was encrypted on disk during migration.
        let raw = Connection::open(&db_path).unwrap();
        let raw_text: String = raw
            .query_row("SELECT text FROM messages WHERE msg_id = 'old1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(raw_text.starts_with(ENC_PREFIX));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_messages_paginates_and_loads_around_target() {
        let dir = std::env::temp_dir().join(format!("litechat-store-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("page.db");
        let mut store = MessageStore::open(&db_path).unwrap();
        for i in 1..=15_i64 {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("m{i}"),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: format!("Msg {i}"),
                    msg_type: "chat.text".into(),
                    ts: i * 1000,
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

        let latest = store.load_messages("t1", None, 5).unwrap();
        assert_eq!(latest.len(), 5);
        assert_eq!(latest[0].msg_id, "m11");
        assert_eq!(latest[4].msg_id, "m15");

        let older = store.load_messages("t1", Some(latest[0].ts), 5).unwrap();
        assert_eq!(older.len(), 5);
        assert_eq!(older[0].msg_id, "m6");
        assert_eq!(older[4].msg_id, "m10");

        let around = store.load_messages_around("t1", 3000, 2, 100).unwrap();
        assert_eq!(around[0].msg_id, "m1");
        assert!(around.iter().any(|m| m.msg_id == "m3"));
        assert_eq!(around.last().unwrap().msg_id, "m15");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reactions_toggle_switch_and_stay_idempotent_for_me() {
        let dir = std::env::temp_dir().join(format!("litechat-store-react-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("react.db");
        let mut store = MessageStore::open(&db_path).unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "m1".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Hi".into(),
                msg_type: "chat.text".into(),
                ts: 1000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // Remote user adds 👍
        let m = store
            .apply_reaction("t1", "m1", Some("👍"), false, false)
            .unwrap()
            .unwrap();
        assert_eq!(m.reactions.len(), 1);
        assert_eq!(m.reactions[0].count, 1);
        assert!(!m.reactions[0].me);

        // Toggle 👍 on for me -> count becomes 2, me = true
        let (m, removed) = store.toggle_reaction("t1", "m1", "👍").unwrap().unwrap();
        assert!(!removed);
        assert_eq!(m.reactions[0].count, 2);
        assert!(m.reactions[0].me);

        // WS echo of our own 👍 -> idempotent, stays count = 2
        let m = store
            .apply_reaction("t1", "m1", Some("👍"), true, false)
            .unwrap()
            .unwrap();
        assert_eq!(m.reactions[0].count, 2);

        // Switch reaction to ❤️ -> 👍 drops back to 1 (me=false), ❤️ becomes 1 (me=true)
        let (m, removed) = store.toggle_reaction("t1", "m1", "❤️").unwrap().unwrap();
        assert!(!removed);
        let like = m.reactions.iter().find(|r| r.emoji == "👍").unwrap();
        assert_eq!(like.count, 1);
        assert!(!like.me);
        let heart = m.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 1);
        assert!(heart.me);

        // Toggle ❤️ off -> ❤️ disappears cleanly
        let (m, removed) = store.toggle_reaction("t1", "m1", "❤️").unwrap().unwrap();
        assert!(removed);
        assert!(!m.reactions.iter().any(|r| r.emoji == "❤️"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn threads_sort_newest_message_first_and_auto_create_missing_preview() {
        let dir = std::env::temp_dir().join(format!("litechat-store-sort-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("sort.db");
        let store = MessageStore::open(&db_path).unwrap();

        store
            .upsert_thread(&ThreadDto {
                id: "pinned-old".into(),
                title: "Pinned Old".into(),
                avatar: String::new(),
                kind: ThreadKind::Cloud,
                last_preview: "old".into(),
                last_ts: 1_700_000_010_000,
                unread_count: 0,
                is_pinned: true,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .upsert_thread(&ThreadDto {
                id: "direct-sec".into(),
                title: "Direct Sec".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "sec".into(),
                last_ts: 1_700_000_050,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .update_thread_preview("stranger-new", "Tin moi nhat", 1_700_000_080_000, true)
            .unwrap();

        let threads = store.list_threads(FolderFilter::All).unwrap();
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        // Pinned stays first even when another thread is newer or unread
        assert_eq!(ids, vec!["pinned-old", "stranger-new", "direct-sec"]);
        assert_eq!(threads[2].last_ts, 1_700_000_050_000);
        assert_eq!(threads[1].unread_count, 1);

        // Among unpinned threads, newer message beats older unread message
        store
            .update_thread_preview("direct-sec", "Tin read moi hon", 1_700_000_085_000, false)
            .unwrap();
        let threads_after = store.list_threads(FolderFilter::All).unwrap();
        let ids_after: Vec<&str> = threads_after.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids_after, vec!["pinned-old", "direct-sec", "stranger-new"]);

        // After unpinning, pure last_ts DESC applies across all threads
        store.mark_thread_read("stranger-new").unwrap();
        store.set_thread_pinned("pinned-old", false).unwrap();
        let threads_unpinned = store.list_threads(FolderFilter::All).unwrap();
        let ids_unpinned: Vec<&str> = threads_unpinned.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids_unpinned, vec!["direct-sec", "stranger-new", "pinned-old"]);

        // An existing unread count is cleared when the newest synced/updated message is outgoing
        store.set_thread_unread("stranger-new", 2).unwrap();
        store
            .update_thread_preview_with_unread("stranger-new", "Bạn: Đã phản hồi", 1_700_000_090_000, false, true)
            .unwrap();
        let cleared = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(
            cleared.iter().find(|t| t.id == "stranger-new").unwrap().unread_count,
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn message_pin_persists_in_store() {
        let dir = std::env::temp_dir().join(format!("litechat-store-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("pin.db");
        let mut store = MessageStore::open(&db_path).unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "pm1".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Thong bao quan trong".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_010_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        let pinned = store.set_message_pinned("t1", "pm1", true).unwrap().unwrap();
        assert!(pinned.is_pinned);
        let reloaded = store.get_message("t1", "pm1").unwrap().unwrap();
        assert!(reloaded.is_pinned);

        let unpinned = store.set_message_pinned("t1", "pm1", false).unwrap().unwrap();
        assert!(!unpinned.is_pinned);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_thread_preview_returning_created_reports_true_only_on_first_insert() {
        let dir = std::env::temp_dir().join(format!("litechat-store-created-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("created.db");
        let store = MessageStore::open(&db_path).unwrap();

        let created_first = store
            .update_thread_preview_returning_created("t-new", "Xin chao", 1_700_000_001_000, true, false)
            .unwrap();
        assert!(created_first);

        let created_second = store
            .update_thread_preview_returning_created("t-new", "Tin tiep theo", 1_700_000_002_000, true, false)
            .unwrap();
        assert!(!created_second);

        let listed = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].unread_count, 2);
        assert_eq!(listed[0].last_preview, "Tin tiep theo");

        let created_clear = store
            .update_thread_preview_returning_created("t-new", "Bạn: Đã xem", 1_700_000_003_000, false, true)
            .unwrap();
        assert!(!created_clear);
        let after_clear = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_clear[0].unread_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recall_clears_attachment_and_resync_preserves_local_pin() {
        let dir = std::env::temp_dir().join(format!("litechat-store-recall-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("recall_pin.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let original = MessageDto {
            msg_id: "rm1".into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "An".into(),
            text: "Anh dinh kem".into(),
            msg_type: "chat.photo".into(),
            ts: 1_700_000_010_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: Some("https://f10-zpc.zdn.vn/pic.jpg".into()),
            attachment_name: Some("pic.jpg".into()),
        };
        store.upsert_message(&original).unwrap();
        store.set_message_pinned("t1", "rm1", true).unwrap();

        // Simulate remote history sync arriving without local is_pinned flag
        store.upsert_message(&original).unwrap();
        let after_resync = store.get_message("t1", "rm1").unwrap().unwrap();
        assert!(after_resync.is_pinned);

        // Explicit unpin still works
        store.set_message_pinned("t1", "rm1", false).unwrap();
        assert!(!store.get_message("t1", "rm1").unwrap().unwrap().is_pinned);

        // Recall must strip old attachment_url & attachment_name in SQLite
        let recalled = store.mark_recalled("t1", "rm1").unwrap().unwrap();
        assert!(recalled.is_recalled);
        assert!(recalled.attachment_url.is_none());
        assert!(recalled.attachment_name.is_none());
        let reloaded = store.get_message("t1", "rm1").unwrap().unwrap();
        assert!(reloaded.is_recalled);
        assert!(reloaded.attachment_url.is_none());
        assert!(reloaded.attachment_name.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_thread_synced_updates_remote_unpin_while_preserves_on_non_synced() {
        let dir = std::env::temp_dir().join(format!("litechat-store-sync-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("sync_pin.db");
        let store = MessageStore::open(&db_path).unwrap();

        let t = ThreadDto {
            id: "t1".into(),
            title: "Alice".into(),
            avatar: String::new(),
            kind: ThreadKind::Direct,
            last_preview: "hi".into(),
            last_ts: 1_700_000_010_000,
            unread_count: 0,
            is_pinned: true,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        };
        store.upsert_thread(&t).unwrap();
        assert!(store.list_threads(FolderFilter::All).unwrap()[0].is_pinned);

        // Non-synced upsert preserves existing pin
        let unpinned = ThreadDto { is_pinned: false, ..t.clone() };
        store.upsert_thread(&unpinned).unwrap();
        assert!(store.list_threads(FolderFilter::All).unwrap()[0].is_pinned);

        // Synced upsert applies authoritative remote unpin
        store.upsert_thread_synced(&unpinned).unwrap();
        assert!(!store.list_threads(FolderFilter::All).unwrap()[0].is_pinned);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_pinned_messages_returns_all_pinned_beyond_first_page() {
        let dir = std::env::temp_dir().join(format!("litechat-store-list-pins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("list_pins.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        for i in 0..70i64 {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("m{i:03}"),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: format!("Msg {i}"),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_000_000 + i * 1_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: i == 5 || i == 65,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                })
                .unwrap();
        }
        let pins = store.list_pinned_messages("t1", 50).unwrap();
        let ids: Vec<&str> = pins.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(ids, vec!["m005", "m065"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_thread_media_collects_files_even_after_many_photos_and_excludes_stickers() {
        let dir = std::env::temp_dir().join(format!("litechat-store-media-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("media_files.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        // Older file message
        store
            .upsert_message(&MessageDto {
                msg_id: "file-old".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Tập tin] spec.pdf".into(),
                msg_type: "share.file".into(),
                ts: 1_700_000_001_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://f.zdn.vn/spec.pdf".into()),
                attachment_name: Some("spec.pdf".into()),
            })
            .unwrap();

        // 15 newer photos and 5 stickers
        for i in 1..=15i64 {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("pic-{i:02}"),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: "[Hình ảnh]".into(),
                    msg_type: "chat.photo".into(),
                    ts: 1_700_000_010_000 + i * 1_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: Some(format!("https://f.zdn.vn/pic-{i}.jpg")),
                    attachment_name: None,
                })
                .unwrap();
        }
        for i in 1..=5i64 {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("stk-{i}"),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: "[Nhãn dán]".into(),
                    msg_type: "chat.sticker".into(),
                    ts: 1_700_000_050_000 + i * 1_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: Some(format!("https://f.zdn.vn/stk-{i}.webp")),
                    attachment_name: None,
                })
                .unwrap();
        }

        let (media, files) = store.list_thread_media("t1", 10).unwrap();
        assert_eq!(media.len(), 10);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].msg_id, "file-old");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_messages_before_handles_same_timestamp_tie_breaker_and_oldest_server_msg_id() {
        let dir = std::env::temp_dir().join(format!("litechat-store-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("cursor.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        for (id, ts) in [
            ("local-opt-1", 1_700_000_000_500),
            ("m1", 1_700_000_001_000),
            ("m2", 1_700_000_001_000),
            ("m3", 1_700_000_002_000),
        ] {
            store
                .upsert_message(&MessageDto {
                    msg_id: id.into(),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: id.into(),
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

        let page1 = store
            .load_messages_before("t1", Some(1_700_000_002_000), Some("m3"), 1)
            .unwrap();
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].msg_id, "m2");

        let page2 = store
            .load_messages_before("t1", Some(1_700_000_001_000), Some("m2"), 1)
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].msg_id, "m1");

        let oldest_srv = store.oldest_server_msg_id("t1").unwrap().unwrap();
        assert_eq!(oldest_srv, (1_700_000_001_000, "m1".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chat_folders_crud_and_ordering() {
        let dir = std::env::temp_dir().join(format!("teza-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MessageStore::open(&dir.join("folders.db")).unwrap();

        let f2 = ChatFolderDto {
            id: "f-work".into(),
            name: "Công việc".into(),
            icon: "briefcase".into(),
            color: "#CFA13A".into(),
            thread_ids: vec!["t1".into(), "t2".into()],
            excluded_thread_ids: Vec::new(),
            include_kinds: vec![ThreadKind::Group],
            only_unread: false,
            exclude_muted: true,
            position: 2,
            remote_label_id: None,
        };
        let f1 = ChatFolderDto {
            id: "f-vip".into(),
            name: "Khách hàng".into(),
            icon: "star".into(),
            color: String::new(),
            thread_ids: vec!["t9".into()],
            excluded_thread_ids: Vec::new(),
            include_kinds: Vec::new(),
            only_unread: true,
            exclude_muted: false,
            position: 1,
            remote_label_id: Some("lbl-1".into()),
        };

        store.save_chat_folder(&f2).unwrap();
        store.save_chat_folder(&f1).unwrap();
        let list = store.list_chat_folders().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], f1);
        assert_eq!(list[1], f2);

        let mut f1_updated = f1.clone();
        f1_updated.name = "Khách VIP".into();
        store.save_chat_folder(&f1_updated).unwrap();
        assert_eq!(store.list_chat_folders().unwrap()[0].name, "Khách VIP");

        assert!(store.delete_chat_folder("f-vip").unwrap());
        assert!(!store.delete_chat_folder("f-missing").unwrap());
        assert_eq!(store.list_chat_folders().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_thread_links_extracts_http_and_https_messages() {
        let dir = std::env::temp_dir().join(format!("teza-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("links.db")).unwrap();

        for (id, text, ts, recalled) in [
            ("m1", "Khong co link", 1_700_000_001_000, false),
            ("m2", "Trang cu http://intranet.local/doc", 1_700_000_002_000, false),
            ("m3", "Da thu hoi https://secret.example", 1_700_000_003_000, true),
            ("m4", "Xem tai https://teza.app/release", 1_700_000_004_000, false),
        ] {
            store
                .upsert_message(&MessageDto {
                    msg_id: id.into(),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "An".into(),
                    text: text.into(),
                    msg_type: "chat.text".into(),
                    ts,
                    is_outgoing: false,
                    is_recalled: recalled,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                })
                .unwrap();
        }

        let links = store.list_thread_links("t1", 20).unwrap();
        let ids: Vec<&str> = links.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(ids, vec!["m4", "m2"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thread_interaction_stats_and_recent_stickers_work() {
        let dir = std::env::temp_dir().join(format!("teza-stats-stk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("stats.db")).unwrap();

        for (id, thread_id, msg_type, url, name, ts) in [
            ("m1", "u-1", "chat.text", None, None, 1_700_000_001_000),
            (
                "m2",
                "u-1",
                "chat.photo",
                Some("https://f.zdn.vn/pic.jpg"),
                Some("pic.jpg"),
                1_700_000_002_000,
            ),
            (
                "s1",
                "u-1",
                "chat.sticker",
                Some("https://api.zaloapp.com/api/emoticon/sticker/webpc?eid=2005&size=130"),
                Some("sticker_12_2005.png"),
                1_700_000_003_000,
            ),
            (
                "s2",
                "grp-2",
                "chat.sticker",
                Some("https://stc-chat.zdn.vn/images/stickers/default/thumb/7.png"),
                None,
                1_700_000_004_000,
            ),
        ] {
            store
                .upsert_message(&MessageDto {
                    msg_id: id.into(),
                    cli_msg_id: None,
                    thread_id: thread_id.into(),
                    sender_id: "u-1".into(),
                    sender_name: "An".into(),
                    text: id.into(),
                    msg_type: msg_type.into(),
                    ts,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: url.map(str::to_string),
                    attachment_name: name.map(str::to_string),
                })
                .unwrap();
        }

        let (total, media, last_ts) = store.thread_interaction_stats("u-1").unwrap();
        assert_eq!(total, 3);
        assert_eq!(media, 1); // excludes sticker from media count
        assert_eq!(last_ts, 1_700_000_003_000);

        let stickers = store.list_recent_stickers(10).unwrap();
        assert_eq!(stickers.len(), 2);
        assert_eq!(stickers[0].0, 7);
        assert_eq!(stickers[1].0, 2005);
        assert_eq!(stickers[1].1, 12);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_watermarks_and_peer_seen_persist_correctly() {
        let dir = std::env::temp_dir().join(format!("teza-watermarks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("watermarks.db")).unwrap();

        store
            .upsert_thread(&ThreadDto {
                id: "dm-1".into(),
                title: "Minh".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Ping".into(),
                last_ts: 1_700_000_030_000,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 1_700_000_010_000,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();

        store
            .upsert_message(&MessageDto {
                msg_id: "m1".into(),
                cli_msg_id: None,
                thread_id: "dm-1".into(),
                sender_id: "dm-1".into(),
                sender_name: "Minh".into(),
                text: "Msg 1".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_010_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "m2".into(),
                cli_msg_id: None,
                thread_id: "dm-1".into(),
                sender_id: "me".into(),
                sender_name: "Bạn".into(),
                text: "Msg 2".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_020_000,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "m3".into(),
                cli_msg_id: None,
                thread_id: "dm-1".into(),
                sender_id: "dm-1".into(),
                sender_name: "Minh".into(),
                text: "Msg 3".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_030_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // Peer reply advances peer_read_ts to 1_700_000_030_000
        let listed = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(listed[0].peer_read_ts, 1_700_000_030_000);
        assert_eq!(listed[0].last_read_in_ts, 1_700_000_010_000);

        let latest_in = store.get_latest_incoming_message("dm-1").unwrap().unwrap();
        assert_eq!(latest_in.0, "m3");
        assert_eq!(latest_in.1, "dm-1");
        assert_eq!(latest_in.2, 1_700_000_030_000);

        // Mark read advances last_read_in_ts to latest incoming message ts
        store.mark_thread_read("dm-1").unwrap();
        let after_read = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_read[0].unread_count, 0);
        assert_eq!(after_read[0].last_read_in_ts, 1_700_000_030_000);

        // Mark unread rewinds last_read_in_ts below latest incoming ts
        store.set_thread_unread("dm-1", 1).unwrap();
        let after_unread = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_unread[0].unread_count, 1);
        assert_eq!(after_unread[0].last_read_in_ts, 1_700_000_029_999);

        // update_peer_read_ts advances peer_read_ts monotonically
        let p = store.update_peer_read_ts("dm-1", 1_700_000_050).unwrap();
        assert_eq!(p, 1_700_000_050_000);
        let kept = store.update_peer_read_ts("dm-1", 1_700_000_010_000).unwrap();
        assert_eq!(kept, 1_700_000_050_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_mute_persists_and_expires_in_list_threads() {
        let dir = std::env::temp_dir().join(format!("teza-timed-mute-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MessageStore::open(&dir.join("mute.db")).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        store
            .upsert_thread(&ThreadDto {
                id: "m-future".into(),
                title: "Muted Future".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "hi".into(),
                last_ts: now,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .upsert_thread(&ThreadDto {
                id: "m-expired".into(),
                title: "Muted Expired".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "hi".into(),
                last_ts: now - 1000,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();

        store
            .set_thread_muted_until("m-future", true, now + 3_600_000)
            .unwrap();
        store
            .set_thread_muted_until("m-expired", true, now - 60_000)
            .unwrap();

        let list = store.list_threads(FolderFilter::All).unwrap();
        let future = list.iter().find(|t| t.id == "m-future").unwrap();
        assert!(future.is_muted);
        assert!(future.muted_until_ts > now);

        let expired = list.iter().find(|t| t.id == "m-expired").unwrap();
        assert!(!expired.is_muted);
        assert_eq!(expired.muted_until_ts, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_and_folder_reorder_and_excluded_ids_persist() {
        let dir = std::env::temp_dir().join(format!("teza-g3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MessageStore::open(&dir.join("g3.db")).unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        store
            .upsert_thread(&ThreadDto {
                id: "arch-1".into(),
                title: "Archived Thread".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "old".into(),
                last_ts: now,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store.set_thread_archived("arch-1", true).unwrap();
        let list = store.list_threads(FolderFilter::All).unwrap();
        assert!(list.iter().find(|t| t.id == "arch-1").unwrap().is_archived);

        // Local upsert preserves archived flag
        store
            .upsert_thread(&ThreadDto {
                id: "arch-1".into(),
                title: "Archived Thread".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "new preview".into(),
                last_ts: now + 1000,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        let list = store.list_threads(FolderFilter::All).unwrap();
        assert!(list.iter().find(|t| t.id == "arch-1").unwrap().is_archived);

        store.set_thread_archived("arch-1", false).unwrap();
        let list = store.list_threads(FolderFilter::All).unwrap();
        assert!(!list.iter().find(|t| t.id == "arch-1").unwrap().is_archived);

        let f1 = ChatFolderDto {
            id: "f-a".into(),
            name: "Folder A".into(),
            icon: "folder".into(),
            color: String::new(),
            thread_ids: vec!["t1".into()],
            excluded_thread_ids: vec!["t2".into()],
            include_kinds: vec![ThreadKind::Direct],
            only_unread: false,
            exclude_muted: false,
            position: 1,
            remote_label_id: None,
        };
        let f2 = ChatFolderDto {
            id: "f-b".into(),
            name: "Folder B".into(),
            icon: "star".into(),
            color: String::new(),
            thread_ids: vec![],
            excluded_thread_ids: vec!["t3".into()],
            include_kinds: vec![ThreadKind::Group],
            only_unread: false,
            exclude_muted: false,
            position: 2,
            remote_label_id: None,
        };
        store.save_chat_folder(&f1).unwrap();
        store.save_chat_folder(&f2).unwrap();
        store
            .reorder_chat_folders(&["f-b".to_string(), "f-a".to_string()])
            .unwrap();
        let folders = store.list_chat_folders().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].id, "f-b");
        assert_eq!(folders[0].position, 1);
        assert_eq!(folders[0].excluded_thread_ids, vec!["t3".to_string()]);
        assert_eq!(folders[1].id, "f-a");
        assert_eq!(folders[1].position, 2);
        assert_eq!(folders[1].excluded_thread_ids, vec!["t2".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_thread_media_skips_stale_external_urls_and_link_resync_clears_them() {
        let dir = std::env::temp_dir().join(format!("teza-media-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("media_filter.db")).unwrap();

        // Legacy external link URL persisted in DB
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-link".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Xem bài viết".into(),
                msg_type: "chat.link".into(),
                ts: 1_700_000_001_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://vnexpress.net/bai-viet.html".into()),
                attachment_name: Some("VnExpress".into()),
            })
            .unwrap();

        // Legacy non-CDN image URL persisted in DB
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-ext-img".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Hình ảnh]".into(),
                msg_type: "chat.photo".into(),
                ts: 1_700_000_002_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://untrusted.example/photo.jpg".into()),
                attachment_name: Some("photo.jpg".into()),
            })
            .unwrap();

        // Legacy ecard & recommended rows whose href is on zalo.me must NOT leak into list_thread_media
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-ecard".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Thiệp chúc]".into(),
                msg_type: "chat.ecard".into(),
                ts: 1_700_000_002_500,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://chat.zalo.me".into()),
                attachment_name: Some("Thiệp".into()),
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-rec".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Danh thiếp]".into(),
                msg_type: "chat.recommended".into(),
                ts: 1_700_000_002_700,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://zalo.me/g/abc123".into()),
                attachment_name: Some("Nhóm".into()),
            })
            .unwrap();

        // Legacy http:// URL on trusted CDN should still appear and be upgraded to https://
        store
            .upsert_message(&MessageDto {
                msg_id: "legacy-http-file".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Tập tin] report.pdf".into(),
                msg_type: "chat.file".into(),
                ts: 1_700_000_003_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("http://files.zalo.cloud/report.pdf".into()),
                attachment_name: Some("report.pdf".into()),
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "dlfl-file".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Tập tin] Check.xlsx".into(),
                msg_type: "share.file".into(),
                ts: 1_700_000_004_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://file-stal-9.dlfl.vn/gr/test/Check.xlsx".into()),
                attachment_name: Some("Check.xlsx".into()),
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "dlmd-video".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Video]".into(),
                msg_type: "chat.video.msg".into(),
                ts: 1_700_000_005_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: Some("https://video-stal-20.dlmd.me/v/clip.mp4".into()),
                attachment_name: Some("clip.mp4".into()),
            })
            .unwrap();

        let (media, files) = store.list_thread_media("t1", 20).unwrap();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].msg_id, "dlmd-video");
        assert_eq!(
            media[0].attachment_url.as_deref(),
            Some("https://video-stal-20.dlmd.me/v/clip.mp4")
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].msg_id, "dlfl-file");
        assert_eq!(files[1].msg_id, "legacy-http-file");
        assert_eq!(
            files[1].attachment_url.as_deref(),
            Some("https://files.zalo.cloud/report.pdf")
        );

        // Resyncing the chat.link with attachment_url = None must clear the stale external URL
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-link".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Xem bài viết\nhttps://vnexpress.net/bai-viet.html".into(),
                msg_type: "chat.link".into(),
                ts: 1_700_000_001_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let reloaded = store.get_message("t1", "stale-link").unwrap().unwrap();
        assert!(reloaded.attachment_url.is_none());
        assert!(reloaded.attachment_name.is_none());

        // Resyncing legacy chat.ecard / chat.recommended with None clears stale zalo.me hrefs
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-ecard".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Thiệp chúc]".into(),
                msg_type: "chat.ecard".into(),
                ts: 1_700_000_002_500,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let reloaded_ecard = store.get_message("t1", "stale-ecard").unwrap().unwrap();
        assert!(reloaded_ecard.attachment_url.is_none());
        assert!(reloaded_ecard.attachment_name.is_none());

        store
            .upsert_message(&MessageDto {
                msg_id: "stale-rec".into(),
                cli_msg_id: None,
                thread_id: "t1".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "[Danh thiếp]".into(),
                msg_type: "chat.recommended".into(),
                ts: 1_700_000_002_700,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let reloaded_rec = store.get_message("t1", "stale-rec").unwrap().unwrap();
        assert!(reloaded_rec.attachment_url.is_none());
        assert!(reloaded_rec.attachment_name.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_thread_read_and_clear_unread_upsert_advance_watermark_past_stale_messages() {
        let dir = std::env::temp_dir().join(format!("teza-watermark-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("watermarks_sync.db")).unwrap();

        let yesterday = 1_700_000_000_000;
        let today = yesterday + 86_400_000;

        store
            .upsert_thread(&ThreadDto {
                id: "dm-stale".into(),
                title: "Lan".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Hôm qua".into(),
                last_ts: today,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: yesterday,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "old-in".into(),
                cli_msg_id: None,
                thread_id: "dm-stale".into(),
                sender_id: "dm-stale".into(),
                sender_name: "Lan".into(),
                text: "Tin cũ hôm qua".into(),
                msg_type: "chat.text".into(),
                ts: yesterday,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // mark_thread_read must advance last_read_in_ts to threads.last_ts (`today`), not stay on `yesterday`
        store.mark_thread_read("dm-stale").unwrap();
        let after_read = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_read[0].unread_count, 0);
        assert_eq!(after_read[0].last_read_in_ts, today);

        // Upsert with clear_unread (`unread_count == 0` and `"Bạn: ..."`) on both INSERT and ON CONFLICT UPDATE must pull `last_read_in_ts`
        let tomorrow = today + 86_400_000;
        let next_day = tomorrow + 86_400_000;
        store
            .upsert_thread(&ThreadDto {
                id: "dm-phone-insert".into(),
                title: "Lan 2".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Bạn: Ok nha".into(),
                last_ts: tomorrow,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .upsert_thread(&ThreadDto {
                id: "dm-phone".into(),
                title: "Hung".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Hôm qua".into(),
                last_ts: yesterday,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: yesterday,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        store
            .upsert_thread(&ThreadDto {
                id: "dm-phone".into(),
                title: "Hung".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Bạn: Ok nha".into(),
                last_ts: tomorrow,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        let listed = store.list_threads(FolderFilter::All).unwrap();
        let phone_insert = listed.iter().find(|t| t.id == "dm-phone-insert").unwrap();
        assert_eq!(phone_insert.last_read_in_ts, tomorrow);
        let phone = listed.iter().find(|t| t.id == "dm-phone").unwrap();
        assert_eq!(phone.unread_count, 0);
        assert_eq!(phone.last_read_in_ts, tomorrow);

        // advance_last_read_in_ts updates monotonically
        let later = next_day + 5_000;
        store.advance_last_read_in_ts("dm-phone", later).unwrap();
        store.advance_last_read_in_ts("dm-phone", yesterday).unwrap();
        let listed2 = store.list_threads(FolderFilter::All).unwrap();
        let phone2 = listed2.iter().find(|t| t.id == "dm-phone").unwrap();
        assert_eq!(phone2.last_read_in_ts, later);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_translations_encrypt_at_rest_invalidate_on_text_change_and_cleanup_on_delete() {
        let dir = std::env::temp_dir().join(format!("teza-tr-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("tr_cache.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let translated = TranslationResult {
            msg_id: "tr-1".into(),
            translated_text: "Cuộc họp bắt đầu lúc 9 giờ".into(),
            detected_source_lang: Some("EN".into()),
            skipped: false,
        };
        let skipped = TranslationResult {
            msg_id: "tr-2".into(),
            translated_text: "Xin chào mọi người".into(),
            detected_source_lang: Some("VI".into()),
            skipped: true,
        };
        let same_text_other_id = TranslationResult {
            msg_id: "tr-same".into(),
            translated_text: "Cuộc họp lúc 9h".into(),
            detected_source_lang: Some("EN".into()),
            skipped: false,
        };
        store
            .upsert_cached_translations(
                &[
                    ("tr-1".into(), "The meeting starts at 9am".into(), translated),
                    ("tr-2".into(), "Xin chào mọi người".into(), skipped),
                    ("tr-same".into(), "The meeting starts at 9am".into(), same_text_other_id),
                ],
                "VI",
                "deepl",
            )
            .unwrap();

        // Raw SQLite column must store ciphertext and keyed per-message MAC (never raw unkeyed MD5)
        let raw_conn = Connection::open(&db_path).unwrap();
        let (raw_text, hash1): (String, String) = raw_conn
            .query_row(
                "SELECT translated_text, text_hash FROM message_translations WHERE msg_id = 'tr-1' AND target_lang = 'VI'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let hash_same: String = raw_conn
            .query_row(
                "SELECT text_hash FROM message_translations WHERE msg_id = 'tr-same' AND target_lang = 'VI'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut raw_md5 = Md5::new();
        raw_md5.update(b"The meeting starts at 9am");
        let unkeyed_hex = raw_md5.finalize().iter().fold(String::with_capacity(32), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        assert!(raw_text.starts_with(ENC_PREFIX));
        assert!(!raw_text.contains("Cuộc họp"));
        assert_eq!(hash1.len(), 32);
        assert_ne!(hash1, unkeyed_hex);
        assert_ne!(hash1, hash_same);
        drop(raw_conn);

        // Lookup hits matching keyed MAC and recovers both translated and skipped entries
        let hits = store
            .get_cached_translations(
                &[
                    ("tr-1".into(), "The meeting starts at 9am".into()),
                    ("tr-2".into(), "Xin chào mọi người".into()),
                ],
                "vi",
            )
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits["tr-1"].translated_text, "Cuộc họp bắt đầu lúc 9 giờ");
        assert!(!hits["tr-1"].skipped);
        assert!(hits["tr-2"].skipped);
        assert_eq!(hits["tr-2"].detected_source_lang.as_deref(), Some("VI"));

        // Changed source text misses cache cleanly
        let changed = store
            .get_cached_translations(
                &[("tr-1".into(), "The meeting starts at 10am".into())],
                "VI",
            )
            .unwrap();
        assert!(changed.is_empty());

        // delete_message removes its cached translations too
        store
            .upsert_message(&MessageDto {
                msg_id: "tr-1".into(),
                cli_msg_id: None,
                thread_id: "t-cache".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "The meeting starts at 9am".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_000_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        assert!(store.delete_message("t-cache", "tr-1").unwrap());
        let after_del = store
            .get_cached_translations(&[("tr-1".into(), "The meeting starts at 9am".into())], "VI")
            .unwrap();
        assert!(after_del.is_empty());

        // clear_cached_translations(Some(&[]), ...) must be a no-op returning 0
        assert_eq!(store.clear_cached_translations(Some(&[]), None).unwrap(), 0);
        let still_there = store
            .get_cached_translations(&[("tr-2".into(), "Xin chào mọi người".into())], "VI")
            .unwrap();
        assert_eq!(still_there.len(), 1);

        // Recalling a message deletes its cached translations from disk
        store
            .upsert_message(&MessageDto {
                msg_id: "tr-2".into(),
                cli_msg_id: None,
                thread_id: "t-cache".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Xin chào mọi người".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_001_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        assert!(store.mark_recalled("t-cache", "tr-2").unwrap().is_some());
        let after_recall = store
            .get_cached_translations(&[("tr-2".into(), "Xin chào mọi người".into())], "VI")
            .unwrap();
        assert!(after_recall.is_empty());

        // clear_cached_translations removes remaining entries (tr-same)
        let cleared = store.clear_cached_translations(None, None).unwrap();
        assert_eq!(cleared, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_messages_skips_empty_quote_punctuation_tokens_and_clamps_limit() {
        let dir = std::env::temp_dir().join(format!("litechat-store-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("search.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        for idx in 0..205usize {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("sm-{idx}"),
                    cli_msg_id: None,
                    thread_id: "t-search".into(),
                    sender_id: "u1".into(),
                    sender_name: "Nguyễn Văn An".into(),
                    text: format!("Báo cáo tổng kết quý {idx}"),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_000 + idx as i64,
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

        // Empty, whitespace, quote-only, punctuation-only, and control-char queries must not fail FTS5
        for empty_query in [
            "",
            "   ",
            "\"\"",
            "\"\"\"\" \"\"",
            "--- *** ??? () []",
            "\0\r\n\t",
            "\"---\" \"...\"",
        ] {
            let hits = store.search_messages(empty_query, 20).unwrap();
            assert!(
                hits.is_empty(),
                "expected empty result for sanitized query {empty_query:?}"
            );
        }

        // Mixed quotes/punctuation around real tokens still match cleanly
        let mixed = store
            .search_messages("\"\" \"tong\" ??? ket! \"\"", 10)
            .unwrap();
        assert_eq!(mixed.len(), 10);

        // Limit clamps safely into 1..=200 (never errors or overflows i64)
        let min_clamped = store.search_messages("tong ket", 0).unwrap();
        assert_eq!(min_clamped.len(), 1);
        let max_clamped = store.search_messages("tong ket", 500).unwrap();
        assert_eq!(max_clamped.len(), 200);
        let huge_clamped = store.search_messages("tong ket", usize::MAX).unwrap();
        assert_eq!(huge_clamped.len(), 200);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_thread_synced_with_read_state_clears_offline_read_and_protects_newer_ws() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-sync-read-state-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("sync_read.db");
        let store = MessageStore::open(&db_path).unwrap();

        let initial = ThreadDto {
            id: "dm-1".into(),
            title: "Alice".into(),
            avatar: String::new(),
            kind: ThreadKind::Direct,
            last_preview: "Bạn có đó không?".into(),
            last_ts: 1_700_000_020_000,
            unread_count: 3,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 1_700_000_010_000,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        };
        store.upsert_thread(&initial).unwrap();

        // 1. Non-authoritative sync with unread_count=0 on incoming preview MUST preserve local unread
        let non_auth = ThreadDto {
            unread_count: 0,
            last_read_in_ts: 0,
            ..initial.clone()
        };
        store
            .upsert_thread_synced_with_read_state(&non_auth, true, false)
            .unwrap();
        let after_non_auth = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_non_auth[0].unread_count, 3);
        assert_eq!(after_non_auth[0].last_read_in_ts, 1_700_000_010_000);

        // 2. Stale authoritative snapshot (excluded.last_ts < threads.last_ts) MUST preserve newer local WS unread
        let stale_auth = ThreadDto {
            last_ts: 1_700_000_015_000,
            unread_count: 0,
            last_read_in_ts: 0,
            ..initial.clone()
        };
        store
            .upsert_thread_synced_with_read_state(&stale_auth, true, true)
            .unwrap();
        let after_stale = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_stale[0].unread_count, 3);
        assert_eq!(after_stale[0].last_ts, 1_700_000_020_000);
        assert_eq!(after_stale[0].last_read_in_ts, 1_700_000_010_000);

        // 3. Authoritative sync when phone read offline (excluded.last_ts >= threads.last_ts, unread=0)
        //    clears local unread_count to 0 and advances last_read_in_ts
        let phone_read = ThreadDto {
            last_ts: 1_700_000_025_000,
            unread_count: 0,
            last_read_in_ts: 0,
            ..initial.clone()
        };
        store
            .upsert_thread_synced_with_read_state(&phone_read, true, true)
            .unwrap();
        let after_phone_read = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_phone_read[0].unread_count, 0);
        assert_eq!(after_phone_read[0].last_ts, 1_700_000_025_000);
        assert_eq!(after_phone_read[0].last_read_in_ts, 1_700_000_025_000);

        // 4. Authoritative remote unread mark (unread_count=1) marks thread unread again
        let phone_marked_unread = ThreadDto {
            last_ts: 1_700_000_025_000,
            unread_count: 1,
            last_read_in_ts: 0,
            ..initial
        };
        store
            .upsert_thread_synced_with_read_state(&phone_marked_unread, true, true)
            .unwrap();
        let after_mark_unread = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_mark_unread[0].unread_count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_latest_incoming_message_with_type_returns_actual_msg_type_and_skips_outgoing() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-latest-in-type-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("latest_in_type.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        assert!(store
            .get_latest_incoming_message_with_type("t-in")
            .unwrap()
            .is_none());

        store
            .upsert_message(&MessageDto {
                msg_id: "m-out-only".into(),
                cli_msg_id: None,
                thread_id: "t-out".into(),
                sender_id: "me".into(),
                sender_name: "Bạn".into(),
                text: "out".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_001_000,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        assert!(store
            .get_latest_incoming_message_with_type("t-out")
            .unwrap()
            .is_none());

        store
            .upsert_message(&MessageDto {
                msg_id: "m1".into(),
                cli_msg_id: None,
                thread_id: "t-in".into(),
                sender_id: "peer-1".into(),
                sender_name: "Alice".into(),
                text: "[Hình ảnh]".into(),
                msg_type: "chat.photo".into(),
                ts: 1_700_000_010_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "m2".into(),
                cli_msg_id: None,
                thread_id: "t-in".into(),
                sender_id: "peer-2".into(),
                sender_name: "Bob".into(),
                text: "[Nhãn dán]".into(),
                msg_type: "chat.sticker".into(),
                ts: 1_700_000_020_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "m3".into(),
                cli_msg_id: None,
                thread_id: "t-in".into(),
                sender_id: "me".into(),
                sender_name: "Bạn".into(),
                text: "reply".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_030_000,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        let latest = store
            .get_latest_incoming_message_with_type("t-in")
            .unwrap()
            .unwrap();
        assert_eq!(
            latest,
            (
                "m2".to_string(),
                "peer-2".to_string(),
                "chat.sticker".to_string()
            )
        );

        // Empty msg_type normalizes to "chat.text"
        store
            .upsert_message(&MessageDto {
                msg_id: "m4".into(),
                cli_msg_id: None,
                thread_id: "t-in".into(),
                sender_id: "peer-3".into(),
                sender_name: "Carol".into(),
                text: "plain".into(),
                msg_type: "".into(),
                ts: 1_700_000_040_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let latest2 = store
            .get_latest_incoming_message_with_type("t-in")
            .unwrap()
            .unwrap();
        assert_eq!(
            latest2,
            (
                "m4".to_string(),
                "peer-3".to_string(),
                "chat.text".to_string()
            )
        );

        // `get_latest_incoming_message_for_seen` preserves `cli_msg_id` and `ts`
        store
            .upsert_message(&MessageDto {
                msg_id: "m5".into(),
                cli_msg_id: Some("1700000050123".into()),
                thread_id: "t-in".into(),
                sender_id: "peer-4".into(),
                sender_name: "Dave".into(),
                text: "with cli id".into(),
                msg_type: "chat.photo".into(),
                ts: 1_700_000_050_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let seen_meta = store
            .get_latest_incoming_message_for_seen("t-in")
            .unwrap()
            .unwrap();
        assert_eq!(
            seen_meta,
            (
                "m5".to_string(),
                Some("1700000050123".to_string()),
                "peer-4".to_string(),
                "chat.photo".to_string(),
                1_700_000_050_000,
            )
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn media_vault_schema_has_no_blobs_encrypts_paths_and_handles_crud_orphans_and_recalls() {
        let dir = std::env::temp_dir().join(format!("teza-store-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("media_idx.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        // Confirm zero BLOB columns in media_objects and message_media
        for table in ["media_objects", "message_media"] {
            let mut stmt = store
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let types: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(2))
                .unwrap()
                .map(|r| r.unwrap().to_ascii_uppercase())
                .collect();
            assert!(!types.is_empty(), "table {table} must exist");
            assert!(
                !types.iter().any(|t| t.contains("BLOB")),
                "table {table} must not have BLOB columns: {types:?}"
            );
        }

        // Insert thumb + main media + cloud-only evicted object
        let thumb_obj = MediaObjectRecord {
            sha256: "t".repeat(64),
            kind: "thumb".into(),
            mime: "image/webp".into(),
            size_bytes: 5_000,
            thumb_sha256: None,
            width: Some(320),
            height: Some(240),
            duration_ms: None,
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some(format!("thumbs/tt/tt/{}.enc", "t".repeat(64))),
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1000,
            last_accessed_ts: 1000,
            cloud_provider: None,
            cloud_file_id: None,
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: None,
        };
        let main_obj = MediaObjectRecord {
            sha256: "a".repeat(64),
            kind: "video".into(),
            mime: "video/mp4".into(),
            size_bytes: 200_000,
            thumb_sha256: Some(thumb_obj.sha256.clone()),
            width: Some(1920),
            height: Some(1080),
            duration_ms: Some(15_000),
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some(format!("blobs/aa/aa/{}.enc", "a".repeat(64))),
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1100,
            last_accessed_ts: 1100,
            cloud_provider: Some("gdrive".into()),
            cloud_file_id: Some("drv-file-1".into()),
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: Some(1200),
        };
        let voice_obj = MediaObjectRecord {
            sha256: "b".repeat(64),
            kind: "voice".into(),
            mime: "audio/aac".into(),
            size_bytes: 30_000,
            thumb_sha256: None,
            width: None,
            height: None,
            duration_ms: Some(4_500),
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some(format!("blobs/bb/bb/{}.enc", "b".repeat(64))),
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1200,
            last_accessed_ts: 1200,
            cloud_provider: None,
            cloud_file_id: None,
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: None,
        };
        store.upsert_media_object(&thumb_obj).unwrap();
        store.upsert_media_object(&main_obj).unwrap();
        store.upsert_media_object(&voice_obj).unwrap();

        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t1".into(),
                msg_id: "m1".into(),
                variant: "primary".into(),
                url_hash: "u1".into(),
                sha256: Some(main_obj.sha256.clone()),
                original_name: Some("Báo cáo mật 2026.mp4".into()),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 1500,
            })
            .unwrap();
        store
            .set_message_saved_path("t1", "m1", "E:\\Exports\\BaoCaoMat.mp4")
            .unwrap();
        assert_eq!(
            store.get_message_saved_path("t1", "m1").unwrap().as_deref(),
            Some("E:\\Exports\\BaoCaoMat.mp4")
        );

        // Verify raw SQLite stores ciphertext (no plaintext filename/user path on disk)
        {
            let raw = Connection::open(&db_path).unwrap();
            let (raw_name, raw_saved): (String, String) = raw
                .query_row(
                    "SELECT original_name, saved_user_path FROM message_media WHERE thread_id='t1' AND msg_id='m1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(raw_name.starts_with(ENC_PREFIX));
            assert!(!raw_name.contains("Báo cáo"));
            assert!(raw_saved.starts_with(ENC_PREFIX));
            assert!(!raw_saved.contains("BaoCaoMat"));
        }

        // Lookup by URL hash and by message
        assert_eq!(
            store.lookup_media_by_url_hash("u1").unwrap().unwrap().sha256,
            main_obj.sha256
        );
        assert_eq!(
            store.lookup_media_by_msg("t1", "m1", "primary").unwrap().unwrap().sha256,
            main_obj.sha256
        );

        // Pin voice_obj -> excluded from LRU candidates; thumb_obj is also excluded from LRU
        assert!(store.set_media_pin(&voice_obj.sha256, true).unwrap());
        let candidates = store.select_lru_eviction_candidates(10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].sha256, main_obj.sha256);

        // Stats accurately break down kinds, pinned, and cloud_synced
        let stats = store.compute_media_stats().unwrap();
        assert_eq!(stats.object_count, 3);
        assert_eq!(stats.total_bytes, 235_000);
        assert_eq!(stats.pinned_count, 1);
        assert_eq!(stats.pinned_bytes, 30_000);
        assert_eq!(stats.video_bytes, 200_000);
        assert_eq!(stats.voice_bytes, 30_000);
        assert_eq!(stats.thumb_bytes, 5_000);
        assert_eq!(stats.cloud_synced_count, 1);
        assert_eq!(stats.cloud_total_bytes, 200_000);

        // Touch & CDN expiry update
        store
            .batch_touch_media_access(&[(main_obj.sha256.clone(), 9_999)])
            .unwrap();
        assert_eq!(
            store.get_media_object(&main_obj.sha256).unwrap().unwrap().last_accessed_ts,
            9_999
        );
        assert_eq!(store.mark_media_cdn_status("u1", MEDIA_CDN_STATUS_EXPIRED).unwrap(), 1);
        assert_eq!(
            store
                .get_message_media_record("t1", "m1", "primary")
                .unwrap()
                .unwrap()
                .cdn_status,
            MEDIA_CDN_STATUS_EXPIRED
        );

        // Orphan cleanup: shared blob stays until last referencing message is removed; thumb also cleaned up
        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t1".into(),
                msg_id: "m2".into(),
                variant: "primary".into(),
                url_hash: "u2".into(),
                sha256: Some(main_obj.sha256.clone()),
                original_name: None,
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 1600,
            })
            .unwrap();
        let orphan_1 = store.cleanup_orphaned_media_for_msg("t1", "m1").unwrap();
        assert!(orphan_1.is_empty(), "m2 still references main_obj");

        let orphan_2 = store.cleanup_orphaned_media_for_msg("t1", "m2").unwrap();
        assert_eq!(orphan_2.len(), 2, "both main_obj and its thumb become orphaned");
        // main_obj had cloud_file_id so row is preserved as evicted (local_state=0); thumb had no cloud so deleted
        assert_eq!(
            store.get_media_object(&main_obj.sha256).unwrap().unwrap().local_state,
            MEDIA_LOCAL_STATE_MISSING
        );
        assert!(store.get_media_object(&thumb_obj.sha256).unwrap().is_none());

        // mark_recalled and delete_message remove message_media rows
        for mid in ["m_rec", "m_del"] {
            store
                .upsert_message(&MessageDto {
                    msg_id: mid.into(),
                    cli_msg_id: None,
                    thread_id: "t2".into(),
                    sender_id: "u1".into(),
                    sender_name: "Alice".into(),
                    text: "[Voice]".into(),
                    msg_type: "chat.voice".into(),
                    ts: 2000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: Some("https://f10-zpc.zdn.vn/v.aac".into()),
                    attachment_name: None,
                })
                .unwrap();
            store
                .link_message_media(&MessageMediaRecord {
                    thread_id: "t2".into(),
                    msg_id: mid.into(),
                    variant: "primary".into(),
                    url_hash: format!("uh_{mid}"),
                    sha256: Some(voice_obj.sha256.clone()),
                    original_name: None,
                    saved_user_path: None,
                    cdn_status: MEDIA_CDN_STATUS_OK,
                    updated_ts: 2000,
                })
                .unwrap();
        }
        store.mark_recalled("t2", "m_rec").unwrap();
        assert!(store.get_message_media_record("t2", "m_rec", "primary").unwrap().is_none());
        assert!(store.delete_message("t2", "m_del").unwrap());
        assert!(store.get_message_media_record("t2", "m_del", "primary").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_storage_manager_queries_breakdown_thread_usage_exclusive_prune_and_clear_links() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-cloud-mgr-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("cloud_mgr.db");
        let store = MessageStore::open(&db_path).unwrap();

        for (id, title) in [("t1", "Nhóm Thiết Kế"), ("t2", "Khách Hàng VIP")] {
            store
                .upsert_thread(&ThreadDto {
                    id: id.into(),
                    title: title.into(),
                    avatar: String::new(),
                    kind: ThreadKind::Group,
                    last_preview: "hi".into(),
                    last_ts: 1_700_000_000_000,
                    unread_count: 0,
                    is_pinned: false,
                    is_muted: false,
                    last_read_in_ts: 0,
                    peer_read_ts: 0,
                    muted_until_ts: 0,
                    is_archived: false,
                })
                .unwrap();
        }

        let sha_img = "1".repeat(64);
        let sha_vid = "2".repeat(64);
        let sha_shared = "3".repeat(64);
        let sha_pending = "4".repeat(64);
        let sha_thumb = "5".repeat(64);

        for (sha, kind, mime, size, local_state, rel, created_ts) in [
            (
                &sha_img,
                "image",
                "image/jpeg",
                100_000u64,
                MEDIA_LOCAL_STATE_LOCAL,
                Some(format!("blobs/11/11/{sha_img}.enc")),
                1_000i64,
            ),
            (
                &sha_vid,
                "video",
                "video/mp4",
                500_000u64,
                MEDIA_LOCAL_STATE_MISSING,
                None,
                2_000i64,
            ),
            (
                &sha_shared,
                "file",
                "application/pdf",
                250_000u64,
                MEDIA_LOCAL_STATE_LOCAL,
                Some(format!("blobs/33/33/{sha_shared}.enc")),
                3_000i64,
            ),
            (
                &sha_pending,
                "voice",
                "audio/aac",
                40_000u64,
                MEDIA_LOCAL_STATE_LOCAL,
                Some(format!("blobs/44/44/{sha_pending}.enc")),
                4_000i64,
            ),
            (
                &sha_thumb,
                "thumb",
                "image/webp",
                5_000u64,
                MEDIA_LOCAL_STATE_LOCAL,
                Some(format!("thumbs/55/55/{sha_thumb}.enc")),
                5_000i64,
            ),
        ] {
            store
                .upsert_media_object(&MediaObjectRecord {
                    sha256: sha.clone(),
                    kind: kind.into(),
                    mime: mime.into(),
                    size_bytes: size,
                    thumb_sha256: None,
                    width: None,
                    height: None,
                    duration_ms: None,
                    local_state,
                    local_rel_path: rel,
                    is_encrypted: true,
                    pin_count: 0,
                    created_ts,
                    last_accessed_ts: created_ts,
                    cloud_provider: None,
                    cloud_file_id: None,
                    cloud_encrypted: true,
                    cloud_key_ver: 1,
                    cloud_synced_ts: None,
                })
                .unwrap();
        }

        // Before marking synced: pending uploads include local non-thumbs only (excludes vid local_state=0 and thumb)
        let pending_before = store.list_pending_cloud_uploads(10).unwrap();
        let pending_shas: Vec<&str> = pending_before.iter().map(|o| o.sha256.as_str()).collect();
        assert_eq!(
            pending_shas,
            vec![sha_img.as_str(), sha_shared.as_str(), sha_pending.as_str()]
        );

        // Mark img (old), vid (mid), shared (new) as cloud synced
        store
            .mark_media_cloud_synced(&sha_img, "gdrive", "drv-img", 1, 10_000)
            .unwrap();
        store
            .mark_media_cloud_synced(&sha_vid, "gdrive", "drv-vid", 1, 20_000)
            .unwrap();
        store
            .mark_media_cloud_synced(&sha_shared, "gdrive", "drv-shared", 1, 30_000)
            .unwrap();

        // Pending now has only sha_pending
        let pending_after = store.list_pending_cloud_uploads(10).unwrap();
        assert_eq!(pending_after.len(), 1);
        assert_eq!(pending_after[0].sha256, sha_pending);

        // Link media to threads:
        // t1 has img (linked twice -> deduplicated!), vid, and shared
        // t2 has shared (so shared is NOT exclusive to t1!)
        for (tid, mid, uh, sha, name) in [
            ("t1", "m1", "u1", &sha_img, Some("anh_du_an.jpg")),
            ("t1", "m1b", "u1b", &sha_img, Some("anh_du_an_dup.jpg")),
            ("t1", "m2", "u2", &sha_vid, Some("demo_4k.mp4")),
            ("t1", "m3", "u3", &sha_shared, Some("hop_dong.pdf")),
            ("t2", "m4", "u4", &sha_shared, Some("hop_dong.pdf")),
        ] {
            store
                .link_message_media(&MessageMediaRecord {
                    thread_id: tid.into(),
                    msg_id: mid.into(),
                    variant: "primary".into(),
                    url_hash: uh.into(),
                    sha256: Some(sha.clone()),
                    original_name: name.map(str::to_string),
                    saved_user_path: None,
                    cdn_status: MEDIA_CDN_STATUS_OK,
                    updated_ts: 50_000,
                })
                .unwrap();
        }

        let bd = store.compute_cloud_media_breakdown().unwrap();
        assert_eq!(bd.total_bytes, 850_000);
        assert_eq!(bd.total_count, 3);
        assert_eq!(bd.images_bytes, 100_000);
        assert_eq!(bd.videos_bytes, 500_000);
        assert_eq!(bd.voice_bytes, 0);
        assert_eq!(bd.files_bytes, 250_000);
        assert_eq!(bd.pending_count, 1);
        assert_eq!(bd.pending_bytes, 40_000);

        let by_thread = store.list_cloud_usage_by_thread().unwrap();
        assert_eq!(by_thread.len(), 2);
        assert_eq!(by_thread[0].thread_id, "t1");
        assert_eq!(by_thread[0].thread_name, "Nhóm Thiết Kế");
        assert_eq!(by_thread[0].object_count, 3); // deduplicated sha_img!
        assert_eq!(by_thread[0].total_bytes, 850_000);
        assert_eq!(by_thread[1].thread_id, "t2");
        assert_eq!(by_thread[1].thread_name, "Khách Hàng VIP");
        assert_eq!(by_thread[1].object_count, 1);
        assert_eq!(by_thread[1].total_bytes, 250_000);

        let largest = store.list_largest_cloud_items(2).unwrap();
        assert_eq!(largest.len(), 2);
        assert_eq!(largest[0].sha256, sha_vid);
        assert_eq!(largest[0].cloud_file_id, "drv-vid");
        assert_eq!(largest[0].original_name.as_deref(), Some("demo_4k.mp4"));
        assert_eq!(largest[0].thread_name.as_deref(), Some("Nhóm Thiết Kế"));
        assert_eq!(largest[0].local_state, 0);
        assert_eq!(largest[1].sha256, sha_shared);
        assert_eq!(largest[1].local_state, 1);

        // Exclusive to t1 MUST skip sha_shared because t2 also references it!
        let excl_t1 = store.list_cloud_items_exclusive_to_thread("t1").unwrap();
        assert_eq!(
            excl_t1,
            vec![
                (sha_vid.clone(), "drv-vid".to_string(), 500_000),
                (sha_img.clone(), "drv-img".to_string(), 100_000),
            ]
        );
        assert!(store.list_cloud_items_exclusive_to_thread("t2").unwrap().is_empty());

        // Monotonic cloud_synced_ts ordering: img < vid < shared; older than shared_ts -> img & vid only!
        let ts_img = store
            .get_media_object(&sha_img)
            .unwrap()
            .unwrap()
            .cloud_synced_ts
            .unwrap();
        let ts_vid = store
            .get_media_object(&sha_vid)
            .unwrap()
            .unwrap()
            .cloud_synced_ts
            .unwrap();
        let ts_shared = store
            .get_media_object(&sha_shared)
            .unwrap()
            .unwrap()
            .cloud_synced_ts
            .unwrap();
        assert!(ts_img < ts_vid && ts_vid < ts_shared);
        let older = store.list_cloud_items_older_than(ts_shared).unwrap();
        assert_eq!(
            older,
            vec![
                (sha_img.clone(), "drv-img".to_string(), 100_000),
                (sha_vid.clone(), "drv-vid".to_string(), 500_000),
            ]
        );

        // Lookup by explicit sha256s deduplicates input and skips unsynced blobs
        let by_shas = store
            .list_cloud_items_by_sha256s(&[
                sha_img.clone(),
                sha_img.clone(),
                sha_pending.clone(),
            ])
            .unwrap();
        assert_eq!(by_shas, vec![(sha_img.clone(), "drv-img".to_string(), 100_000)]);

        // Clear cloud links preserves local_state & local_rel_path!
        let cleared = store
            .clear_media_cloud_links(std::slice::from_ref(&sha_img))
            .unwrap();
        assert_eq!(cleared, 1);
        let reloaded_img = store.get_media_object(&sha_img).unwrap().unwrap();
        assert!(reloaded_img.cloud_file_id.is_none());
        assert!(reloaded_img.cloud_provider.is_none());
        assert!(reloaded_img.cloud_synced_ts.is_none());
        assert_eq!(reloaded_img.local_state, MEDIA_LOCAL_STATE_LOCAL);
        assert!(reloaded_img.local_rel_path.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_delta_export_and_idempotent_import_preserve_state_and_avoid_regressions() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-delta-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = MessageStore::open(&dir.join("src.db")).unwrap();

        src.upsert_thread(&ThreadDto {
            id: "t1".into(),
            title: "Phòng Kỹ Thuật".into(),
            avatar: "https://s.zalo.me/t1.jpg".into(),
            kind: ThreadKind::Group,
            last_preview: "Đã xong bản build".into(),
            last_ts: 1_700_000_200_000,
            unread_count: 0,
            is_pinned: true,
            is_muted: false,
            last_read_in_ts: 1_700_000_200_000,
            peer_read_ts: 1_700_000_190_000,
            muted_until_ts: 0,
            is_archived: false,
        })
        .unwrap();

        src.save_chat_folder(&ChatFolderDto {
            id: "f-dev".into(),
            name: "Dev".into(),
            icon: "code".into(),
            color: "#3b82f6".into(),
            thread_ids: vec!["t1".into()],
            excluded_thread_ids: Vec::new(),
            include_kinds: vec![ThreadKind::Group],
            only_unread: false,
            exclude_muted: false,
            position: 1,
            remote_label_id: None,
        })
        .unwrap();

        src.upsert_message(&MessageDto {
            msg_id: "m1".into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "Minh".into(),
            text: "Deploy bản cập nhật nhé".into(),
            msg_type: "chat.text".into(),
            ts: 1_700_000_100_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: true,
            quote: None,
            reactions: vec![ReactionSummary {
                emoji: "👍".into(),
                count: 2,
                me: true,
            }],
            attachment_url: None,
            attachment_name: None,
        })
        .unwrap();

        src.upsert_cached_translations(
            &[(
                "m1".into(),
                "Deploy bản cập nhật nhé".into(),
                TranslationResult {
                    msg_id: "m1".into(),
                    translated_text: "Let's deploy the update".into(),
                    detected_source_lang: Some("VI".into()),
                    skipped: false,
                },
            )],
            "EN",
            "deepl",
        )
        .unwrap();

        let batch = src
            .export_history_delta_since("uid-1", "dev-A", 0, 500)
            .unwrap();
        assert_eq!(batch.version, HistoryDeltaBatch::CURRENT_VERSION);
        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 1);
        assert_eq!(batch.folders.len(), 1);
        assert_eq!(batch.thread_folders, vec![("f-dev".into(), "t1".into())]);
        assert_eq!(batch.translations.len(), 1);

        // Import into a brand-new DB (different DbCipher key!)
        let mut dst = MessageStore::open(&dir.join("dst.db")).unwrap();
        assert!(!dst.is_delta_applied("delta-file-1").unwrap());
        let first_applied = dst.import_history_delta_idempotent(&batch).unwrap();
        assert_eq!(first_applied, 1);
        dst.mark_delta_applied("delta-file-1", 1_700_000_300_000)
            .unwrap();
        assert!(dst.is_delta_applied("delta-file-1").unwrap());

        // Verify thread, folder, pinned message, FTS search, and translation all imported!
        let dst_threads = dst.list_threads(FolderFilter::All).unwrap();
        assert_eq!(dst_threads.len(), 1);
        assert_eq!(dst_threads[0].title, "Phòng Kỹ Thuật");
        assert!(dst_threads[0].is_pinned);
        assert_eq!(dst_threads[0].last_read_in_ts, 1_700_000_200_000);

        let dst_folders = dst.list_chat_folders().unwrap();
        assert_eq!(dst_folders.len(), 1);
        assert_eq!(dst_folders[0].thread_ids, vec!["t1".to_string()]);

        let dst_msg = dst.get_message("t1", "m1").unwrap().unwrap();
        assert!(dst_msg.is_pinned);
        assert_eq!(dst_msg.reactions.len(), 1);
        assert_eq!(dst.search_messages("ban cap nhat", 10).unwrap().len(), 1);

        let dst_tr = dst
            .get_cached_translations(&[("m1".into(), "Deploy bản cập nhật nhé".into())], "EN")
            .unwrap();
        assert_eq!(dst_tr["m1"].translated_text, "Let's deploy the update");

        // Re-applying identical delta is a clean 0-change no-op
        assert_eq!(dst.import_history_delta_idempotent(&batch).unwrap(), 0);

        // Local advances last_ts & recalls m1 -> re-applying older delta MUST NOT regress or un-recall!
        dst.update_thread_preview("t1", "Tin mới nhất cục bộ", 1_700_000_999_000, false)
            .unwrap();
        dst.mark_thread_read("t1").unwrap();
        dst.mark_recalled("t1", "m1").unwrap();

        assert_eq!(dst.import_history_delta_idempotent(&batch).unwrap(), 0);
        let after_old = dst.list_threads(FolderFilter::All).unwrap();
        assert_eq!(after_old[0].last_ts, 1_700_000_999_000);
        assert_eq!(after_old[0].last_read_in_ts, 1_700_000_999_000);
        assert_eq!(after_old[0].last_preview, "Tin mới nhất cục bộ");
        assert!(dst.get_message("t1", "m1").unwrap().unwrap().is_recalled);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_media_cloud_pruned_excludes_local_blob_from_pending_reupload() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-cloud-pruned-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MessageStore::open(&dir.join("pruned.db")).unwrap();

        let sha_keep = "c".repeat(64);
        let sha_prune = "d".repeat(64);
        for (sha, size, ts) in [(&sha_keep, 10_000u64, 1_000i64), (&sha_prune, 80_000u64, 2_000i64)]
        {
            store
                .upsert_media_object(&MediaObjectRecord {
                    sha256: sha.clone(),
                    kind: "video".into(),
                    mime: "video/mp4".into(),
                    size_bytes: size,
                    thumb_sha256: None,
                    width: None,
                    height: None,
                    duration_ms: None,
                    local_state: MEDIA_LOCAL_STATE_LOCAL,
                    local_rel_path: Some(format!("blobs/cc/cc/{sha}.enc")),
                    is_encrypted: true,
                    pin_count: 0,
                    created_ts: ts,
                    last_accessed_ts: ts,
                    cloud_provider: None,
                    cloud_file_id: None,
                    cloud_encrypted: true,
                    cloud_key_ver: 1,
                    cloud_synced_ts: None,
                })
                .unwrap();
        }
        store
            .mark_media_cloud_synced(&sha_prune, "gdrive", "drv-prune-1", 1, 5_000)
            .unwrap();

        // Before prune: sha_keep is pending (10_000), sha_prune is synced (80_000)
        let bd_before = store.compute_cloud_media_breakdown().unwrap();
        assert_eq!(bd_before.total_count, 1);
        assert_eq!(bd_before.pending_count, 1);
        assert_eq!(bd_before.pending_bytes, 10_000);

        // User prunes sha_prune via Cloud Storage Manager
        let pruned = store
            .mark_media_cloud_pruned(std::slice::from_ref(&sha_prune))
            .unwrap();
        assert_eq!(pruned, 1);

        // Local blob stays intact on disk, cloud link is gone, and it MUST NOT re-enter pending queue!
        let rec = store.get_media_object(&sha_prune).unwrap().unwrap();
        assert_eq!(rec.local_state, MEDIA_LOCAL_STATE_LOCAL);
        assert!(rec.local_rel_path.is_some());
        assert!(rec.cloud_file_id.is_none());

        let pending = store.list_pending_cloud_uploads(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sha256, sha_keep);

        let bd_after = store.compute_cloud_media_breakdown().unwrap();
        assert_eq!(bd_after.total_count, 0);
        assert_eq!(bd_after.pending_count, 1);
        assert_eq!(bd_after.pending_bytes, 10_000);

        // Explicit re-sync resets cloud_excluded = 0 cleanly
        store
            .mark_media_cloud_synced(&sha_prune, "gdrive", "drv-resync", 1, 9_000)
            .unwrap();
        // And non-prune clear_media_cloud_links (e.g., Drive 404) allows re-queueing again
        store
            .clear_media_cloud_links(std::slice::from_ref(&sha_prune))
            .unwrap();
        let pending_re = store.list_pending_cloud_uploads(10).unwrap();
        assert_eq!(pending_re.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_history_delta_includes_boundary_ties_and_paginates_without_loss() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-delta-page-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = MessageStore::open(&dir.join("src_page.db")).unwrap();

        // 10 messages across timestamps with 4 siblings sharing boundary ts = 2000!
        let items = [
            ("t1", "m01", 1_000i64),
            ("t1", "m02", 1_500),
            ("t1", "m03", 2_000),
            ("t1", "m04", 2_000),
            ("t2", "m04", 2_000), // same msg_id in different thread at identical ms!
            ("t2", "m05", 2_000),
            ("t1", "m06", 2_500),
            ("t1", "m07", 3_000),
            ("t1", "m08", 3_000),
            ("t1", "m09", 4_000),
        ];
        for (tid, mid, ts) in items {
            src.upsert_message(&MessageDto {
                msg_id: mid.into(),
                cli_msg_id: None,
                thread_id: tid.into(),
                sender_id: "u1".into(),
                sender_name: "Alice".into(),
                text: format!("msg {tid}/{mid}"),
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

        // Paginate with tiny limit_msgs = 3 -> page 1 hits boundary ts = 2000 and must include ALL 4 siblings!
        let dst = MessageStore::open(&dir.join("dst_page.db")).unwrap();
        let mut cursor = 0i64;
        let mut pages = 0usize;
        let mut total_imported = 0usize;
        loop {
            let batch = src
                .export_history_delta_since("u1", "dev-1", cursor, 3)
                .unwrap();
            pages += 1;
            if pages == 1 {
                // m01 (1000), m02 (1500), plus ALL 4 messages at boundary ts = 2000!
                assert_eq!(batch.messages.len(), 6);
                assert!(batch.messages_truncated);
                assert_eq!(batch.max_message_ts(), Some(2_000));
            }
            total_imported += dst.import_history_delta_idempotent(&batch).unwrap();
            if !batch.messages_truncated {
                break;
            }
            cursor = batch.max_message_ts().unwrap() + 1;
            assert!(pages < 10, "pagination must terminate");
        }
        assert!(pages >= 2);
        assert_eq!(total_imported, items.len());
        for (tid, mid, _) in items {
            assert!(
                dst.get_message(tid, mid).unwrap().is_some(),
                "missing {tid}/{mid}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_thread_cleans_messages_fts_translations_folders_and_orphans_without_touching_others() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-delete-thread-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("delete_thread.db")).unwrap();

        let make_thread = |id: &str, title: &str| ThreadDto {
            id: id.into(),
            title: title.into(),
            avatar: String::new(),
            kind: ThreadKind::Group,
            last_preview: String::new(),
            last_ts: 1_700_000_000_000,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        };
        store.upsert_thread(&make_thread("grp-del", "Nhóm Rời")).unwrap();
        store.upsert_thread(&make_thread("grp-keep", "Nhóm Giữ")).unwrap();

        let make_msg = |tid: &str, mid: &str, text: &str, ts: i64| MessageDto {
            msg_id: mid.into(),
            cli_msg_id: None,
            thread_id: tid.into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: text.into(),
            msg_type: "chat.text".into(),
            ts,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        };
        store.upsert_message(&make_msg("grp-del", "m-del-1", "Bí mật nhóm rời", 1_700_000_010_000)).unwrap();
        store.upsert_message(&make_msg("grp-del", "m-shared-id", "Tin trùng id", 1_700_000_015_000)).unwrap();
        store.upsert_message(&make_msg("grp-keep", "m-keep-1", "Thông tin nhóm giữ", 1_700_000_020_000)).unwrap();
        store.upsert_message(&make_msg("grp-keep", "m-shared-id", "Tin trùng id", 1_700_000_025_000)).unwrap();

        // Cached translations: exclusive to grp-del, exclusive to grp-keep, and shared msg_id
        store
            .upsert_cached_translations(
                &[
                    (
                        "m-del-1".into(),
                        "Bí mật nhóm rời".into(),
                        TranslationResult {
                            msg_id: "m-del-1".into(),
                            translated_text: "Secret deleted group".into(),
                            detected_source_lang: Some("VI".into()),
                            skipped: false,
                        },
                    ),
                    (
                        "m-keep-1".into(),
                        "Thông tin nhóm giữ".into(),
                        TranslationResult {
                            msg_id: "m-keep-1".into(),
                            translated_text: "Kept group info".into(),
                            detected_source_lang: Some("VI".into()),
                            skipped: false,
                        },
                    ),
                    (
                        "m-shared-id".into(),
                        "Tin trùng id".into(),
                        TranslationResult {
                            msg_id: "m-shared-id".into(),
                            translated_text: "Shared id msg".into(),
                            detected_source_lang: Some("VI".into()),
                            skipped: false,
                        },
                    ),
                ],
                "EN",
                "deepl",
            )
            .unwrap();

        // Media objects:
        // 1) local-only orphan with thumbnail (both must be deleted & returned)
        // 2) cloud-backed orphan (local_state -> 0, local_rel_path returned, cloud row kept)
        // 3) shared media referenced by BOTH grp-del and grp-keep (must stay intact!)
        let sha_orphan = "1".repeat(64);
        let sha_thumb = "2".repeat(64);
        let sha_cloud = "3".repeat(64);
        let sha_shared = "4".repeat(64);

        let make_media_obj = |sha: &str, kind: &str, rel: &str, thumb: Option<String>, cloud_id: Option<String>| {
            MediaObjectRecord {
                sha256: sha.into(),
                kind: kind.into(),
                mime: "image/jpeg".into(),
                size_bytes: 1024,
                thumb_sha256: thumb,
                width: None,
                height: None,
                duration_ms: None,
                local_state: MEDIA_LOCAL_STATE_LOCAL,
                local_rel_path: Some(rel.into()),
                is_encrypted: true,
                pin_count: 0,
                created_ts: 1_000,
                last_accessed_ts: 1_000,
                cloud_provider: cloud_id.as_ref().map(|_| "gdrive".into()),
                cloud_file_id: cloud_id,
                cloud_encrypted: true,
                cloud_key_ver: 1,
                cloud_synced_ts: None,
            }
        };

        store.upsert_media_object(&make_media_obj(&sha_thumb, "thumb", "blobs/thumb.enc", None, None)).unwrap();
        store.upsert_media_object(&make_media_obj(&sha_orphan, "image", "blobs/orphan.enc", Some(sha_thumb.clone()), None)).unwrap();
        store.upsert_media_object(&make_media_obj(&sha_cloud, "image", "blobs/cloud.enc", None, Some("drv-99".into()))).unwrap();
        store.upsert_media_object(&make_media_obj(&sha_shared, "image", "blobs/shared.enc", None, None)).unwrap();

        let make_link = |tid: &str, mid: &str, variant: &str, sha: &str| MessageMediaRecord {
            thread_id: tid.into(),
            msg_id: mid.into(),
            variant: variant.into(),
            url_hash: format!("uh-{tid}-{mid}-{variant}"),
            sha256: Some(sha.into()),
            original_name: None,
            saved_user_path: None,
            cdn_status: MEDIA_CDN_STATUS_OK,
            updated_ts: 2_000,
        };
        store.link_message_media(&make_link("grp-del", "m-del-1", "primary", &sha_orphan)).unwrap();
        store.link_message_media(&make_link("grp-del", "m-del-1", "cloud", &sha_cloud)).unwrap();
        store.link_message_media(&make_link("grp-del", "m-shared-id", "primary", &sha_shared)).unwrap();
        store.link_message_media(&make_link("grp-keep", "m-keep-1", "primary", &sha_shared)).unwrap();

        // Chat folder referencing both grp-del and grp-keep
        store
            .save_chat_folder(&ChatFolderDto {
                id: "f-work".into(),
                name: "Công việc".into(),
                icon: "folder".into(),
                color: String::new(),
                thread_ids: vec!["grp-del".into(), "grp-keep".into()],
                excluded_thread_ids: vec!["grp-del".into()],
                include_kinds: Vec::new(),
                only_unread: false,
                exclude_muted: false,
                position: 1,
                remote_label_id: None,
            })
            .unwrap();

        let mut orphans = store.delete_thread("grp-del").unwrap();
        orphans.sort();
        assert_eq!(
            orphans,
            vec!["blobs/cloud.enc", "blobs/orphan.enc", "blobs/thumb.enc"]
        );

        // Deleted thread & its messages & FTS entries are gone
        let remaining_threads = store.list_threads(FolderFilter::All).unwrap();
        assert_eq!(remaining_threads.len(), 1);
        assert_eq!(remaining_threads[0].id, "grp-keep");
        assert!(store.load_messages("grp-del", None, 50).unwrap().is_empty());
        assert!(store.search_messages("bi mat nhom roi", 10).unwrap().is_empty());

        // Kept thread's messages & FTS remain intact
        assert_eq!(store.load_messages("grp-keep", None, 50).unwrap().len(), 2);
        assert_eq!(store.search_messages("thong tin nhom giu", 10).unwrap().len(), 1);

        // Exclusive translation removed; kept & shared translations preserved
        let trs = store
            .get_cached_translations(
                &[
                    ("m-del-1".into(), "Bí mật nhóm rời".into()),
                    ("m-keep-1".into(), "Thông tin nhóm giữ".into()),
                    ("m-shared-id".into(), "Tin trùng id".into()),
                ],
                "EN",
            )
            .unwrap();
        assert!(!trs.contains_key("m-del-1"));
        assert_eq!(trs["m-keep-1"].translated_text, "Kept group info");
        assert_eq!(trs["m-shared-id"].translated_text, "Shared id msg");

        // Orphaned local-only media & thumb removed; cloud-backed evicted locally; shared preserved
        assert!(store.get_media_object(&sha_orphan).unwrap().is_none());
        assert!(store.get_media_object(&sha_thumb).unwrap().is_none());
        let cloud_after = store.get_media_object(&sha_cloud).unwrap().unwrap();
        assert_eq!(cloud_after.local_state, MEDIA_LOCAL_STATE_MISSING);
        assert!(cloud_after.local_rel_path.is_none());
        assert_eq!(cloud_after.cloud_file_id.as_deref(), Some("drv-99"));
        let shared_after = store.get_media_object(&sha_shared).unwrap().unwrap();
        assert_eq!(shared_after.local_state, MEDIA_LOCAL_STATE_LOCAL);
        assert_eq!(shared_after.local_rel_path.as_deref(), Some("blobs/shared.enc"));

        // Chat folder scrubbed of grp-del while retaining grp-keep
        let folders = store.list_chat_folders().unwrap();
        assert_eq!(folders[0].thread_ids, vec!["grp-keep"]);
        assert!(folders[0].excluded_thread_ids.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reactions_persist_full_removal_cli_msg_id_lookup_and_peer_actor_tracking() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-react-sync-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("react_sync.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        // Store message with distinct server msg_id, cli_msg_id, and ts
        store
            .upsert_message(&MessageDto {
                msg_id: "7970889894523".into(),
                cli_msg_id: Some("1782286269667".into()),
                thread_id: "7560225984834507848".into(),
                sender_id: "211235336".into(),
                sender_name: "Bạn".into(),
                text: "Tin nhan tu dien thoai".into(),
                msg_type: "chat.text".into(),
                ts: 1_782_286_279_518,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // Re-upserting without cli_msg_id preserves stored cli_msg_id
        store
            .upsert_message(&MessageDto {
                msg_id: "7970889894523".into(),
                cli_msg_id: None,
                thread_id: "7560225984834507848".into(),
                sender_id: "211235336".into(),
                sender_name: "Bạn".into(),
                text: "Tin nhan tu dien thoai".into(),
                msg_type: "chat.text".into(),
                ts: 1_782_286_279_518,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // Lookup by gMsgID, by cMsgID (when gMsgID=0), and with group_ prefix all resolve same row
        let by_gid = store
            .get_message_by_ids("7560225984834507848", "7970889894523", None)
            .unwrap()
            .unwrap();
        assert_eq!(by_gid.cli_msg_id.as_deref(), Some("1782286269667"));
        let by_cid = store
            .get_message_by_ids("group_7560225984834507848", "0", Some("1782286269667"))
            .unwrap()
            .unwrap();
        assert_eq!(by_cid.msg_id, "7970889894523");

        // Peer Alice reacts 👍 via cMsgID (gMsgID == 0 capture from mobile)
        let updated = store
            .apply_reaction_event(
                "7560225984834507848",
                "1782286269667",
                Some("1782286269667"),
                Some("u-alice"),
                Some("/-strong"),
                false,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.msg_id, "7970889894523");
        assert_eq!(updated.reactions.len(), 1);
        assert_eq!(updated.reactions[0].emoji, "👍");
        assert_eq!(updated.reactions[0].count, 1);
        assert!(!updated.reactions[0].me);

        // Idempotent replay of Alice's 👍 does NOT double-count
        let replayed = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                Some("1782286269667"),
                Some("u-alice"),
                Some("👍"),
                false,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(replayed.reactions[0].count, 1);

        // Peer Bob reacts 👍 -> count=2; Alice switches to ❤️ -> 👍=1, ❤️=1
        let _ = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("u-bob"),
                Some("👍"),
                false,
                false,
            )
            .unwrap()
            .unwrap();
        let after_switch = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("u-alice"),
                Some("❤"),
                false,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            after_switch.reactions.iter().find(|r| r.emoji == "👍").unwrap().count,
            1
        );
        let heart = after_switch.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 1);
        assert!(!heart.me);

        // Self adds ❤️ (from phone) -> ❤️ count=2, me=true
        let after_self = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("211235336"),
                Some("❤️"),
                true,
                false,
            )
            .unwrap()
            .unwrap();
        let heart = after_self.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 2);
        assert!(heart.me);

        // Peer Alice removes reaction (Zalo wire sends rIcon="" & rType=-1 => emoji=None!)
        let after_alice_remove = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("u-alice"),
                None,
                false,
                true,
            )
            .unwrap()
            .unwrap();
        let heart = after_alice_remove.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 1);
        assert!(heart.me);

        // Self removes ❤️ and Bob removes 👍 -> reactions become empty AND stay empty on SQLite reload!
        let _ = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("211235336"),
                None,
                true,
                true,
            )
            .unwrap()
            .unwrap();
        let after_all_removed = store
            .apply_reaction_event(
                "7560225984834507848",
                "7970889894523",
                None,
                Some("u-bob"),
                None,
                false,
                true,
            )
            .unwrap()
            .unwrap();
        assert!(after_all_removed.reactions.is_empty());

        // Reload from SQLite (regression guard against zombie reactions when removing last reaction!)
        let reloaded = store
            .get_message("7560225984834507848", "7970889894523")
            .unwrap()
            .unwrap();
        assert!(reloaded.reactions.is_empty());

        // Legacy row without cli_msg_id can still be resolved by ts == cMsgID fallback
        store
            .upsert_message(&MessageDto {
                msg_id: "7970889899999".into(),
                cli_msg_id: None,
                thread_id: "dm-legacy".into(),
                sender_id: "me".into(),
                sender_name: "Bạn".into(),
                text: "Legacy".into(),
                msg_type: "chat.text".into(),
                ts: 1_782_286_999_111,
                is_outgoing: true,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let legacy_hit = store
            .get_message_by_ids("dm-legacy", "0", Some("1782286999111"))
            .unwrap()
            .unwrap();
        assert_eq!(legacy_hit.msg_id, "7970889899999");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_non_canonical_reactions_unreact_toggle_and_merge_cleanly() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-react-legacy-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("react_legacy.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        // Pre-patch row with non-canonical emojis: "❤" (no U+FE0F, me=true), "😡" (peer), "/-heart" (peer)
        store
            .upsert_message(&MessageDto {
                msg_id: "m-legacy-1".into(),
                cli_msg_id: Some("1782287000001".into()),
                thread_id: "t-legacy".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Legacy emojis".into(),
                msg_type: "chat.text".into(),
                ts: 1_782_287_000_001,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: vec![
                    ReactionSummary { emoji: "❤".into(), count: 1, me: true },
                    ReactionSummary { emoji: "/-heart".into(), count: 1, me: false },
                    ReactionSummary { emoji: "😡".into(), count: 1, me: false },
                ],
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        // Unreacting "❤️" (canonical) must merge legacy "❤" + "/-heart" -> ❤️ (count 2, me true)
        // and remove self's reaction -> ❤️ (count 1, me false), persisted to SQLite!
        let after_unreact = store
            .apply_reaction_event("t-legacy", "m-legacy-1", None, None, Some("❤️"), true, true)
            .unwrap()
            .unwrap();
        let heart = after_unreact.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 1);
        assert!(!heart.me);
        assert!(!after_unreact.reactions.iter().any(|r| r.emoji == "❤" || r.emoji == "/-heart"));
        let angry = after_unreact.reactions.iter().find(|r| r.emoji == "😠").unwrap();
        assert_eq!(angry.count, 1);
        assert!(!angry.me);

        // Self reacting "😠" merges into normalized legacy "😡" -> count 2, me true (no duplicate!)
        let after_merge = store
            .apply_reaction_event("t-legacy", "m-legacy-1", None, None, Some("😠"), true, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            after_merge.reactions.iter().filter(|r| r.emoji == "😠" || r.emoji == "😡").count(),
            1
        );
        let angry = after_merge.reactions.iter().find(|r| r.emoji == "😠").unwrap();
        assert_eq!(angry.count, 2);
        assert!(angry.me);

        // Pre-patch row with legacy "😭" (me=true): toggle_reaction("😢") must detect already_me=true and remove cleanly
        store
            .upsert_message(&MessageDto {
                msg_id: "m-legacy-2".into(),
                cli_msg_id: None,
                thread_id: "t-legacy".into(),
                sender_id: "u1".into(),
                sender_name: "An".into(),
                text: "Legacy cry".into(),
                msg_type: "chat.text".into(),
                ts: 1_782_287_000_002,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: vec![ReactionSummary { emoji: "😭".into(), count: 1, me: true }],
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();
        let (toggled, removed) = store.toggle_reaction("t-legacy", "m-legacy-2", "😢").unwrap().unwrap();
        assert!(removed);
        assert!(toggled.reactions.is_empty());
        let reloaded = store.get_message("t-legacy", "m-legacy-2").unwrap().unwrap();
        assert!(reloaded.reactions.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_messages_encrypted_on_disk_chronological_and_clearable() {
        let dir = std::env::temp_dir().join(format!("teza-store-ai-msg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("ai.db");
        let store = MessageStore::open(&db_path).unwrap();

        let entries = [
            AiStoredMessageDto {
                id: "ai-1".into(),
                session_id: "default".into(),
                role: "user".into(),
                content: "Bí mật dự án Alpha 2026".into(),
                model: "gemini-2.5-flash".into(),
                backend: "gemini_api".into(),
                ts: 1_700_000_001_000,
            },
            AiStoredMessageDto {
                id: "ai-2".into(),
                session_id: "default".into(),
                role: "assistant".into(),
                content: "Đã tóm tắt kế hoạch Alpha".into(),
                model: "gemini-2.5-flash".into(),
                backend: "gemini_api".into(),
                ts: 1_700_000_002_000,
            },
            AiStoredMessageDto {
                id: "ai-3".into(),
                session_id: "default".into(),
                role: "user".into(),
                content: "Dịch sang tiếng Anh".into(),
                model: "gemini-2.5-flash".into(),
                backend: "gemini_api".into(),
                ts: 1_700_000_003_000,
            },
            AiStoredMessageDto {
                id: "ai-other".into(),
                session_id: "session-2".into(),
                role: "user".into(),
                content: "Phiên khác".into(),
                model: String::new(),
                backend: "gemini_cli".into(),
                ts: 1_700_000_004_000,
            },
        ];
        for e in &entries {
            store.insert_ai_message(e).unwrap();
        }

        // Invalid role is rejected.
        assert!(store
            .insert_ai_message(&AiStoredMessageDto {
                id: "ai-bad".into(),
                session_id: "default".into(),
                role: "system".into(),
                content: "nope".into(),
                model: String::new(),
                backend: String::new(),
                ts: 1_700_000_005_000,
            })
            .is_err());

        // Latest 2 messages returned in chronological oldest->newest order, with role normalized.
        let latest2 = store.list_ai_messages("default", 2).unwrap();
        assert_eq!(latest2.len(), 2);
        assert_eq!(latest2[0].id, "ai-2");
        assert_eq!(latest2[0].role, "model");
        assert_eq!(latest2[0].content, "Đã tóm tắt kế hoạch Alpha");
        assert_eq!(latest2[1].id, "ai-3");
        assert_eq!(latest2[1].role, "user");
        assert!(store.list_ai_messages("default", 0).unwrap().is_empty());

        // Raw SQLite row must store encrypted content with zero plaintext leakage.
        let raw = Connection::open(&db_path).unwrap();
        let raw_content: String = raw
            .query_row("SELECT content FROM ai_messages WHERE id = 'ai-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(raw_content.starts_with(ENC_PREFIX));
        assert!(!raw_content.contains("Alpha"));
        drop(raw);

        // Clearing one session leaves other sessions untouched.
        assert_eq!(store.clear_ai_messages("default").unwrap(), 3);
        assert!(store.list_ai_messages("default", 10).unwrap().is_empty());
        assert_eq!(store.list_ai_messages("session-2", 10).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_session_legacy_orphan_messages_backfill_on_list() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-ai-session-legacy-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("ai_legacy.db");
        let store = MessageStore::open(&db_path).unwrap();

        let long_first_prompt =
            "  Phân tích   chi tiết\nkế hoạch triển khai quý IV năm 2026 cho toàn bộ đội ngũ kỹ thuật  ";
        let long_last_reply = format!(
            "  Báo cáo\ttổng hợp\n{}",
            "Nội dung phản hồi dài để kiểm tra cắt ngắn preview sạch sẽ ".repeat(3)
        );
        for msg in [
            AiStoredMessageDto {
                id: "legacy-1".into(),
                session_id: "default".into(),
                role: "user".into(),
                content: long_first_prompt.into(),
                model: "gpt-4.1".into(),
                backend: "openai".into(),
                ts: 1_700_000_010_000,
            },
            AiStoredMessageDto {
                id: "legacy-2".into(),
                session_id: "default".into(),
                role: "assistant".into(),
                content: "Đã tiếp nhận yêu cầu.".into(),
                model: "gpt-4.1".into(),
                backend: "openai".into(),
                ts: 1_700_000_020_000,
            },
            AiStoredMessageDto {
                id: "legacy-3".into(),
                session_id: "default".into(),
                role: "model".into(),
                content: long_last_reply,
                model: "gpt-4.1".into(),
                backend: "openai".into(),
                ts: 1_700_000_030_000,
            },
            AiStoredMessageDto {
                id: "legacy-model-only-1".into(),
                session_id: "model-only".into(),
                role: "model".into(),
                content: "Xin chào! Tôi có thể giúp gì cho bạn?".into(),
                model: "gpt-4.1".into(),
                backend: "openai".into(),
                ts: 1_700_000_005_000,
            },
        ] {
            store.insert_ai_message(&msg).unwrap();
        }

        // Simulate a pre-upgrade DB where ai_messages existed before ai_sessions rows were written.
        let raw = Connection::open(&db_path).unwrap();
        raw.execute("DELETE FROM ai_sessions", []).unwrap();
        drop(raw);

        let sessions = store.list_ai_sessions(10).unwrap();
        assert_eq!(sessions.len(), 2);

        let def = &sessions[0];
        assert_eq!(def.id, "default");
        assert_eq!(def.created_ts, 1_700_000_010_000);
        assert_eq!(def.updated_ts, 1_700_000_030_000);
        assert_eq!(def.message_count, 3);
        assert!(def.title.starts_with("Phân tích chi tiết kế hoạch"));
        assert!(def.title.ends_with('…'));
        assert!(def.title.chars().count() <= AI_SESSION_AUTO_TITLE_CHARS);
        assert!(!def.title.contains('\n'));
        assert!(def.last_preview.starts_with("Báo cáo tổng hợp Nội dung"));
        assert!(def.last_preview.ends_with('…'));
        assert!(def.last_preview.chars().count() <= AI_SESSION_PREVIEW_CHARS);
        assert!(!def.last_preview.contains('\n'));
        assert!(!def.last_preview.contains('\t'));

        let model_only = &sessions[1];
        assert_eq!(model_only.id, "model-only");
        assert_eq!(model_only.title, AI_SESSION_LEGACY_FALLBACK_TITLE);
        assert_eq!(model_only.created_ts, 1_700_000_005_000);
        assert_eq!(model_only.updated_ts, 1_700_000_005_000);
        assert_eq!(model_only.message_count, 1);
        assert_eq!(
            model_only.last_preview,
            "Xin chào! Tôi có thể giúp gì cho bạn?"
        );

        // Raw SQLite row for backfilled session title must be encrypted on disk.
        let raw = Connection::open(&db_path).unwrap();
        let raw_title: String = raw
            .query_row(
                "SELECT title FROM ai_sessions WHERE id = 'default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(raw_title.starts_with(ENC_PREFIX));
        assert!(!raw_title.contains("Phân tích"));
        drop(raw);

        // Calling list_ai_sessions again is idempotent.
        let sessions_again = store.list_ai_sessions(10).unwrap();
        assert_eq!(sessions_again, sessions);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_session_create_auto_title_rename_preview_and_delete() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-ai-session-crud-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("ai_sessions.db");
        let store = MessageStore::open(&db_path).unwrap();

        // Validation errors on create_ai_session.
        assert!(store
            .create_ai_session("   ", "Hợp lệ", 1_700_000_000_000)
            .is_err());
        let too_long_id = "s".repeat(AI_SESSION_MAX_ID_CHARS + 1);
        assert!(store
            .create_ai_session(&too_long_id, "Hợp lệ", 1_700_000_000_000)
            .is_err());

        // Empty title defaults to "Cuộc trò chuyện mới".
        let s1 = store
            .create_ai_session("sess-1", "   ", 1_700_000_001_000)
            .unwrap();
        assert_eq!(s1.id, "sess-1");
        assert_eq!(s1.title, AI_SESSION_DEFAULT_TITLE);
        assert_eq!(s1.created_ts, 1_700_000_001_000);
        assert_eq!(s1.updated_ts, 1_700_000_001_000);
        assert_eq!(s1.message_count, 0);
        assert_eq!(s1.last_preview, "");

        // Overlong title on create is truncated cleanly to 120 chars.
        let long_custom_title = "Đ".repeat(AI_SESSION_MAX_TITLE_CHARS + 30);
        let s_long = store
            .create_ai_session("sess-long", &long_custom_title, 1_700_000_000_500)
            .unwrap();
        assert_eq!(s_long.title.chars().count(), AI_SESSION_MAX_TITLE_CHARS);

        // Duplicate create with empty title preserves existing custom title.
        let s_long_dup = store
            .create_ai_session("sess-long", "", 1_700_000_009_999)
            .unwrap();
        assert_eq!(s_long_dup.title, s_long.title);
        assert_eq!(s_long_dup.created_ts, 1_700_000_000_500);

        // Model message first keeps placeholder title but bumps updated_ts.
        store
            .insert_ai_message(&AiStoredMessageDto {
                id: "m-1".into(),
                session_id: "sess-1".into(),
                role: "assistant".into(),
                content: "Chào bạn, hãy đặt câu hỏi!".into(),
                model: "m1".into(),
                backend: "b1".into(),
                ts: 1_700_000_002_000,
            })
            .unwrap();
        let after_model = store.list_ai_sessions(10).unwrap();
        let s1_after_model = after_model.iter().find(|s| s.id == "sess-1").unwrap();
        assert_eq!(s1_after_model.title, AI_SESSION_DEFAULT_TITLE);
        assert_eq!(s1_after_model.updated_ts, 1_700_000_002_000);
        assert_eq!(s1_after_model.message_count, 1);

        // First user message upgrades placeholder title to single-line <= 48 chars with ellipsis.
        store
            .insert_ai_message(&AiStoredMessageDto {
                id: "m-2".into(),
                session_id: "sess-1".into(),
                role: "user".into(),
                content: "  Tóm tắt\nnhanh danh sách công việc ưu tiên trong tuần tới giúp mình nhé! "
                    .into(),
                model: "m1".into(),
                backend: "b1".into(),
                ts: 1_700_000_003_000,
            })
            .unwrap();
        let after_first_user = store.list_ai_sessions(10).unwrap();
        let s1_upgraded = after_first_user.iter().find(|s| s.id == "sess-1").unwrap();
        assert!(s1_upgraded.title.starts_with("Tóm tắt nhanh danh sách"));
        assert!(s1_upgraded.title.ends_with('…'));
        assert!(s1_upgraded.title.chars().count() <= AI_SESSION_AUTO_TITLE_CHARS);
        assert!(!s1_upgraded.title.contains('\n'));
        assert_eq!(s1_upgraded.updated_ts, 1_700_000_003_000);
        let auto_title = s1_upgraded.title.clone();

        // Second user message preserves auto-upgraded title and updates last_preview + count.
        let long_followup = format!(
            "Câu hỏi tiếp theo:\n{}",
            "Chi tiết hạng mục số một cần kiểm tra kỹ lưỡng ".repeat(3)
        );
        store
            .insert_ai_message(&AiStoredMessageDto {
                id: "m-3".into(),
                session_id: "sess-1".into(),
                role: "user".into(),
                content: long_followup,
                model: "m1".into(),
                backend: "b1".into(),
                ts: 1_700_000_004_000,
            })
            .unwrap();
        let after_second_user = store.list_ai_sessions(10).unwrap();
        let s1_second = after_second_user.iter().find(|s| s.id == "sess-1").unwrap();
        assert_eq!(s1_second.title, auto_title);
        assert_eq!(s1_second.message_count, 3);
        assert_eq!(s1_second.updated_ts, 1_700_000_004_000);
        assert!(s1_second.last_preview.starts_with("Câu hỏi tiếp theo: Chi tiết"));
        assert!(s1_second.last_preview.ends_with('…'));
        assert!(s1_second.last_preview.chars().count() <= AI_SESSION_PREVIEW_CHARS);

        // Inserting into an unseen session auto-creates an ai_sessions entry.
        store
            .insert_ai_message(&AiStoredMessageDto {
                id: "m-s2-1".into(),
                session_id: "sess-2".into(),
                role: "user".into(),
                content: "Viết email cảm ơn khách hàng".into(),
                model: "m1".into(),
                backend: "b1".into(),
                ts: 1_700_000_010_000,
            })
            .unwrap();

        // Rename validation and happy path.
        assert!(store.rename_ai_session("", "Hợp lệ").is_err());
        assert!(store.rename_ai_session("sess-1", "   ").is_err());
        assert!(store
            .rename_ai_session("sess-1", &"A".repeat(AI_SESSION_MAX_TITLE_CHARS + 1))
            .is_err());
        assert!(store.rename_ai_session("non-existent", "Hợp lệ").is_err());

        let renamed = store
            .rename_ai_session("sess-1", "  Bí mật Kế hoạch   Phát hành 2026  ")
            .unwrap();
        assert_eq!(renamed.id, "sess-1");
        assert_eq!(renamed.title, "Bí mật Kế hoạch Phát hành 2026");
        assert_eq!(renamed.created_ts, 1_700_000_001_000);
        assert_eq!(renamed.updated_ts, 1_700_000_004_000);
        assert_eq!(renamed.message_count, 3);
        assert_eq!(renamed.last_preview, s1_second.last_preview);

        // Raw SQLite title must be encrypted with zero plaintext leakage.
        let raw = Connection::open(&db_path).unwrap();
        let raw_title: String = raw
            .query_row(
                "SELECT title FROM ai_sessions WHERE id = 'sess-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(raw_title.starts_with(ENC_PREFIX));
        assert!(!raw_title.contains("Phát hành"));
        assert!(!raw_title.contains("2026"));
        drop(raw);

        // Ordering is updated_ts DESC, id DESC; limit is respected.
        assert!(store.list_ai_sessions(0).unwrap().is_empty());
        let top1 = store.list_ai_sessions(1).unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].id, "sess-2");
        assert_eq!(top1[0].title, "Viết email cảm ơn khách hàng");

        let all = store.list_ai_sessions(10).unwrap();
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["sess-2", "sess-1", "sess-long"]);

        // clear_ai_messages keeps the ai_sessions row intact with 0 messages and empty preview.
        assert_eq!(store.clear_ai_messages("sess-1").unwrap(), 3);
        let after_clear = store.list_ai_sessions(10).unwrap();
        let s1_cleared = after_clear.iter().find(|s| s.id == "sess-1").unwrap();
        assert_eq!(s1_cleared.title, "Bí mật Kế hoạch Phát hành 2026");
        assert_eq!(s1_cleared.message_count, 0);
        assert_eq!(s1_cleared.last_preview, "");

        // delete_ai_session removes messages and session row without touching other sessions.
        assert_eq!(store.delete_ai_session("sess-2").unwrap(), 1);
        assert!(store.list_ai_messages("sess-2", 10).unwrap().is_empty());
        let remaining = store.list_ai_sessions(10).unwrap();
        let remaining_ids: Vec<&str> = remaining.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(remaining_ids, vec!["sess-1", "sess-long"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_unread_messages_for_summary_respects_watermark_and_fallback() {
        let dir =
            std::env::temp_dir().join(format!("teza-store-unread-sum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("unread_sum.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let make_msg = |tid: &str, id: &str, ts: i64, outgoing: bool, recalled: bool| MessageDto {
            msg_id: id.into(),
            cli_msg_id: None,
            thread_id: tid.into(),
            sender_id: if outgoing { "me".into() } else { "peer".into() },
            sender_name: if outgoing { "Bạn".into() } else { "An".into() },
            text: format!("Nội dung {id}"),
            msg_type: "chat.text".into(),
            ts,
            is_outgoing: outgoing,
            is_recalled: recalled,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        };

        // Thread A: last_read_in_ts > 0 -> returns non-recalled messages strictly after watermark,
        // chronological oldest->newest, capped to latest max_count.
        store
            .upsert_thread(&ThreadDto {
                id: "t-wm".into(),
                title: "Nhóm A".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: "hi".into(),
                last_ts: 1_700_000_050_000,
                unread_count: 3,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 1_700_000_020_000,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        for (id, ts, out, rec) in [
            ("m1", 1_700_000_010_000, false, false),
            ("m2", 1_700_000_020_000, false, false),
            ("m3", 1_700_000_030_000, false, false),
            ("m4", 1_700_000_035_000, true, false),
            ("m5", 1_700_000_040_000, false, true),
            ("m6", 1_700_000_050_000, false, false),
        ] {
            store
                .upsert_message(&make_msg("t-wm", id, ts, out, rec))
                .unwrap();
        }

        let unread = store.list_unread_messages_for_summary("t-wm", 20).unwrap();
        let ids: Vec<&str> = unread.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(ids, vec!["m3", "m4", "m6"]);

        let capped = store.list_unread_messages_for_summary("t-wm", 2).unwrap();
        let capped_ids: Vec<&str> = capped.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(capped_ids, vec!["m4", "m6"]);

        // Thread B: last_read_in_ts == 0 -> returns min(max(unread_count, 10), max_count) incoming.
        store
            .upsert_thread(&ThreadDto {
                id: "t-zero".into(),
                title: "Nhóm B".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: "hi".into(),
                last_ts: 1_700_000_200_000,
                unread_count: 3,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        for i in 1..=15_i64 {
            store
                .upsert_message(&make_msg(
                    "t-zero",
                    &format!("in-{i:02}"),
                    1_700_000_000_000 + i * 1_000,
                    false,
                    false,
                ))
                .unwrap();
        }
        store
            .upsert_message(&make_msg("t-zero", "out-1", 1_700_000_099_000, true, false))
            .unwrap();

        let fallback10 = store.list_unread_messages_for_summary("t-zero", 40).unwrap();
        assert_eq!(fallback10.len(), 10);
        assert_eq!(fallback10[0].msg_id, "in-06");
        assert_eq!(fallback10[9].msg_id, "in-15");
        assert!(fallback10.iter().all(|m| !m.is_outgoing));

        // Capped below 10 when max_count < 10
        let fallback5 = store.list_unread_messages_for_summary("t-zero", 5).unwrap();
        assert_eq!(fallback5.len(), 5);
        assert_eq!(fallback5[0].msg_id, "in-11");
        assert_eq!(fallback5[4].msg_id, "in-15");

        // Thread C: watermark caught up & unread_count == 0 -> empty; if unread_count > 0 -> fallback.
        store.mark_thread_read("t-wm").unwrap();
        assert!(store
            .list_unread_messages_for_summary("t-wm", 20)
            .unwrap()
            .is_empty());
        store
            .conn
            .execute(
                "UPDATE threads SET unread_count = 2 WHERE id = 't-wm'",
                [],
            )
            .unwrap();
        let marked_unread = store.list_unread_messages_for_summary("t-wm", 20).unwrap();
        assert!(!marked_unread.is_empty());
        assert!(marked_unread.iter().all(|m| !m.is_outgoing));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_next_occurrence_ts_handles_once_daily_weekly_and_monthly_rollover() {
        let jan31_2026 = DateTime::parse_from_rfc3339("2026-01-31T09:30:00Z")
            .unwrap()
            .timestamp_millis();
        let feb28_2026 = DateTime::parse_from_rfc3339("2026-02-28T09:30:00Z")
            .unwrap()
            .timestamp_millis();
        let mar31_2026 = DateTime::parse_from_rfc3339("2026-03-31T09:30:00Z")
            .unwrap()
            .timestamp_millis();

        assert_eq!(
            compute_next_occurrence_in_tz(&Utc, jan31_2026, Some(31), "once", jan31_2026),
            None
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&Utc, jan31_2026, Some(31), "daily", jan31_2026),
            Some(jan31_2026 + DAY_MS)
        );
        // Catch-up past 3.5 days jumps straight to day +4 at same wall time.
        assert_eq!(
            compute_next_occurrence_in_tz(
                &Utc,
                jan31_2026,
                Some(31),
                "daily",
                jan31_2026 + 3 * DAY_MS + 10_000
            ),
            Some(jan31_2026 + 4 * DAY_MS)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&Utc, jan31_2026, Some(31), "weekly", jan31_2026),
            Some(jan31_2026 + WEEK_MS)
        );
        // Monthly rollover clamps Jan 31 -> Feb 28 (non-leap 2026).
        assert_eq!(
            compute_next_occurrence_in_tz(
                &Utc,
                jan31_2026,
                Some(31),
                "monthly",
                jan31_2026 + 1_000
            ),
            Some(feb28_2026)
        );
        // Catch-up past Feb 28 preserves anchor day 31 -> Mar 31!
        assert_eq!(
            compute_next_occurrence_in_tz(
                &Utc,
                jan31_2026,
                Some(31),
                "monthly",
                feb28_2026 + 60_000
            ),
            Some(mar31_2026)
        );
        // Leap year 2028: Jan 31 -> Feb 29.
        let jan31_2028 = DateTime::parse_from_rfc3339("2028-01-31T08:00:00Z")
            .unwrap()
            .timestamp_millis();
        let feb29_2028 = DateTime::parse_from_rfc3339("2028-02-29T08:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&Utc, jan31_2028, Some(31), "monthly", jan31_2028),
            Some(feb29_2028)
        );

        // Local-timezone convenience wrapper matches local calendar wall-clock time.
        let local_jan31 = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 1, 31)
                .unwrap()
                .and_hms_opt(9, 30, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        let local_feb28 = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 2, 28)
                .unwrap()
                .and_hms_opt(9, 30, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        let local_mar31 = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 3, 31)
                .unwrap()
                .and_hms_opt(9, 30, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        assert_eq!(compute_next_occurrence_ts(local_jan31, "once", local_jan31), None);
        assert_eq!(
            compute_next_occurrence_ts(local_jan31, "monthly", local_jan31 + 1_000),
            Some(local_feb28)
        );
        assert_eq!(
            compute_next_occurrence_ts_with_anchor(
                local_feb28,
                Some(31),
                "monthly",
                local_feb28 + 1_000
            ),
            Some(local_mar31)
        );
    }

    #[test]
    fn scheduled_messages_encrypted_atomic_claim_recovery_recurrence_and_misfire() {
        let dir = std::env::temp_dir().join(format!("teza-store-sched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("sched.db");
        let store = MessageStore::open(&db_path).unwrap();

        // Seed a thread to test auto-resolution of thread_title & thread_kind.
        store
            .upsert_thread(&ThreadDto {
                id: "grp-ops".into(),
                title: "Nhóm Vận Hành Mật".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: String::new(),
                last_ts: 1_700_000_000_000,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();

        let base_ts = 1_750_000_000_000_i64;
        let s1 = store
            .create_scheduled_message(&NewScheduledMessageInput {
                thread_id: "grp-ops".into(),
                thread_title: String::new(),
                thread_kind: String::new(),
                text: "Báo cáo doanh thu mật 999".into(),
                attachment_path: r"J:\secret\report.pdf".into(),
                send_at_ts: base_ts,
                recurrence: "once".into(),
                misfire_policy: "ask".into(),
                misfire_grace_secs: 600,
            })
            .unwrap();
        assert_eq!(s1.thread_title, "Nhóm Vận Hành Mật");
        assert_eq!(s1.thread_kind, "group");
        assert_eq!(s1.status, "pending");

        let s2 = store
            .create_scheduled_message(&NewScheduledMessageInput {
                thread_id: "u-bob".into(),
                thread_title: "Bob".into(),
                thread_kind: "direct".into(),
                text: "Chào buổi sáng".into(),
                attachment_path: String::new(),
                send_at_ts: base_ts + 5_000,
                recurrence: "daily".into(),
                misfire_policy: "skip".into(),
                misfire_grace_secs: 300,
            })
            .unwrap();

        let s3 = store
            .create_scheduled_message(&NewScheduledMessageInput {
                thread_id: "u-future".into(),
                thread_title: "Future".into(),
                thread_kind: "direct".into(),
                text: "Tương lai".into(),
                attachment_path: String::new(),
                send_at_ts: base_ts + 60_000,
                recurrence: "once".into(),
                misfire_policy: "send_on_resume".into(),
                misfire_grace_secs: 600,
            })
            .unwrap();

        // Raw SQLite verification: all sensitive fields MUST be encrypted on disk.
        {
            let raw = Connection::open(&db_path).unwrap();
            let (rt, rtxt, ratt, rerr): (String, String, String, String) = raw
                .query_row(
                    "SELECT thread_title, text, attachment_path, last_error FROM scheduled_messages WHERE id = ?1",
                    params![&s1.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert!(rt.starts_with(ENC_PREFIX) && !rt.contains("Vận Hành"));
            assert!(rtxt.starts_with(ENC_PREFIX) && !rtxt.contains("999"));
            assert!(ratt.starts_with(ENC_PREFIX) && !ratt.contains("report.pdf"));
            assert!(rerr.starts_with(ENC_PREFIX));
        }

        // Update s1 text before sending.
        let updated = store
            .update_scheduled_message(
                &s1.id,
                &UpdateScheduledMessageInput {
                    text: Some("Báo cáo đã cập nhật".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.text, "Báo cáo đã cập nhật");
        assert_eq!(store.next_pending_schedule_ts().unwrap(), Some(base_ts));

        // Atomic claim: claims s1 & s2, leaves s3 pending; second call returns empty!
        let claimed = store
            .claim_due_scheduled_messages(base_ts + 10_000, 10)
            .unwrap();
        let claimed_ids: Vec<&str> = claimed.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(claimed_ids, vec![s1.id.as_str(), s2.id.as_str()]);
        assert!(claimed.iter().all(|c| c.status == "sending"));
        assert!(store
            .claim_due_scheduled_messages(base_ts + 10_000, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.next_pending_schedule_ts().unwrap(),
            Some(base_ts + 60_000)
        );

        // Crash recovery: s1 (retry 0) -> pending (retry 1); force s2 retry_count=2 -> failed!
        store
            .conn
            .execute(
                "UPDATE scheduled_messages SET retry_count = 2 WHERE id = ?1",
                params![&s2.id],
            )
            .unwrap();
        let recovered = store
            .recover_stuck_sending_schedules(base_ts + 15_000)
            .unwrap();
        assert_eq!(recovered, 2);
        let s1_rec = store.get_scheduled_message(&s1.id).unwrap().unwrap();
        assert_eq!(s1_rec.status, "pending");
        assert_eq!(s1_rec.retry_count, 1);
        let s2_rec = store.get_scheduled_message(&s2.id).unwrap().unwrap();
        assert_eq!(s2_rec.status, "failed");
        assert_eq!(s2_rec.retry_count, 3);

        // 1-click reschedule_now on failed s2 sets next_attempt_ts + bypass_misfire WITHOUT
        // overwriting canonical send_at_ts!
        let s2_now = store
            .reschedule_now(&s2.id, base_ts + 20_000)
            .unwrap()
            .unwrap();
        assert_eq!(s2_now.status, "pending");
        assert_eq!(s2_now.send_at_ts, base_ts + 5_000);
        assert_eq!(s2_now.next_attempt_ts, Some(base_ts + 20_000));
        assert!(s2_now.bypass_misfire);
        assert_eq!(s2_now.retry_count, 0);
        assert!(s2_now.last_error.is_empty());

        // Complete recurring s2 -> advances from canonical send_at_ts (base_ts + 5_000), clearing
        // next_attempt_ts & bypass_misfire!
        let expected_s2_next1 =
            compute_next_occurrence_ts(base_ts + 5_000, "daily", base_ts + 20_000).unwrap();
        let s2_done = store
            .complete_scheduled_message(&s2.id, base_ts + 20_000)
            .unwrap()
            .unwrap();
        assert_eq!(s2_done.status, "pending");
        assert_eq!(s2_done.send_at_ts, expected_s2_next1);
        assert_eq!(s2_done.next_attempt_ts, None);
        assert!(!s2_done.bypass_misfire);
        assert_eq!(s2_done.sent_at_ts, base_ts + 20_000);

        // Misfire "skip" on recurring s2 -> advances another day & stays pending!
        let skip_now = expected_s2_next1 + 700_000;
        let expected_s2_next2 =
            compute_next_occurrence_ts(expected_s2_next1, "daily", skip_now).unwrap();
        let s2_skipped = store
            .mark_scheduled_message_missed(&s2.id, "Bỏ qua do tắt máy", skip_now)
            .unwrap()
            .unwrap();
        assert_eq!(s2_skipped.status, "pending");
        assert_eq!(s2_skipped.send_at_ts, expected_s2_next2);
        assert_eq!(s2_skipped.next_attempt_ts, None);
        assert!(!s2_skipped.bypass_misfire);

        // Retry on s1 sets next_attempt_ts WITHOUT overwriting canonical send_at_ts;
        // then mark missed ("ask"), then reschedule_now, then complete ("once" -> sent).
        let s1_retry = store
            .fail_or_retry_scheduled_message(
                &s1.id,
                "Lỗi mạng tạm thời",
                Some(base_ts + 50_000),
                base_ts + 20_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(s1_retry.status, "pending");
        assert_eq!(s1_retry.send_at_ts, base_ts);
        assert_eq!(s1_retry.next_attempt_ts, Some(base_ts + 50_000));
        assert_eq!(s1_retry.retry_count, 2);

        let s1_missed = store
            .mark_scheduled_message_missed(&s1.id, "Quá hạn chờ xác nhận", base_ts + 2_000_000)
            .unwrap()
            .unwrap();
        assert_eq!(s1_missed.status, "missed");

        let err_list = store
            .list_scheduled_messages(Some("failed,missed"), 50)
            .unwrap();
        assert_eq!(err_list.len(), 1);
        assert_eq!(err_list[0].id, s1.id);

        let _ = store.reschedule_now(&s1.id, base_ts + 2_001_000).unwrap();
        let s1_sent = store
            .complete_scheduled_message(&s1.id, base_ts + 2_002_000)
            .unwrap()
            .unwrap();
        assert_eq!(s1_sent.status, "sent");
        assert_eq!(s1_sent.sent_at_ts, base_ts + 2_002_000);

        // Cancel & delete s3
        assert!(store.cancel_scheduled_message(&s3.id).unwrap());
        assert_eq!(
            store.get_scheduled_message(&s3.id).unwrap().unwrap().status,
            "cancelled"
        );
        assert!(store.delete_scheduled_message(&s3.id).unwrap());
        assert!(store.get_scheduled_message(&s3.id).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_clients_hash_only_verification_audit_cap_and_idempotency_ttl() {
        let dir = std::env::temp_dir().join(format!("teza-store-localapi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("localapi.db");
        let store = MessageStore::open(&db_path).unwrap();

        // Token generation & hashing
        let t1 = generate_api_token().unwrap();
        let t2 = generate_api_token().unwrap();
        assert!(t1.starts_with("teza_"));
        assert_eq!(t1.len(), 48);
        assert_ne!(t1, t2);
        let h1 = hash_api_token(&t1);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        // Create client
        let created = store
            .create_api_client(
                "n8n Workflow",
                &["send".into(), "read_threads".into()],
                &["t-1".into(), "t-2".into(), "t-1".into()],
                30,
            )
            .unwrap();
        assert!(created.raw_token.starts_with("teza_"));
        assert_eq!(created.client.allowed_thread_ids, vec!["t-1", "t-2"]);
        assert_eq!(created.client.rate_limit_per_min, 30);
        // Debug redacts raw_token!
        assert!(!format!("{created:?}").contains(&created.raw_token));

        // Raw SQLite stores ONLY SHA-256 hash, never raw_token!
        {
            let raw = Connection::open(&db_path).unwrap();
            let (stored_hash, stored_prefix): (String, String) = raw
                .query_row(
                    "SELECT token_hash, token_prefix FROM api_clients WHERE id = ?1",
                    params![&created.client.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(stored_hash, hash_api_token(&created.raw_token));
            assert_ne!(stored_hash, created.raw_token);
            assert!(stored_prefix.len() <= 12);
            assert_ne!(stored_prefix, created.raw_token);
        }

        // Verify token updates last_used_at; invalid/disabled tokens return None.
        let verified = store
            .verify_api_token(&created.raw_token, 1_750_000_010_000)
            .unwrap()
            .unwrap();
        assert_eq!(verified.id, created.client.id);
        assert_eq!(verified.last_used_at, 1_750_000_010_000);
        assert!(store
            .verify_api_token("teza_wrong_token", 1_750_000_020_000)
            .unwrap()
            .is_none());
        assert!(store
            .verify_api_token(&format!(" {} ", created.raw_token), 1_750_000_020_000)
            .unwrap()
            .is_none());

        // Disable client -> token immediately rejected.
        let disabled = store
            .update_api_client(&created.client.id, Some(false), None, None, None)
            .unwrap()
            .unwrap();
        assert!(!disabled.enabled);
        assert!(store
            .verify_api_token(&created.raw_token, 1_750_000_030_000)
            .unwrap()
            .is_none());

        // Re-enable & update scopes/rate_limit
        let reenabled = store
            .update_api_client(
                &created.client.id,
                Some(true),
                Some(&["schedule".into()]),
                Some(&[]),
                Some(45),
            )
            .unwrap()
            .unwrap();
        assert!(reenabled.enabled);
        assert_eq!(reenabled.scopes, vec!["schedule"]);
        assert!(reenabled.allowed_thread_ids.is_empty());
        assert_eq!(reenabled.rate_limit_per_min, 45);
        assert_eq!(store.list_api_clients().unwrap().len(), 1);

        // Idempotency: encrypted on disk, TTL check, and >24h auto-prune.
        let t0 = 1_750_000_000_000_i64;
        store
            .put_api_idempotency(
                &created.client.id,
                "idem-old",
                200,
                r#"{"ok":true,"secret":"payload-old"}"#,
                t0,
            )
            .unwrap();
        {
            let raw = Connection::open(&db_path).unwrap();
            let enc_resp: String = raw
                .query_row(
                    "SELECT response_json FROM api_idempotency WHERE idem_key = 'idem-old'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(enc_resp.starts_with(ENC_PREFIX));
            assert!(!enc_resp.contains("payload-old"));
        }
        assert_eq!(
            store
                .get_api_idempotency(&created.client.id, "idem-old", 60_000, t0 + 30_000)
                .unwrap(),
            Some((200, r#"{"ok":true,"secret":"payload-old"}"#.to_string()))
        );
        // Expired TTL -> returns None
        assert_eq!(
            store
                .get_api_idempotency(&created.client.id, "idem-old", 60_000, t0 + 60_001)
                .unwrap(),
            None
        );

        // Re-insert old key and confirm put > 24h later prunes it while keeping fresh key.
        store
            .put_api_idempotency(&created.client.id, "idem-stale", 200, r#"{"old":1}"#, t0)
            .unwrap();
        store
            .put_api_idempotency(
                &created.client.id,
                "idem-fresh",
                201,
                r#"{"fresh":1}"#,
                t0 + DAY_MS + 10_000,
            )
            .unwrap();
        assert!(store
            .get_api_idempotency(
                &created.client.id,
                "idem-stale",
                2 * DAY_MS,
                t0 + DAY_MS + 10_000
            )
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get_api_idempotency(
                    &created.client.id,
                    "idem-fresh",
                    DAY_MS,
                    t0 + DAY_MS + 20_000
                )
                .unwrap(),
            Some((201, r#"{"fresh":1}"#.to_string()))
        );

        // Audit logs: encrypted detail + 500-row cap + clear.
        for i in 1..=510_i64 {
            store
                .append_api_audit_log(
                    &created.client.id,
                    "n8n Workflow",
                    "POST",
                    "/v1/messages/send",
                    "t-1",
                    200,
                    "ok",
                    &format!("Chi tiết bí mật #{i}"),
                    t0 + i * 1_000,
                )
                .unwrap();
        }
        let logs = store.list_api_audit_logs(600).unwrap();
        assert_eq!(logs.len(), 500);
        assert_eq!(logs[0].detail, "Chi tiết bí mật #510");
        assert_eq!(logs[499].detail, "Chi tiết bí mật #11");
        {
            let raw = Connection::open(&db_path).unwrap();
            let raw_detail: String = raw
                .query_row(
                    "SELECT detail FROM api_audit_logs ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(raw_detail.starts_with(ENC_PREFIX));
            assert!(!raw_detail.contains("bí mật"));
        }
        assert_eq!(store.clear_api_audit_logs().unwrap(), 500);
        assert!(store.list_api_audit_logs(10).unwrap().is_empty());

        // Delete client cleans up client & idempotency rows.
        assert!(store.delete_api_client(&created.client.id).unwrap());
        assert!(store
            .verify_api_token(&created.raw_token, t0 + DAY_MS + 30_000)
            .unwrap()
            .is_none());
        assert!(store
            .get_api_idempotency(&created.client.id, "idem-fresh", DAY_MS, t0 + DAY_MS + 30_000)
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_media_original_name_by_hash_prefers_primary_and_skips_empty_names() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-media-name-hash-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("media_name_hash.db");
        let store = MessageStore::open(&db_path).unwrap();

        // Empty / unknown hash returns None.
        assert_eq!(store.find_media_original_name_by_hash("").unwrap(), None);
        assert_eq!(
            store.find_media_original_name_by_hash("missing-hash").unwrap(),
            None
        );

        // Non-primary variant with name + newer primary row with blank name -> skips blank primary,
        // returns non-empty name; once a real primary name exists, prefers primary over newer thumb.
        let hash = "uh-photo-abc123";
        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t-1".into(),
                msg_id: "m-1".into(),
                variant: "thumb".into(),
                url_hash: hash.into(),
                sha256: None,
                original_name: Some("fallback_thumb.webp".into()),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 5_000,
            })
            .unwrap();
        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t-1".into(),
                msg_id: "m-2".into(),
                variant: "primary".into(),
                url_hash: hash.into(),
                sha256: None,
                original_name: Some("   ".into()),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 6_000,
            })
            .unwrap();
        assert_eq!(
            store.find_media_original_name_by_hash(hash).unwrap(),
            Some("fallback_thumb.webp".to_string())
        );

        // Add primary row with actual original filename -> preferred even if thumb has newer ts.
        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t-2".into(),
                msg_id: "m-3".into(),
                variant: "primary".into(),
                url_hash: hash.into(),
                sha256: None,
                original_name: Some("bao_cao_2026.11.26.png".into()),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 4_000,
            })
            .unwrap();
        assert_eq!(
            store.find_media_original_name_by_hash(hash).unwrap(),
            Some("bao_cao_2026.11.26.png".to_string())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_messages_supports_korean_substrings_nfd_nfc_choseong_and_vietnamese() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-ko-search-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("ko_search.db");

        {
            let mut store = MessageStore::open(&db_path).unwrap();
            let msgs = [
                MessageDto {
                    msg_id: "ko-1".into(),
                    cli_msg_id: None,
                    thread_id: "t-ko".into(),
                    sender_id: "u-kim".into(),
                    sender_name: "김민수".into(),
                    text: "김부장님께 보고서 보냈습니다".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_100_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                },
                MessageDto {
                    msg_id: "ko-2".into(),
                    cli_msg_id: None,
                    thread_id: "t-ko".into(),
                    sender_id: "u-park".into(),
                    sender_name: "박지영".into(),
                    text: "회의실에서 만나요".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_200_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                },
                MessageDto {
                    msg_id: "ko-3".into(),
                    cli_msg_id: None,
                    thread_id: "t-ko".into(),
                    sender_id: "u-kim".into(),
                    // Stored as NFD Hangul Jamo for "김민수" + "오늘보고서"
                    sender_name: "\u{1100}\u{1175}\u{11B7}\u{1106}\u{1175}\u{11AB}\u{1109}\u{116E}"
                        .into(),
                    text: "\u{110B}\u{1169}\u{1102}\u{1173}\u{11AF}\u{1107}\u{1169}\u{1100}\u{1169}\u{1109}\u{1165} 확인 부탁드립니다"
                        .into(),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_300_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                },
                MessageDto {
                    msg_id: "vi-1".into(),
                    cli_msg_id: None,
                    thread_id: "t-vi".into(),
                    sender_id: "u-minh".into(),
                    // Stored with NFD Vietnamese diacritics
                    sender_name: "Nguy\u{0065}\u{0302}\u{0303}n V\u{0061}\u{0306}n Minh".into(),
                    text: "Đà Nẵng báo cáo tiến độ dự án".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_400_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                },
            ];
            for m in &msgs {
                store.upsert_message(m).unwrap();
            }

            // 1. Korean agglutinative compound substring search
            let bujang = store.search_messages("부장", 20).unwrap();
            assert_eq!(
                bujang.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["ko-1"]
            );

            let sil = store.search_messages("실", 20).unwrap();
            assert_eq!(
                sil.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["ko-2"]
            );

            // "보고서" matches both "오늘보고서" (ko-3, newer) and "보고서" (ko-1, older) without duplicates
            let bogoseo = store.search_messages("보고서", 20).unwrap();
            assert_eq!(
                bogoseo.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["ko-3", "ko-1"]
            );

            // 2. NFD Hangul query ("부장" in NFD) matches NFC stored message
            let nfd_bujang = "\u{1107}\u{116E}\u{110C}\u{1161}\u{11BC}";
            let nfd_hits = store.search_messages(nfd_bujang, 20).unwrap();
            assert_eq!(
                nfd_hits.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["ko-1"]
            );

            // NFC query ("오늘보고서") matches NFD stored message (ko-3)
            let nfc_hits = store.search_messages("오늘보고서", 20).unwrap();
            assert_eq!(
                nfc_hits.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["ko-3"]
            );

            // 3. Korean Choseong search:
            //    - "ㅎㅇ" matches both "확인" (ko-3, newer) and "회의실" (ko-2, older)
            //    - "ㅎㅇㅅ" uniquely matches "회의실" (ko-2)
            //    - "ㄱㅁㅅ" matches sender "김민수" (ko-3, ko-1)
            let cho_hy = store.search_messages("ㅎㅇ", 20).unwrap();
            assert_eq!(
                cho_hy
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-3", "ko-2"]
            );

            let cho_meeting = store.search_messages("ㅎㅇㅅ", 20).unwrap();
            assert_eq!(
                cho_meeting
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-2"]
            );

            let cho_sender = store.search_messages("ㄱㅁㅅ", 20).unwrap();
            assert_eq!(
                cho_sender
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-3", "ko-1"]
            );

            // Mixed Choseong + Hangul multi-token AND
            let cho_mixed = store.search_messages("ㄱㅁㅅ 오늘", 20).unwrap();
            assert_eq!(
                cho_mixed
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-3"]
            );

            // 4. Vietnamese NFC + NFD accent-insensitive search
            let vi_ascii = store.search_messages("nguyen minh da nang", 20).unwrap();
            assert_eq!(
                vi_ascii.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["vi-1"]
            );
            let vi_nfd = store
                .search_messages("Ba\u{0301}o ca\u{0301}o ti\u{0065}\u{0302}\u{0301}n", 20)
                .unwrap();
            assert_eq!(
                vi_nfd.iter().map(|m| m.msg_id.as_str()).collect::<Vec<_>>(),
                vec!["vi-1"]
            );
        }

        // Re-open DB to verify `rebuild_in_memory_fts` preserves NFD->NFC Hangul & Choseong search
        {
            let store = MessageStore::open(&db_path).unwrap();
            assert_eq!(
                store
                    .search_messages("보고서", 20)
                    .unwrap()
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-3", "ko-1"]
            );
            assert_eq!(
                store
                    .search_messages("ㅎㅇㅅ", 20)
                    .unwrap()
                    .iter()
                    .map(|m| m.msg_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["ko-2"]
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_optimistic_poll_message_removes_synthetic_rows_and_prevents_duplicates() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-poll-dedupe-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("poll_dedupe.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let make_poll_msg = |msg_id: &str, poll_id: i64, text: &str, ts: i64| MessageDto {
            msg_id: msg_id.to_string(),
            cli_msg_id: None,
            thread_id: "grp-1".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: text.to_string(),
            msg_type: "chat.poll".into(),
            ts,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{{\"poll_id\":{poll_id},\"question\":\"Lunch?\",\"options\":[]}}"
            )),
        };

        // Insert synthetic poll-99, local-poll-99, timestamp local-poll-1700000001234 (same poll_id 99),
        // plus an unrelated poll-100 that must NOT be touched.
        store
            .upsert_message(&make_poll_msg("poll-99", 99, "Bình chọn tạm 99", 1_700_000_100_000))
            .unwrap();
        store
            .upsert_message(&make_poll_msg(
                "local-poll-99",
                99,
                "Bình chọn cục bộ 99",
                1_700_000_100_500,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_msg(
                "local-poll-1700000001234",
                99,
                "Bình chọn lạc quan 99",
                1_700_000_101_000,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_msg(
                "poll-100",
                100,
                "Bình chọn khác 100",
                1_700_000_102_000,
            ))
            .unwrap();

        let real_msg = make_poll_msg(
            "srv-msg-9901",
            99,
            "Bình chọn thật 99",
            1_700_000_105_000,
        );
        store
            .replace_optimistic_poll_message("grp-1", 99, &real_msg)
            .unwrap();

        // Synthetic rows for poll 99 are gone from both `messages` and `messages_fts`.
        assert!(store.get_message("grp-1", "poll-99").unwrap().is_none());
        assert!(store.get_message("grp-1", "local-poll-99").unwrap().is_none());
        assert!(store
            .get_message("grp-1", "local-poll-1700000001234")
            .unwrap()
            .is_none());
        assert!(store.search_messages("binh chon tam 99", 10).unwrap().is_empty());
        assert!(store
            .search_messages("binh chon cuc bo 99", 10)
            .unwrap()
            .is_empty());
        assert!(store
            .search_messages("binh chon lac quan 99", 10)
            .unwrap()
            .is_empty());

        // Real server message is persisted & searchable; unrelated poll-100 is untouched.
        assert_eq!(
            store
                .get_message("grp-1", "srv-msg-9901")
                .unwrap()
                .unwrap()
                .text,
            "Bình chọn thật 99"
        );
        assert_eq!(
            store
                .search_messages("binh chon that 99", 10)
                .unwrap()
                .len(),
            1
        );
        assert!(store.get_message("grp-1", "poll-100").unwrap().is_some());

        // In-place update when real_msg.msg_id == "poll-100" keeps the row intact without self-deleting.
        let updated_100 = make_poll_msg(
            "poll-100",
            100,
            "Bình chọn cập nhật 100",
            1_700_000_106_000,
        );
        store
            .replace_optimistic_poll_message("grp-1", 100, &updated_100)
            .unwrap();
        assert_eq!(
            store.get_message("grp-1", "poll-100").unwrap().unwrap().text,
            "Bình chọn cập nhật 100"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_or_merge_poll_message_merges_real_votes_in_place_and_preserves_flags() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-poll-upsert-merge-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("poll_upsert_merge.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let make_poll = |msg_id: &str,
                         sender_id: &str,
                         sender_name: &str,
                         is_outgoing: bool,
                         ts: i64,
                         text: &str,
                         poll: &PollDto| {
            let poll_json = serde_json::to_string(poll).unwrap();
            MessageDto {
                msg_id: msg_id.to_string(),
                cli_msg_id: None,
                thread_id: "grp-poll".into(),
                sender_id: sender_id.to_string(),
                sender_name: sender_name.to_string(),
                text: text.to_string(),
                msg_type: "chat.poll".into(),
                ts,
                is_outgoing,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: Some(format!("teza-poll:{poll_json}")),
            }
        };

        // 1. Insert optimistic synthetic card with full flags & options.
        let syn_poll = PollDto {
            poll_id: 55,
            question: "Trưa ăn gì?".into(),
            options: vec![
                crate::chat::PollOptionDto {
                    id: 1,
                    content: "Phở".into(),
                    votes: 0,
                    voted: false,
                },
                crate::chat::PollOptionDto {
                    id: 2,
                    content: "Bún chả".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: true,
            expired_time: 1_800_000_000_000,
            closed: false,
        };
        let syn_msg = make_poll(
            "local-poll-55",
            "u-alice",
            "Alice",
            true,
            1_700_000_100_000,
            "Bình chọn: Trưa ăn gì?",
            &syn_poll,
        );
        store.upsert_message(&syn_msg).unwrap();

        // 2. Case 2: Sparse real server creation message M1 arrives -> replaces synthetic,
        //    preserves synthetic flags/options, returns was_updated_existing = false.
        let sparse_m1_poll = PollDto {
            poll_id: 55,
            question: "Trưa ăn gì?".into(),
            options: Vec::new(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let m1 = make_poll(
            "M1",
            "u-alice",
            "Alice",
            true,
            1_700_000_101_000,
            "Bình chọn",
            &sparse_m1_poll,
        );
        let (saved_m1, updated_existing) = store
            .upsert_or_merge_poll_message("grp-poll", 55, &m1)
            .unwrap();
        assert!(!updated_existing);
        assert_eq!(saved_m1.msg_id, "M1");
        assert_eq!(saved_m1.text, "Bình chọn: Trưa ăn gì?");
        assert!(store.get_message("grp-poll", "local-poll-55").unwrap().is_none());
        let dto_after_m1 =
            parse_poll_dto_from_attachment_name(saved_m1.attachment_name.as_deref()).unwrap();
        assert!(dto_after_m1.allow_multi);
        assert!(dto_after_m1.allow_add_option);
        assert!(dto_after_m1.is_anonymous);
        assert_eq!(dto_after_m1.options.len(), 2);

        // 3. Case 1 (different server msgId M2 on poll vote): merges INTO canonical M1 in place!
        let vote_m2_poll = PollDto {
            poll_id: 55,
            question: "Bình chọn".into(),
            options: vec![
                crate::chat::PollOptionDto {
                    id: 1,
                    content: "Phở".into(),
                    votes: 2,
                    voted: true,
                },
                crate::chat::PollOptionDto {
                    id: 2,
                    content: "Bún chả".into(),
                    votes: 1,
                    voted: false,
                },
            ],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let m2 = make_poll(
            "M2",
            "u-bob",
            "Bob",
            false,
            1_700_000_200_000,
            "Bob đã bình chọn",
            &vote_m2_poll,
        );
        let (saved_after_m2, updated_existing) = store
            .upsert_or_merge_poll_message("grp-poll", 55, &m2)
            .unwrap();
        assert!(updated_existing);
        assert_eq!(saved_after_m2.msg_id, "M1");
        assert_eq!(saved_after_m2.ts, 1_700_000_101_000);
        assert_eq!(saved_after_m2.sender_id, "u-alice");
        assert_eq!(saved_after_m2.sender_name, "Alice");
        assert!(saved_after_m2.is_outgoing);
        assert_eq!(saved_after_m2.text, "Bình chọn: Trưa ăn gì?");
        assert!(store.get_message("grp-poll", "M2").unwrap().is_none());
        let dto_after_m2 =
            parse_poll_dto_from_attachment_name(saved_after_m2.attachment_name.as_deref()).unwrap();
        assert_eq!(dto_after_m2.question, "Trưa ăn gì?");
        assert_eq!(dto_after_m2.options[0].votes, 2);
        assert!(dto_after_m2.options[0].voted);
        assert!(dto_after_m2.allow_multi);
        assert!(dto_after_m2.allow_add_option);
        assert!(dto_after_m2.is_anonymous);

        // 4. Case 1 (same msgId M1 updated with closed=true): updates in place, returns true.
        let mut closed_poll = dto_after_m2.clone();
        closed_poll.closed = true;
        let m1_closed = make_poll(
            "M1",
            "u-alice",
            "Alice",
            true,
            1_700_000_300_000,
            "Bình chọn: Trưa ăn gì?",
            &closed_poll,
        );
        let (saved_closed, updated_existing) = store
            .upsert_or_merge_poll_message("grp-poll", 55, &m1_closed)
            .unwrap();
        assert!(updated_existing);
        assert_eq!(saved_closed.msg_id, "M1");
        assert_eq!(saved_closed.ts, 1_700_000_101_000);
        assert!(
            parse_poll_dto_from_attachment_name(saved_closed.attachment_name.as_deref())
                .unwrap()
                .closed
        );

        // 5. Case 1 (synthetic row arrives after real M1 exists): never inserts synthetic, merges into M1.
        let late_syn = make_poll(
            "poll-55",
            "u-alice",
            "Alice",
            true,
            1_700_000_400_000,
            "Bình chọn: Trưa ăn gì?",
            &syn_poll,
        );
        let (saved_after_syn, updated_existing) = store
            .upsert_or_merge_poll_message("grp-poll", 55, &late_syn)
            .unwrap();
        assert!(updated_existing);
        assert_eq!(saved_after_syn.msg_id, "M1");
        assert!(store.get_message("grp-poll", "poll-55").unwrap().is_none());
        assert!(
            parse_poll_dto_from_attachment_name(saved_after_syn.attachment_name.as_deref())
                .unwrap()
                .closed
        );

        // Exactly 1 message exists in the entire thread.
        let all_msgs = store.load_messages("grp-poll", None, 50).unwrap();
        assert_eq!(all_msgs.len(), 1);
        assert_eq!(all_msgs[0].msg_id, "M1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupe_duplicate_poll_messages_in_thread_collapses_preexisting_duplicates() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-poll-dedupe-thread-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("poll_dedupe_thread.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        let make_poll_row = |msg_id: &str, sender: &str, ts: i64, text: &str, poll: &PollDto| {
            MessageDto {
                msg_id: msg_id.to_string(),
                cli_msg_id: None,
                thread_id: "grp-dup".into(),
                sender_id: format!("u-{sender}"),
                sender_name: sender.to_string(),
                text: text.to_string(),
                msg_type: "chat.poll".into(),
                ts,
                is_outgoing: sender == "Alice",
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: Some(format!(
                    "teza-poll:{}",
                    serde_json::to_string(poll).unwrap()
                )),
            }
        };

        let p88_v1 = PollDto {
            poll_id: 88,
            question: "Đi chơi cuối tuần?".into(),
            options: vec![crate::chat::PollOptionDto {
                id: 1,
                content: "Đà Lạt".into(),
                votes: 1,
                voted: true,
            }],
            allow_multi: true,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let p88_v2 = PollDto {
            poll_id: 88,
            question: "Bình chọn".into(),
            options: vec![crate::chat::PollOptionDto {
                id: 1,
                content: "Đà Lạt".into(),
                votes: 3,
                voted: true,
            }],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let p88_v3 = PollDto {
            poll_id: 88,
            question: "Bình chọn".into(),
            options: vec![crate::chat::PollOptionDto {
                id: 1,
                content: "Đà Lạt".into(),
                votes: 4,
                voted: true,
            }],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: true,
        };
        let p89 = PollDto {
            poll_id: 89,
            question: "Họp thứ mấy?".into(),
            options: Vec::new(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };

        // Pre-populate duplicate rows for poll 88 (synthetic + 3 real server rows),
        // plus an unrelated poll 89 and a regular text message.
        store
            .upsert_message(&make_poll_row(
                "poll-88",
                "Alice",
                1_700_000_099_000,
                "Bình chọn tạm 88",
                &p88_v1,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_row(
                "M1",
                "Alice",
                1_700_000_100_000,
                "Bình chọn: Đi chơi cuối tuần?",
                &p88_v1,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_row(
                "M2",
                "Bob",
                1_700_000_200_000,
                "Bình chọn dup M2",
                &p88_v2,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_row(
                "M3",
                "Carol",
                1_700_000_300_000,
                "Bình chọn dup M3",
                &p88_v3,
            ))
            .unwrap();
        store
            .upsert_message(&make_poll_row(
                "M89",
                "Alice",
                1_700_000_400_000,
                "Bình chọn: Họp thứ mấy?",
                &p89,
            ))
            .unwrap();
        store
            .upsert_message(&MessageDto {
                msg_id: "TXT1".into(),
                cli_msg_id: None,
                thread_id: "grp-dup".into(),
                sender_id: "u-bob".into(),
                sender_name: "Bob".into(),
                text: "Chốt lịch nhé".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_500_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            })
            .unwrap();

        let removed = store
            .dedupe_duplicate_poll_messages_in_thread("grp-dup")
            .unwrap();
        assert_eq!(removed, 3);

        // Second run is a clean no-op.
        assert_eq!(
            store
                .dedupe_duplicate_poll_messages_in_thread("grp-dup")
                .unwrap(),
            0
        );

        let msgs = store.load_messages("grp-dup", None, 50).unwrap();
        let ids: Vec<&str> = msgs.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(ids, vec!["M1", "M89", "TXT1"]);

        // Canonical M1 kept earliest identity & merged newest votes/closed/flags.
        assert_eq!(msgs[0].sender_name, "Alice");
        assert_eq!(msgs[0].ts, 1_700_000_100_000);
        assert_eq!(msgs[0].text, "Bình chọn: Đi chơi cuối tuần?");
        let merged_88 =
            parse_poll_dto_from_attachment_name(msgs[0].attachment_name.as_deref()).unwrap();
        assert_eq!(merged_88.options[0].votes, 4);
        assert!(merged_88.allow_multi);
        assert!(merged_88.closed);

        // Duplicate FTS entries were cleaned too.
        assert!(store.search_messages("dup M2", 10).unwrap().is_empty());
        assert!(store.search_messages("dup M3", 10).unwrap().is_empty());
        assert!(store.search_messages("binh chon tam 88", 10).unwrap().is_empty());
        assert_eq!(
            store.search_messages("di choi cuoi tuan", 10).unwrap().len(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stranger_thread_fallback_meta_profile_upsert_ensure_and_unresolved_listing() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-stranger-threads-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("stranger_threads.db");
        let store = MessageStore::open(&db_path).unwrap();

        // 1. Brand-new stub uses real fallback_title + fallback_avatar instead of raw ID.
        let created = store
            .update_thread_preview_with_fallback_meta(
                "900001",
                "Xin chào shop",
                1_700_000_100_000,
                true,
                false,
                Some("Anh Minh"),
                Some("https://avt.zdn.vn/minh.jpg"),
            )
            .unwrap();
        assert!(created);
        let t1 = store.get_thread_row("900001").unwrap().unwrap();
        assert_eq!(t1.title, "Anh Minh");
        assert_eq!(t1.avatar, "https://avt.zdn.vn/minh.jpg");
        assert_eq!(t1.last_preview, "Xin chào shop");
        assert_eq!(t1.unread_count, 1);

        // 2. Numeric fallback_title falls back to thread_id; later real fallback upgrades it.
        let created2 = store
            .update_thread_preview_with_fallback_meta(
                "900002",
                "Alo",
                1_700_000_200_000,
                true,
                false,
                Some("900002"),
                Some(""),
            )
            .unwrap();
        assert!(created2);
        let t2_stub = store.get_thread_row("900002").unwrap().unwrap();
        assert_eq!(t2_stub.title, "900002");
        assert!(t2_stub.avatar.is_empty());

        let created2_again = store
            .update_thread_preview_with_fallback_meta(
                "900002",
                "Còn hàng không?",
                1_700_000_205_000,
                true,
                false,
                Some("Chị Lan"),
                Some("https://avt.zdn.vn/lan.jpg"),
            )
            .unwrap();
        assert!(!created2_again);
        let t2_upgraded = store.get_thread_row("900002").unwrap().unwrap();
        assert_eq!(t2_upgraded.title, "Chị Lan");
        assert_eq!(t2_upgraded.avatar, "https://avt.zdn.vn/lan.jpg");
        assert_eq!(t2_upgraded.unread_count, 2);

        // Existing real title/avatar are NOT overwritten by fallback meta on subsequent previews.
        let _ = store
            .update_thread_preview_with_fallback_meta(
                "900002",
                "Cảm ơn nhé",
                1_700_000_210_000,
                true,
                false,
                Some("Tên Khác"),
                Some("https://avt.zdn.vn/other.jpg"),
            )
            .unwrap();
        let t2_kept = store.get_thread_row("900002").unwrap().unwrap();
        assert_eq!(t2_kept.title, "Chị Lan");
        assert_eq!(t2_kept.avatar, "https://avt.zdn.vn/lan.jpg");

        // 3. upsert_thread_profile_meta updates title/avatar without clobbering thread state.
        store.set_thread_pinned("900001", true).unwrap();
        store.set_thread_muted("900001", true).unwrap();
        store.set_thread_archived("900001", true).unwrap();
        assert!(!store
            .upsert_thread_profile_meta("nonexistent", "Nobody", "https://avt.zdn.vn/x.jpg")
            .unwrap());
        // Raw numeric title + empty avatar must NOT downgrade real profile.
        assert!(!store
            .upsert_thread_profile_meta("900001", "900001", "")
            .unwrap());
        assert!(store
            .upsert_thread_profile_meta(
                "900001",
                "Anh Minh (VIP)",
                "https://avt.zdn.vn/minh_new.jpg"
            )
            .unwrap());
        let t1_after = store.get_thread_row("900001").unwrap().unwrap();
        assert_eq!(t1_after.title, "Anh Minh (VIP)");
        assert_eq!(t1_after.avatar, "https://avt.zdn.vn/minh_new.jpg");
        assert_eq!(t1_after.last_preview, "Xin chào shop");
        assert_eq!(t1_after.last_ts, 1_700_000_100_000);
        assert_eq!(t1_after.unread_count, 1);
        assert!(t1_after.is_pinned);
        assert!(t1_after.is_muted);
        assert!(t1_after.is_archived);
        assert_eq!(t1_after.kind, ThreadKind::Direct);

        // 4. ensure_direct_thread creates missing thread (last_ts=0, unread=0) and updates existing safely.
        let ensured_new = store
            .ensure_direct_thread("900003", "Khách Mới", "https://avt.zdn.vn/new.jpg")
            .unwrap();
        assert_eq!(ensured_new.id, "900003");
        assert_eq!(ensured_new.title, "Khách Mới");
        assert_eq!(ensured_new.avatar, "https://avt.zdn.vn/new.jpg");
        assert_eq!(ensured_new.kind, ThreadKind::Direct);
        assert_eq!(ensured_new.last_ts, 0);
        assert_eq!(ensured_new.unread_count, 0);

        let ensured_existing = store
            .ensure_direct_thread("900001", "Anh Minh (VIP 2)", "")
            .unwrap();
        assert_eq!(ensured_existing.title, "Anh Minh (VIP 2)");
        assert_eq!(ensured_existing.avatar, "https://avt.zdn.vn/minh_new.jpg");
        assert_eq!(ensured_existing.last_preview, "Xin chào shop");
        assert_eq!(ensured_existing.last_ts, 1_700_000_100_000);
        assert_eq!(ensured_existing.unread_count, 1);
        assert!(ensured_existing.is_pinned);

        // 5. list_unresolved_direct_thread_ids filters cloud_*/g*/group/resolved & orders by last_ts DESC.
        let make_thread = |id: &str, title: &str, avatar: &str, kind: ThreadKind, last_ts: i64| {
            ThreadDto {
                id: id.into(),
                title: title.into(),
                avatar: avatar.into(),
                kind,
                last_preview: "hi".into(),
                last_ts,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            }
        };
        store
            .upsert_thread(&make_thread(
                "cloud_me",
                "",
                "",
                ThreadKind::Direct,
                1_700_000_999_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "g123456",
                "",
                "",
                ThreadKind::Direct,
                1_700_000_998_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "777000",
                "",
                "",
                ThreadKind::Group,
                1_700_000_997_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "800001",
                "800001",
                "https://avt.zdn.vn/1.jpg",
                ThreadKind::Direct,
                1_700_000_900_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "800002",
                "Người Không Avatar",
                "",
                ThreadKind::Direct,
                1_700_000_800_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "800003",
                "",
                "https://avt.zdn.vn/3.jpg",
                ThreadKind::Direct,
                1_700_000_700_000,
            ))
            .unwrap();
        store
            .upsert_thread(&make_thread(
                "800004",
                "999888777",
                "https://avt.zdn.vn/4.jpg",
                ThreadKind::Direct,
                1_700_000_600_000,
            ))
            .unwrap();

        assert_eq!(
            store.list_unresolved_direct_thread_ids(2).unwrap(),
            vec!["800001".to_string(), "800002".to_string()]
        );
        assert_eq!(
            store.list_unresolved_direct_thread_ids(20).unwrap(),
            vec![
                "800001".to_string(),
                "800002".to_string(),
                "800003".to_string(),
                "800004".to_string(),
            ]
        );
        assert!(store
            .list_unresolved_direct_thread_ids(0)
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suffixed_numeric_uid_title_is_treated_as_placeholder_rejected_as_fallback_and_rehydrated() {
        let dir = std::env::temp_dir().join(format!(
            "litechat-store-suffixed-uid-placeholder-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("suffixed_uid_placeholder.db");
        let store = MessageStore::open(&db_path).unwrap();

        // Pure helper checks: numeric and `<digits>_<digits>` match; real names with `_` do not.
        assert!(is_raw_numeric_uid_like("777001"));
        assert!(is_raw_numeric_uid_like("777001_0"));
        assert!(is_raw_numeric_uid_like(" 777001_12 "));
        assert!(!is_raw_numeric_uid_like(""));
        assert!(!is_raw_numeric_uid_like("   "));
        assert!(!is_raw_numeric_uid_like("minh_anh"));
        assert!(!is_raw_numeric_uid_like("user_123"));
        assert!(!is_raw_numeric_uid_like("123_abc"));
        assert!(!is_raw_numeric_uid_like("777001_"));
        assert!(!is_raw_numeric_uid_like("_0"));
        assert!(is_placeholder_thread_title("777001_0", "777001"));
        assert!(!is_placeholder_thread_title("minh_anh", "777001"));

        // 1. `"777001_0"` is rejected as `fallback_title` when inserting a stub (falls back to thread_id).
        let created = store
            .update_thread_preview_with_fallback_meta(
                "777001",
                "Xin chào",
                1_700_000_100_000,
                true,
                false,
                Some("777001_0"),
                Some("https://avt.zdn.vn/777001.jpg"),
            )
            .unwrap();
        assert!(created);
        let stub = store.get_thread_row("777001").unwrap().unwrap();
        assert_eq!(stub.title, "777001");
        assert_eq!(stub.avatar, "https://avt.zdn.vn/777001.jpg");

        // 2. A thread whose stored title is `"777001_0"` (even with an avatar) is treated as
        //    unresolved by `list_unresolved_direct_thread_ids`.
        store
            .upsert_thread(&ThreadDto {
                id: "777002".into(),
                title: "777001_0".into(),
                avatar: "https://avt.zdn.vn/777002.jpg".into(),
                kind: ThreadKind::Direct,
                last_preview: "Alo".into(),
                last_ts: 1_700_000_200_000,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            })
            .unwrap();
        assert_eq!(
            store.list_unresolved_direct_thread_ids(10).unwrap(),
            vec!["777002".to_string(), "777001".to_string()]
        );

        // 3. When a real name arrives via `update_thread_preview_with_fallback_meta`, `"777001_0"`
        //    is upgraded cleanly and leaves `list_unresolved_direct_thread_ids`.
        let created_again = store
            .update_thread_preview_with_fallback_meta(
                "777002",
                "Gửi anh báo giá",
                1_700_000_205_000,
                true,
                false,
                Some("minh_anh"),
                None,
            )
            .unwrap();
        assert!(!created_again);
        let upgraded = store.get_thread_row("777002").unwrap().unwrap();
        assert_eq!(upgraded.title, "minh_anh");
        assert_eq!(upgraded.avatar, "https://avt.zdn.vn/777002.jpg");

        // 4. `upsert_thread_profile_meta` rejects downgrading a real name to `"777001_0"`,
        //    and upgrades `"777001"` to a real name so zero unresolved threads remain.
        assert!(!store
            .upsert_thread_profile_meta("777002", "777001_0", "")
            .unwrap());
        assert_eq!(
            store.get_thread_row("777002").unwrap().unwrap().title,
            "minh_anh"
        );
        assert!(store
            .upsert_thread_profile_meta("777001", "Trần Quốc Bảo", "")
            .unwrap());
        assert_eq!(
            store.get_thread_row("777001").unwrap().unwrap().title,
            "Trần Quốc Bảo"
        );
        assert!(store
            .list_unresolved_direct_thread_ids(10)
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recall_of_old_synced_message_exports_via_updated_ts_and_cleans_media_on_import() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-recall-delta-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = MessageStore::open(&dir.join("src.db")).unwrap();
        let dst = MessageStore::open(&dir.join("dst.db")).unwrap();

        let make_msg = |mid: &str, text: &str, ts: i64, att: Option<&str>| MessageDto {
            msg_id: mid.into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: text.into(),
            msg_type: if att.is_some() { "chat.photo" } else { "chat.text" }.into(),
            ts,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: att.map(str::to_string),
            attachment_name: att.map(|_| "secret.jpg".into()),
        };

        src.upsert_message(&make_msg(
            "m1",
            "Confidential photo",
            1_600_000_001_000,
            Some("https://f10-zpc.zdn.vn/secret.jpg"),
        ))
        .unwrap();
        src.upsert_message(&make_msg("m2", "Followup", 1_600_000_002_000, None))
            .unwrap();
        src.upsert_cached_translations(
            &[(
                "m1".into(),
                "Confidential photo".into(),
                TranslationResult {
                    msg_id: "m1".into(),
                    translated_text: "Anh bi mat".into(),
                    detected_source_lang: Some("EN".into()),
                    skipped: false,
                },
            )],
            "VI",
            "deepl",
        )
        .unwrap();

        let sha_m1 = "a".repeat(64);
        src.upsert_media_object(&MediaObjectRecord {
            sha256: sha_m1.clone(),
            kind: "image".into(),
            mime: "image/jpeg".into(),
            size_bytes: 50_000,
            thumb_sha256: None,
            width: None,
            height: None,
            duration_ms: None,
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some("blobs/aa/aa/src_m1.enc".into()),
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1_600_000_001_000,
            last_accessed_ts: 1_600_000_001_000,
            cloud_provider: Some("gdrive".into()),
            cloud_file_id: Some("drv-m1-secret".into()),
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: Some(1_600_000_001_500),
        })
        .unwrap();
        src.link_message_media(&MessageMediaRecord {
            thread_id: "t1".into(),
            msg_id: "m1".into(),
            variant: "primary".into(),
            url_hash: "uh-m1".into(),
            sha256: Some(sha_m1.clone()),
            original_name: Some("secret.jpg".into()),
            saved_user_path: None,
            cdn_status: MEDIA_CDN_STATUS_OK,
            updated_ts: 1_600_000_001_500,
        })
        .unwrap();

        // Initial delta sync
        let delta1 = src
            .export_history_delta_since_with_mutation_cursor("u1", "dev-A", 0, 0, 500)
            .unwrap();
        assert_eq!(delta1.messages.len(), 2);
        assert_eq!(delta1.cloud_media.len(), 1);
        assert_eq!(delta1.max_message_ts_cursor, Some(1_600_000_002_000));
        assert_eq!(delta1.max_mutation_ts_cursor, None);

        let imp1 = dst.import_history_delta_idempotent_detailed(&delta1).unwrap();
        assert_eq!(imp1.applied_messages, 2);
        // Simulate dst having also hydrated the blob locally on disk
        dst.upsert_media_object(&MediaObjectRecord {
            sha256: sha_m1.clone(),
            kind: "image".into(),
            mime: "image/jpeg".into(),
            size_bytes: 50_000,
            thumb_sha256: None,
            width: None,
            height: None,
            duration_ms: None,
            local_state: MEDIA_LOCAL_STATE_LOCAL,
            local_rel_path: Some("blobs/aa/aa/dst_m1.enc".into()),
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1_600_000_001_000,
            last_accessed_ts: 1_600_000_001_000,
            cloud_provider: Some("gdrive".into()),
            cloud_file_id: Some("drv-m1-secret".into()),
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: Some(1_600_000_001_500),
        })
        .unwrap();

        // Recall OLD m1 (ts < cursor_ts = 1_600_000_002_001) on src
        let recalled = src.mark_recalled("t1", "m1").unwrap().unwrap();
        assert!(recalled.is_recalled);

        // Cloud delete queue must contain m1's blob on src!
        let pending_del = src.list_pending_cloud_deletes(10).unwrap();
        assert_eq!(pending_del.len(), 1);
        assert_eq!(pending_del[0].sha256, sha_m1);
        assert_eq!(pending_del[0].cloud_file_id, "drv-m1-secret");

        // Export since (cursor_ts=1_600_000_002_001, mut_cursor=0) MUST include old recalled m1!
        let delta2 = src
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-A",
                1_600_000_002_001,
                0,
                500,
            )
            .unwrap();
        assert_eq!(delta2.messages.len(), 1);
        assert_eq!(delta2.messages[0].msg_id, "m1");
        assert!(delta2.messages[0].is_recalled);
        assert!(delta2.messages[0].attachment_url.is_none());
        assert_eq!(delta2.max_message_ts_cursor, None);
        assert!(delta2.max_mutation_ts_cursor.is_some());

        // Import recall delta on dst -> overwrites m1, deletes translation & media, returns local orphan path!
        let imp2 = dst.import_history_delta_idempotent_detailed(&delta2).unwrap();
        assert_eq!(imp2.applied_messages, 1);
        assert_eq!(imp2.orphaned_local_paths, vec!["blobs/aa/aa/dst_m1.enc"]);
        let dst_m1 = dst.get_message("t1", "m1").unwrap().unwrap();
        assert!(dst_m1.is_recalled);
        assert!(dst_m1.attachment_url.is_none());
        assert!(dst
            .get_message_media_record("t1", "m1", "primary")
            .unwrap()
            .is_none());
        // Remote recall evicts local blob AND queues cloud deletion for known cloud_file_id
        let dst_obj = dst.get_media_object(&sha_m1).unwrap().unwrap();
        assert_eq!(dst_obj.local_state, MEDIA_LOCAL_STATE_MISSING);
        assert!(dst_obj.local_rel_path.is_none());
        let dst_pending = dst.list_pending_cloud_deletes(10).unwrap();
        assert_eq!(dst_pending.len(), 1);
        assert_eq!(dst_pending[0].sha256, sha_m1);
        assert_eq!(dst_pending[0].cloud_file_id, "drv-m1-secret");
        dst.complete_pending_cloud_delete_for_file(&sha_m1, "drv-m1-secret")
            .unwrap();
        assert!(dst.get_media_object(&sha_m1).unwrap().is_none());
        // Importing recall must NOT trigger echo re-export on dst (updated_ts stays 0)
        let dst_echo = dst
            .export_history_delta_since_with_mutation_cursor("u1", "dev-B", 1_600_000_002_001, 0, 500)
            .unwrap();
        assert!(dst_echo.messages.is_empty());

        // Re-importing OLD pre-recall delta1 on dst must NOT un-recall m1 OR re-link cloud_media!
        let imp_old = dst.import_history_delta_idempotent_detailed(&delta1).unwrap();
        assert_eq!(imp_old.applied_messages, 0);
        assert!(dst.get_message("t1", "m1").unwrap().unwrap().is_recalled);
        assert!(dst
            .get_message_media_record("t1", "m1", "primary")
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_message_and_delete_thread_tombstones_sync_prevent_resurrection_and_handle_rejoin() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-tombstones-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = MessageStore::open(&dir.join("src.db")).unwrap();
        let dst_in_order = MessageStore::open(&dir.join("dst_in_order.db")).unwrap();
        let dst_ooo = MessageStore::open(&dir.join("dst_ooo.db")).unwrap();

        let make_thread = |id: &str, title: &str, last_ts: i64| ThreadDto {
            id: id.into(),
            title: title.into(),
            avatar: String::new(),
            kind: ThreadKind::Group,
            last_preview: "hi".into(),
            last_ts,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        };
        let make_msg = |tid: &str, mid: &str, text: &str, ts: i64| MessageDto {
            msg_id: mid.into(),
            cli_msg_id: None,
            thread_id: tid.into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: text.into(),
            msg_type: "chat.text".into(),
            ts,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        };

        src.upsert_thread(&make_thread("t_keep", "Keep", 1_600_000_010_000))
            .unwrap();
        src.upsert_thread(&make_thread("g_leave", "Leave Group", 1_600_000_020_000))
            .unwrap();
        src.save_chat_folder(&ChatFolderDto {
            id: "f-work".into(),
            name: "Work".into(),
            icon: "folder".into(),
            color: String::new(),
            thread_ids: vec!["t_keep".into(), "g_leave".into()],
            excluded_thread_ids: Vec::new(),
            include_kinds: Vec::new(),
            only_unread: false,
            exclude_muted: false,
            position: 0,
            remote_label_id: None,
        })
        .unwrap();
        src.upsert_message(&make_msg("t_keep", "m_keep", "keep me", 1_600_000_005_000))
            .unwrap();
        src.upsert_message(&make_msg("t_keep", "m_del", "delete me", 1_600_000_010_000))
            .unwrap();
        src.upsert_message(&make_msg("g_leave", "m_grp", "group msg", 1_600_000_020_000))
            .unwrap();

        let delta1 = src
            .export_history_delta_since_with_mutation_cursor("u1", "dev-A", 0, 0, 500)
            .unwrap();

        // Delete m_del locally and leave g_leave
        assert!(src.delete_message("t_keep", "m_del").unwrap());
        let _ = src.delete_thread("g_leave").unwrap();

        let delta2 = src
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-A",
                1_600_000_020_001,
                0,
                500,
            )
            .unwrap();
        assert_eq!(delta2.deleted_messages.len(), 1);
        assert_eq!(delta2.deleted_messages[0].msg_id, "m_del");
        assert_eq!(delta2.deleted_threads.len(), 1);
        assert_eq!(delta2.deleted_threads[0].thread_id, "g_leave");
        let thread_del_ts = delta2.deleted_threads[0].deleted_ts;

        // 1) In-order restore (delta1 -> delta2)
        dst_in_order
            .import_history_delta_idempotent_detailed(&delta1)
            .unwrap();
        let rep2 = dst_in_order
            .import_history_delta_idempotent_detailed(&delta2)
            .unwrap();
        assert_eq!(rep2.deleted_messages, 1);
        assert_eq!(rep2.deleted_threads, 1);
        assert!(dst_in_order.get_message("t_keep", "m_keep").unwrap().is_some());
        assert!(dst_in_order.get_message("t_keep", "m_del").unwrap().is_none());
        assert!(dst_in_order.get_message("g_leave", "m_grp").unwrap().is_none());
        assert_eq!(dst_in_order.list_threads(FolderFilter::All).unwrap().len(), 1);
        assert_eq!(
            dst_in_order.list_chat_folders().unwrap()[0].thread_ids,
            vec!["t_keep".to_string()]
        );
        // Must not echo imported tombstones back out
        let echo_batch = dst_in_order
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-B",
                1_600_000_020_001,
                0,
                500,
            )
            .unwrap();
        assert!(echo_batch.deleted_messages.is_empty());
        assert!(echo_batch.deleted_threads.is_empty());

        // 2) Out-of-order restore (delta2 -> delta1) MUST converge to exact same state!
        dst_ooo
            .import_history_delta_idempotent_detailed(&delta2)
            .unwrap();
        dst_ooo
            .import_history_delta_idempotent_detailed(&delta1)
            .unwrap();
        assert!(dst_ooo.get_message("t_keep", "m_keep").unwrap().is_some());
        assert!(dst_ooo.get_message("t_keep", "m_del").unwrap().is_none());
        assert!(dst_ooo.get_message("g_leave", "m_grp").unwrap().is_none());
        assert_eq!(dst_ooo.list_threads(FolderFilter::All).unwrap().len(), 1);
        assert_eq!(
            dst_ooo.list_chat_folders().unwrap()[0].thread_ids,
            vec!["t_keep".to_string()]
        );

        // 3) Re-joining g_leave later (timestamps strictly after thread_del_ts) MUST succeed while keeping old m_grp blocked!
        let rejoin_ts = thread_del_ts + 5_000;
        let delta3 = HistoryDeltaBatch {
            version: HistoryDeltaBatch::CURRENT_VERSION,
            uid: "u1".into(),
            device_id: "dev-A".into(),
            exported_ts: rejoin_ts,
            threads: vec![make_thread("g_leave", "Leave Group (Rejoined)", rejoin_ts)],
            messages: vec![
                make_msg("g_leave", "m_grp", "stale pre-leave msg", 1_600_000_020_000),
                make_msg("g_leave", "m_rejoin", "welcome back!", rejoin_ts),
            ],
            folders: Vec::new(),
            thread_folders: Vec::new(),
            translations: Vec::new(),
            cloud_media: Vec::new(),
            deleted_messages: Vec::new(),
            deleted_threads: Vec::new(),
            messages_truncated: false,
            cloud_media_truncated: false,
            mutations_truncated: false,
            max_message_ts_cursor: None,
            max_cloud_media_cursor_ts: None,
            max_mutation_ts_cursor: None,
        };
        dst_ooo
            .import_history_delta_idempotent_detailed(&delta3)
            .unwrap();
        assert!(dst_ooo.get_message("g_leave", "m_grp").unwrap().is_none());
        assert_eq!(
            dst_ooo
                .get_message("g_leave", "m_rejoin")
                .unwrap()
                .unwrap()
                .text,
            "welcome back!"
        );
        assert_eq!(dst_ooo.list_threads(FolderFilter::All).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_cloud_delete_queue_respects_shared_blobs_relinks_and_remote_recall_upsert() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-cloud-del-queue-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("q.db")).unwrap();

        let sha_shared = "e".repeat(64);
        let sha_remote = "f".repeat(64);
        for (sha, cid, rel) in [
            (&sha_shared, "drv-shared", "blobs/ee/ee/shared.enc"),
            (&sha_remote, "drv-remote", "blobs/ff/ff/remote.enc"),
        ] {
            store
                .upsert_media_object(&MediaObjectRecord {
                    sha256: sha.clone(),
                    kind: "image".into(),
                    mime: "image/png".into(),
                    size_bytes: 12_345,
                    thumb_sha256: None,
                    width: None,
                    height: None,
                    duration_ms: None,
                    local_state: MEDIA_LOCAL_STATE_LOCAL,
                    local_rel_path: Some(rel.into()),
                    is_encrypted: true,
                    pin_count: 0,
                    created_ts: 1_000,
                    last_accessed_ts: 1_000,
                    cloud_provider: Some("gdrive".into()),
                    cloud_file_id: Some(cid.into()),
                    cloud_encrypted: true,
                    cloud_key_ver: 1,
                    cloud_synced_ts: Some(2_000),
                })
                .unwrap();
        }

        let make_link = |mid: &str, sha: &str| MessageMediaRecord {
            thread_id: "t1".into(),
            msg_id: mid.into(),
            variant: "primary".into(),
            url_hash: format!("uh-{mid}"),
            sha256: Some(sha.into()),
            original_name: None,
            saved_user_path: None,
            cdn_status: MEDIA_CDN_STATUS_OK,
            updated_ts: 2_000,
        };
        store.link_message_media(&make_link("m1", &sha_shared)).unwrap();
        store.link_message_media(&make_link("m2", &sha_shared)).unwrap();
        store.link_message_media(&make_link("m_rem", &sha_remote)).unwrap();

        // Removing m1 keeps sha_shared referenced by m2 -> NOT queued
        let _ = store.cleanup_orphaned_media_for_msg("t1", "m1").unwrap();
        assert!(store.list_pending_cloud_deletes(10).unwrap().is_empty());

        // Removing m2 orphans sha_shared -> queued!
        let _ = store.cleanup_orphaned_media_for_msg("t1", "m2").unwrap();
        let queued = store.list_pending_cloud_deletes(10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].sha256, sha_shared);
        assert!(store.is_media_unreferenced(&sha_shared).unwrap());

        // Re-linking sha_shared to m3 cancels the queued cloud delete automatically!
        store.link_message_media(&make_link("m3", &sha_shared)).unwrap();
        assert!(store.list_pending_cloud_deletes(10).unwrap().is_empty());
        assert!(!store.is_media_unreferenced(&sha_shared).unwrap());

        // Remote recall via upsert_message_returning_orphans cleans local blob AND queues cloud delete!
        let mut msg_rem = MessageDto {
            msg_id: "m_rem".into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u2".into(),
            sender_name: "Bob".into(),
            text: "photo".into(),
            msg_type: "chat.photo".into(),
            ts: 1_500,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: Some("https://f10-zpc.zdn.vn/r.png".into()),
            attachment_name: Some("r.png".into()),
        };
        store.upsert_message(&msg_rem).unwrap();
        msg_rem.is_recalled = true;
        msg_rem.text = "[Tin nhắn đã thu hồi]".into();
        let orphans = store.upsert_message_returning_orphans(&msg_rem).unwrap();
        assert_eq!(orphans, vec!["blobs/ff/ff/remote.enc"]);

        let queued2 = store.list_pending_cloud_deletes(10).unwrap();
        assert_eq!(queued2.len(), 1);
        assert_eq!(queued2[0].sha256, sha_remote);
        assert_eq!(queued2[0].cloud_file_id, "drv-remote");

        // Completing pending cloud delete removes queue row & drops unreferenced evicted media_objects row
        store.complete_pending_cloud_delete(&sha_remote).unwrap();
        assert!(store.list_pending_cloud_deletes(10).unwrap().is_empty());
        assert!(store.get_media_object(&sha_remote).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_media_uses_mutation_cursor_and_monotonic_mutation_seq_never_collides() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-cm-mut-cursor-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("cm_mut.db")).unwrap();

        let make_msg = |mid: &str, ts: i64| MessageDto {
            msg_id: mid.into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: format!("msg {mid}"),
            msg_type: "chat.photo".into(),
            ts,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: Some("https://f10-zpc.zdn.vn/a.jpg".into()),
            attachment_name: Some("a.jpg".into()),
        };
        store.upsert_message(&make_msg("m1", 1_600_000_001_000)).unwrap();

        let sha = "a".repeat(64);
        store
            .upsert_media_object(&MediaObjectRecord {
                sha256: sha.clone(),
                kind: "image".into(),
                mime: "image/jpeg".into(),
                size_bytes: 1234,
                thumb_sha256: None,
                width: None,
                height: None,
                duration_ms: None,
                local_state: MEDIA_LOCAL_STATE_LOCAL,
                local_rel_path: Some("blobs/aa/aa/a.enc".into()),
                is_encrypted: true,
                pin_count: 0,
                created_ts: 1_500_000_000_000,
                last_accessed_ts: 1_500_000_000_000,
                cloud_provider: None,
                cloud_file_id: None,
                cloud_encrypted: true,
                cloud_key_ver: 1,
                cloud_synced_ts: None,
            })
            .unwrap();
        store
            .link_message_media(&MessageMediaRecord {
                thread_id: "t1".into(),
                msg_id: "m1".into(),
                variant: "primary".into(),
                url_hash: "uh-a".into(),
                sha256: Some(sha.clone()),
                original_name: Some("a.jpg".into()),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: 1_500_000_000_000,
            })
            .unwrap();

        // Simulate PC clock behind server cursor (`since_ts` far ahead of `cloud_synced_ts`):
        // `mark_media_cloud_synced` allocates monotonic ts >= now_ms and `cloud_media` filters by `since_mutation_ts`!
        store
            .mark_media_cloud_synced(&sha, "gdrive", "drv-a", 1, 1_500_000_000_500)
            .unwrap();
        let synced_ts = store
            .get_media_object(&sha)
            .unwrap()
            .unwrap()
            .cloud_synced_ts
            .unwrap();
        assert!(synced_ts > 1_500_000_000_500);

        // Even when server `since_ts` is far ahead of `synced_ts`, `since_mutation_ts = 0` STILL exports cloud_media!
        let batch = store
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-A",
                synced_ts + 999_999_999,
                0,
                500,
            )
            .unwrap();
        assert!(batch.messages.is_empty());
        assert_eq!(batch.max_message_ts_cursor, None);
        assert_eq!(batch.cloud_media.len(), 1);
        assert_eq!(batch.max_cloud_media_cursor_ts, Some(synced_ts));

        // Once mutation cursor advances past `synced_ts`, `cloud_media` is NOT re-exported;
        // also `mark_media_cdn_status` must NOT bump `updated_ts`!
        store
            .mark_media_cdn_status("uh-a", MEDIA_CDN_STATUS_EXPIRED)
            .unwrap();
        let batch_after = store
            .export_history_delta_since_with_mutation_cursor("u1", "dev-A", 0, synced_ts + 1, 500)
            .unwrap();
        assert!(batch_after.cloud_media.is_empty());

        // Strictly monotonic `next_local_mutation_ts`: rapid calls in the same ms (and even when
        // wall clock is behind `gdrive_last_mutation_sync_ts`) never collide!
        store
            .set_gdrive_last_mutation_sync_ts(synced_ts + 10_000_000)
            .unwrap();
        let m1 = store.next_local_mutation_ts().unwrap();
        let m2 = store.next_local_mutation_ts().unwrap();
        let m3 = store.next_local_mutation_ts().unwrap();
        assert!(m1 >= synced_ts + 10_000_000);
        assert_eq!(m2, m1 + 1);
        assert_eq!(m3, m2 + 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_message_prevents_unrecall_and_sets_insert_updated_ts_for_backfill_and_recall() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-upsert-recall-backfill-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = MessageStore::open(&dir.join("ur.db")).unwrap();

        let orig = MessageDto {
            msg_id: "m1".into(),
            cli_msg_id: None,
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: "Top secret payload".into(),
            msg_type: "chat.photo".into(),
            ts: 1_600_000_010_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: Some("https://f10-zpc.zdn.vn/secret.jpg".into()),
            attachment_name: Some("secret.jpg".into()),
        };
        store.upsert_message(&orig).unwrap();
        store.mark_recalled("t1", "m1").unwrap();

        let recalled_before = store.get_message("t1", "m1").unwrap().unwrap();
        assert!(recalled_before.is_recalled);
        let mut_cursor_after_recall = store
            .export_history_delta_since_with_mutation_cursor("u1", "dev-A", 1_600_000_020_000, 0, 500)
            .unwrap()
            .max_mutation_ts_cursor
            .unwrap();

        // Stale non-recalled packet arrives -> MUST NOT un-recall, restore text/attachments, corrupt FTS, or bump updated_ts!
        store.upsert_message(&orig).unwrap();
        let after_stale = store.get_message("t1", "m1").unwrap().unwrap();
        assert!(after_stale.is_recalled);
        assert_eq!(after_stale.text, "[Tin nhắn đã thu hồi]");
        assert!(after_stale.attachment_url.is_none());
        assert!(after_stale.attachment_name.is_none());
        assert!(store.search_messages("Top secret", 10).unwrap().is_empty());
        let no_echo = store
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-A",
                1_600_000_020_000,
                mut_cursor_after_recall + 1,
                500,
            )
            .unwrap();
        assert!(no_echo.messages.is_empty());

        // Set `gdrive_last_history_sync_ts` ahead, then insert a scrolled-back old message AND a brand-new recalled message:
        // both MUST receive `updated_ts > 0` on INSERT so they export via `since_mutation_ts`!
        store
            .set_gdrive_last_history_sync_ts(1_600_000_050_000)
            .unwrap();
        let mut backfilled = orig.clone();
        backfilled.msg_id = "m_old_backfill".into();
        backfilled.text = "Scrolled back message".into();
        backfilled.ts = 1_600_000_001_000;
        store.upsert_message(&backfilled).unwrap();

        let mut new_recalled = orig.clone();
        new_recalled.msg_id = "m_new_recalled".into();
        new_recalled.text = "[Tin nhắn đã thu hồi]".into();
        new_recalled.ts = 1_600_000_002_000;
        new_recalled.is_recalled = true;
        store.upsert_message(&new_recalled).unwrap();

        let exported = store
            .export_history_delta_since_with_mutation_cursor(
                "u1",
                "dev-A",
                1_600_000_050_000,
                mut_cursor_after_recall + 1,
                500,
            )
            .unwrap();
        let ids: Vec<&str> = exported.messages.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(ids, vec!["m_old_backfill", "m_new_recalled"]);
        assert!(exported.messages[1].is_recalled);
        assert!(exported.messages[1].attachment_url.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_thread_anchors_to_server_clock_and_step5_blocks_zombie_cloud_media() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-thread-tomb-skew-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = MessageStore::open(&dir.join("src.db")).unwrap();
        let dst = MessageStore::open(&dir.join("dst.db")).unwrap();

        // Thread with server timestamps far behind current PC wall-clock (simulating PC clock lead!)
        src.upsert_thread(&ThreadDto {
            id: "g1".into(),
            title: "Team Chat".into(),
            avatar: String::new(),
            kind: ThreadKind::Group,
            last_preview: "old".into(),
            last_ts: 1_600_000_010_000,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        })
        .unwrap();
        src.upsert_message(&MessageDto {
            msg_id: "m_old".into(),
            cli_msg_id: None,
            thread_id: "g1".into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: "old".into(),
            msg_type: "chat.text".into(),
            ts: 1_600_000_010_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        })
        .unwrap();

        let _ = src.delete_thread("g1").unwrap();
        let tomb_batch = src
            .export_history_delta_since_with_mutation_cursor("u1", "dev-A", 1_600_000_020_000, 0, 500)
            .unwrap();
        assert_eq!(tomb_batch.deleted_threads.len(), 1);
        // MUST anchor to server clock 1_600_000_010_000, NOT PC wall-clock (~1.79e12)!
        assert_eq!(tomb_batch.deleted_threads[0].deleted_ts, 1_600_000_010_000);

        dst.import_history_delta_idempotent_detailed(&tomb_batch)
            .unwrap();

        // Post-rejoin message at server_ts = 1_600_000_015_000 (still behind PC wall-clock!) MUST survive,
        // while a late cloud_media record (with PC wall-clock cloud_synced_ts > del_ts!) for deleted `m_old`
        // MUST be rejected (no zombie message_media/media_objects resurrection)!
        let sha_zombie = "9".repeat(64);
        let late_batch = HistoryDeltaBatch {
            version: HistoryDeltaBatch::CURRENT_VERSION,
            uid: "u1".into(),
            device_id: "dev-B".into(),
            exported_ts: 1_900_000_000_000,
            threads: vec![ThreadDto {
                id: "g1".into(),
                title: "Team Chat".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: "rejoined".into(),
                last_ts: 1_600_000_015_000,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            }],
            messages: vec![MessageDto {
                msg_id: "m_rejoin".into(),
                cli_msg_id: None,
                thread_id: "g1".into(),
                sender_id: "u2".into(),
                sender_name: "Bob".into(),
                text: "rejoined".into(),
                msg_type: "chat.text".into(),
                ts: 1_600_000_015_000,
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            }],
            folders: Vec::new(),
            thread_folders: Vec::new(),
            translations: Vec::new(),
            cloud_media: vec![HistoryCloudMediaRecord {
                thread_id: "g1".into(),
                msg_id: "m_old".into(),
                variant: "primary".into(),
                url_hash: "uh-zombie".into(),
                sha256: sha_zombie.clone(),
                kind: "image".into(),
                mime: "image/jpeg".into(),
                size_bytes: 4096,
                original_name: Some("zombie.jpg".into()),
                cloud_provider: "gdrive".into(),
                cloud_file_id: "drv-zombie".into(),
                cloud_key_ver: 1,
                cloud_synced_ts: 1_900_000_000_000,
            }],
            deleted_messages: Vec::new(),
            deleted_threads: Vec::new(),
            messages_truncated: false,
            cloud_media_truncated: false,
            mutations_truncated: false,
            max_message_ts_cursor: None,
            max_cloud_media_cursor_ts: None,
            max_mutation_ts_cursor: None,
        };
        dst.import_history_delta_idempotent_detailed(&late_batch)
            .unwrap();
        assert!(dst.get_message("g1", "m_rejoin").unwrap().is_some());
        assert!(dst.get_message("g1", "m_old").unwrap().is_none());
        assert!(dst
            .get_message_media_record("g1", "m_old", "primary")
            .unwrap()
            .is_none());
        assert!(dst.get_media_object(&sha_zombie).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_delete_backoff_poison_drop_qualified_complete_upload_backoff_and_vault_reset() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-queue-backoff-reset-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = MessageStore::open(&dir.join("qbr.db")).unwrap();

        let sha_poison = "1".repeat(64);
        let sha_ok = "2".repeat(64);
        for (sha, cid, rel) in [
            (&sha_poison, "drv-poison", "blobs/11/11/p.enc"),
            (&sha_ok, "drv-ok", "blobs/22/22/o.enc"),
        ] {
            store
                .upsert_media_object(&MediaObjectRecord {
                    sha256: sha.clone(),
                    kind: "image".into(),
                    mime: "image/jpeg".into(),
                    size_bytes: 1000,
                    thumb_sha256: None,
                    width: None,
                    height: None,
                    duration_ms: None,
                    local_state: MEDIA_LOCAL_STATE_LOCAL,
                    local_rel_path: Some(rel.into()),
                    is_encrypted: true,
                    pin_count: 0,
                    created_ts: 1_000,
                    last_accessed_ts: 1_000,
                    cloud_provider: Some("gdrive".into()),
                    cloud_file_id: Some(cid.into()),
                    cloud_encrypted: true,
                    cloud_key_ver: 1,
                    cloud_synced_ts: Some(1_000),
                })
                .unwrap();
            store
                .link_message_media(&MessageMediaRecord {
                    thread_id: "t1".into(),
                    msg_id: format!("m-{cid}"),
                    variant: "primary".into(),
                    url_hash: format!("uh-{cid}"),
                    sha256: Some(sha.clone()),
                    original_name: None,
                    saved_user_path: None,
                    cdn_status: MEDIA_CDN_STATUS_OK,
                    updated_ts: 1_000,
                })
                .unwrap();
            let _ = store
                .cleanup_orphaned_media_for_msg("t1", &format!("m-{cid}"))
                .unwrap();
        }

        // Before any history pull, `require_history_pull = true` returns empty!
        let now = chrono::Utc::now().timestamp_millis();
        assert!(store
            .list_ready_pending_cloud_deletes(10, true, 0, now)
            .unwrap()
            .is_empty());

        // After history pull completes, both items become ready
        store.record_gdrive_history_pull_completed(now).unwrap();
        let ready = store
            .list_ready_pending_cloud_deletes(10, true, 0, now)
            .unwrap();
        assert_eq!(ready.len(), 2);

        // Recording 1st permanent 403 failure backs off `sha_poison`, unblocking `sha_ok` immediately!
        assert!(!store
            .record_pending_cloud_delete_failure(&sha_poison, "drv-poison", true, now)
            .unwrap());
        let ready_after_1 = store
            .list_ready_pending_cloud_deletes(10, true, 0, now)
            .unwrap();
        assert_eq!(ready_after_1.len(), 1);
        assert_eq!(ready_after_1[0].sha256, sha_ok);

        // Attempts 2 and 3 drop permanent poison row completely
        assert!(!store
            .record_pending_cloud_delete_failure(&sha_poison, "drv-poison", true, now + 120_000)
            .unwrap());
        assert!(store
            .record_pending_cloud_delete_failure(&sha_poison, "drv-poison", true, now + 500_000)
            .unwrap());
        assert!(store
            .list_ready_pending_cloud_deletes(10, true, 0, now + 10_000_000)
            .unwrap()
            .iter()
            .all(|r| r.sha256 != sha_poison));

        // Qualified `complete_pending_cloud_delete_for_file` MUST NOT clobber a newer `cloud_file_id`!
        store
            .mark_media_cloud_synced(&sha_ok, "gdrive", "drv-ok-newer", 1, now)
            .unwrap();
        store
            .complete_pending_cloud_delete_for_file(&sha_ok, "drv-ok")
            .unwrap();
        assert_eq!(
            store
                .get_media_object(&sha_ok)
                .unwrap()
                .unwrap()
                .cloud_file_id
                .as_deref(),
            Some("drv-ok-newer")
        );

        // Pending upload backoff isolates failing local blob so healthy newer blob is returned first
        let sha_bad_up = "3".repeat(64);
        let sha_good_up = "4".repeat(64);
        for (sha, created_ts) in [(&sha_bad_up, 1_000i64), (&sha_good_up, 2_000i64)] {
            store
                .upsert_media_object(&MediaObjectRecord {
                    sha256: sha.clone(),
                    kind: "image".into(),
                    mime: "image/jpeg".into(),
                    size_bytes: 512,
                    thumb_sha256: None,
                    width: None,
                    height: None,
                    duration_ms: None,
                    local_state: MEDIA_LOCAL_STATE_LOCAL,
                    local_rel_path: Some(format!("blobs/33/33/{sha}.enc")),
                    is_encrypted: true,
                    pin_count: 0,
                    created_ts,
                    last_accessed_ts: created_ts,
                    cloud_provider: None,
                    cloud_file_id: None,
                    cloud_encrypted: true,
                    cloud_key_ver: 1,
                    cloud_synced_ts: None,
                })
                .unwrap();
        }
        store
            .record_media_cloud_upload_failure(&sha_bad_up, now)
            .unwrap();
        let pending_up = store.list_pending_cloud_uploads(10).unwrap();
        assert_eq!(pending_up.len(), 1);
        assert_eq!(pending_up[0].sha256, sha_good_up);

        // Vault recovery reset clears stale cloud links (preserving local blobs), drops orphan
        // cloud-only rows, clears applied deltas, and resets sync cursors
        store
            .mark_media_cloud_synced(&sha_good_up, "gdrive", "drv-good-old", 1, now)
            .unwrap();
        store.set_gdrive_last_history_sync_ts(999).unwrap();
        store.set_gdrive_last_mutation_sync_ts(888).unwrap();
        store.mark_delta_applied("d1", now).unwrap();
        store
            .reset_gdrive_sync_state_for_vault_recovery(true, true)
            .unwrap();
        assert_eq!(store.get_gdrive_last_history_sync_ts().unwrap(), Some(0));
        assert_eq!(store.get_gdrive_last_mutation_sync_ts().unwrap(), Some(0));
        assert!(!store.is_delta_applied("d1").unwrap());
        assert!(store.get_media_object(&sha_ok).unwrap().is_none());
        let good_after = store.get_media_object(&sha_good_up).unwrap().unwrap();
        assert_eq!(good_after.local_state, MEDIA_LOCAL_STATE_LOCAL);
        assert!(good_after.cloud_file_id.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_recalled_backfill_and_ai_session_validation_trim_same_ms_title() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-backfill-ai-52-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("bf.db");

        {
            let mut store = MessageStore::open(&db_path).unwrap();
            store
                .upsert_message(&MessageDto {
                    msg_id: "m_rec".into(),
                    cli_msg_id: None,
                    thread_id: "t1".into(),
                    sender_id: "u1".into(),
                    sender_name: "Alice".into(),
                    text: "[Tin nhắn đã thu hồi]".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_600_000_001_000,
                    is_outgoing: true,
                    is_recalled: true,
                    is_pinned: false,
                    quote: None,
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                })
                .unwrap();
            // Simulate a crash between `ALTER TABLE messages ADD COLUMN updated_ts` and the backfill UPDATE:
            store
                .conn
                .execute(
                    "UPDATE messages SET updated_ts = 0 WHERE msg_id = 'm_rec'",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "DELETE FROM store_meta WHERE k = 'backfilled_recalled_updated_ts_v1'",
                    [],
                )
                .unwrap();
        }

        // Reopening MUST atomically detect missing sentinel and backfill `updated_ts > 0`!
        let store = MessageStore::open(&db_path).unwrap();
        let upd_ts: i64 = store
            .conn
            .query_row(
                "SELECT updated_ts FROM messages WHERE msg_id = 'm_rec'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(upd_ts > 0);

        // P3 #52: `insert_ai_message` rejects >64-char session_id; `list_ai_messages`/`clear_ai_messages` trim;
        // same-ms model reply derives title from legacy un-backfilled user message at identical ms!
        let too_long_sid = "s".repeat(65);
        assert!(store
            .insert_ai_message(&AiStoredMessageDto {
                id: "ai-long".into(),
                session_id: too_long_sid,
                role: "user".into(),
                content: "hello".into(),
                model: "m".into(),
                backend: "cloud".into(),
                ts: 1_700_000_000_000,
            })
            .is_err());

        // Seed an un-backfilled legacy user row in `ai_messages` without an `ai_sessions` row,
        // then insert a `model` reply sharing the EXACT same millisecond.
        let enc_legacy_user = store
            .cipher
            .encrypt_str(&ai_msg_aad("u-same-ms"), "Hỏi về lịch họp tuần sau")
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO ai_messages (id, session_id, role, content, model, backend, ts)
                 VALUES ('u-same-ms', 'legacy-same', 'user', ?1, 'm', 'cloud', 1700000005000)",
                params![enc_legacy_user],
            )
            .unwrap();
        store
            .insert_ai_message(&AiStoredMessageDto {
                id: "m-same-ms".into(),
                session_id: "  legacy-same  ".into(),
                role: "model".into(),
                content: "Lịch họp vào thứ Hai lúc 9h.".into(),
                model: "m".into(),
                backend: "cloud".into(),
                ts: 1_700_000_005_000,
            })
            .unwrap();

        let sessions = store.list_ai_sessions(10).unwrap();
        let sess = sessions.iter().find(|s| s.id == "legacy-same").unwrap();
        assert_eq!(sess.title, "Hỏi về lịch họp tuần sau");

        let listed = store.list_ai_messages("  legacy-same  ", 10).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(store.clear_ai_messages("  legacy-same  ").unwrap(), 2);
        assert!(store.list_ai_messages("legacy-same", 10).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Debug, Clone, Copy)]
    struct MockUsEastern2026;

    impl MockUsEastern2026 {
        fn est() -> chrono::FixedOffset {
            chrono::FixedOffset::west_opt(5 * 3600).unwrap()
        }
        fn edt() -> chrono::FixedOffset {
            chrono::FixedOffset::west_opt(4 * 3600).unwrap()
        }
    }

    impl TimeZone for MockUsEastern2026 {
        type Offset = chrono::FixedOffset;

        fn from_offset(_offset: &Self::Offset) -> Self {
            MockUsEastern2026
        }

        fn offset_from_local_date(
            &self,
            local: &NaiveDate,
        ) -> chrono::LocalResult<Self::Offset> {
            let start = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
            let end = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
            if *local >= start && *local < end {
                chrono::LocalResult::Single(Self::edt())
            } else {
                chrono::LocalResult::Single(Self::est())
            }
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> chrono::LocalResult<Self::Offset> {
            let spring_start = NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap();
            let spring_end = NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(3, 0, 0)
                .unwrap();
            let fall_start = NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(1, 0, 0)
                .unwrap();
            let fall_end = NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap();

            if *local < spring_start {
                chrono::LocalResult::Single(Self::est())
            } else if *local < spring_end {
                chrono::LocalResult::None
            } else if *local < fall_start {
                chrono::LocalResult::Single(Self::edt())
            } else if *local < fall_end {
                chrono::LocalResult::Ambiguous(Self::edt(), Self::est())
            } else {
                chrono::LocalResult::Single(Self::est())
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> Self::Offset {
            let start = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
            let end = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
            if *utc >= start && *utc < end {
                Self::edt()
            } else {
                Self::est()
            }
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> Self::Offset {
            let spring_utc = NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(7, 0, 0)
                .unwrap();
            let fall_utc = NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(6, 0, 0)
                .unwrap();
            if *utc >= spring_utc && *utc < fall_utc {
                Self::edt()
            } else {
                Self::est()
            }
        }
    }

    #[test]
    fn compute_next_occurrence_preserves_anchor_day_utc_plus_7_and_dst_wall_clock() {
        let vn = chrono::FixedOffset::east_opt(7 * 3600).unwrap();

        // Bug #8 (Vietnam UTC+7 1st-of-month): Mar 1 01:00 +07:00 (= Feb 28 18:00Z) MUST roll
        // to Apr 1 01:00 +07:00, NEVER Mar 29!
        let mar1_vn = DateTime::parse_from_rfc3339("2026-03-01T01:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        let apr1_vn = DateTime::parse_from_rfc3339("2026-04-01T01:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, mar1_vn, Some(1), "monthly", mar1_vn),
            Some(apr1_vn)
        );
        // Even with anchor_day = None, derives day 1 in +07:00 (not Feb 28 in UTC!)
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, mar1_vn, None, "monthly", mar1_vn),
            Some(apr1_vn)
        );

        // May 1 06:00 +07:00 (= Apr 30 23:00Z) MUST roll to Jun 1 06:00 +07:00, NEVER May 31!
        let may1_vn = DateTime::parse_from_rfc3339("2026-05-01T06:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        let jun1_vn = DateTime::parse_from_rfc3339("2026-06-01T06:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, may1_vn, Some(1), "monthly", may1_vn),
            Some(jun1_vn)
        );

        // Bug #8 (anchor_day = 31): Jan 31 -> Feb 28 -> Mar 31 -> Apr 30 -> May 31 (no drift!)
        let jan31_vn = DateTime::parse_from_rfc3339("2026-01-31T09:30:00+07:00")
            .unwrap()
            .timestamp_millis();
        let feb28_vn = DateTime::parse_from_rfc3339("2026-02-28T09:30:00+07:00")
            .unwrap()
            .timestamp_millis();
        let mar31_vn = DateTime::parse_from_rfc3339("2026-03-31T09:30:00+07:00")
            .unwrap()
            .timestamp_millis();
        let apr30_vn = DateTime::parse_from_rfc3339("2026-04-30T09:30:00+07:00")
            .unwrap()
            .timestamp_millis();
        let may31_vn = DateTime::parse_from_rfc3339("2026-05-31T09:30:00+07:00")
            .unwrap()
            .timestamp_millis();

        assert_eq!(
            compute_next_occurrence_in_tz(&vn, jan31_vn, Some(31), "monthly", jan31_vn),
            Some(feb28_vn)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, feb28_vn, Some(31), "monthly", feb28_vn),
            Some(mar31_vn)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, mar31_vn, Some(31), "monthly", mar31_vn),
            Some(apr30_vn)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, apr30_vn, Some(31), "monthly", apr30_vn),
            Some(may31_vn)
        );

        // Leap year 2028: Jan 31 -> Feb 29 -> Mar 31
        let jan31_2028_vn = DateTime::parse_from_rfc3339("2028-01-31T08:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        let feb29_2028_vn = DateTime::parse_from_rfc3339("2028-02-29T08:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        let mar31_2028_vn = DateTime::parse_from_rfc3339("2028-03-31T08:00:00+07:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, jan31_2028_vn, Some(31), "monthly", jan31_2028_vn),
            Some(feb29_2028_vn)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&vn, feb29_2028_vn, Some(31), "monthly", feb29_2028_vn),
            Some(mar31_2028_vn)
        );

        // Bug #8 (DST preservation): daily/weekly across spring-forward (23h) & fall-back (25h)
        let tz = MockUsEastern2026;
        let mar7_0900 = DateTime::parse_from_rfc3339("2026-03-07T09:00:00-05:00")
            .unwrap()
            .timestamp_millis();
        let mar8_0900 = DateTime::parse_from_rfc3339("2026-03-08T09:00:00-04:00")
            .unwrap()
            .timestamp_millis();
        let mar14_0900 = DateTime::parse_from_rfc3339("2026-03-14T09:00:00-04:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(mar8_0900 - mar7_0900, 23 * 3600 * 1000);
        assert_eq!(
            compute_next_occurrence_in_tz(&tz, mar7_0900, None, "daily", mar7_0900),
            Some(mar8_0900)
        );
        assert_eq!(
            compute_next_occurrence_in_tz(&tz, mar7_0900, None, "weekly", mar7_0900),
            Some(mar14_0900)
        );

        let oct31_0900 = DateTime::parse_from_rfc3339("2026-10-31T09:00:00-04:00")
            .unwrap()
            .timestamp_millis();
        let nov1_0900 = DateTime::parse_from_rfc3339("2026-11-01T09:00:00-05:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(nov1_0900 - oct31_0900, 25 * 3600 * 1000);
        assert_eq!(
            compute_next_occurrence_in_tz(&tz, oct31_0900, None, "daily", oct31_0900),
            Some(nov1_0900)
        );

        // DST spring-forward non-existent gap (02:30 on Mar 8) resolves to 03:00 EDT, NOT None!
        let mar7_0230 = DateTime::parse_from_rfc3339("2026-03-07T02:30:00-05:00")
            .unwrap()
            .timestamp_millis();
        let mar8_0300 = DateTime::parse_from_rfc3339("2026-03-08T03:00:00-04:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&tz, mar7_0230, None, "daily", mar7_0230),
            Some(mar8_0300)
        );

        // DST fall-back ambiguous hour (01:30 on Nov 1) resolves cleanly to earliest (01:30 EDT).
        let oct31_0130 = DateTime::parse_from_rfc3339("2026-10-31T01:30:00-04:00")
            .unwrap()
            .timestamp_millis();
        let nov1_0130_edt = DateTime::parse_from_rfc3339("2026-11-01T01:30:00-04:00")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            compute_next_occurrence_in_tz(&tz, oct31_0130, None, "daily", oct31_0130),
            Some(nov1_0130_edt)
        );
    }

    #[test]
    fn scheduled_messages_cadence_revert_edit_reset_delete_guard_filter_and_migration() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-sched-ux-fixes-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("sched_ux.db");
        let store = MessageStore::open(&db_path).unwrap();

        // 1. Monthly anchor_day = 31 survives Jan 31 -> Feb 28 -> Mar 31 via complete_scheduled_message,
        //    even when retry backoff and reschedule_now occur!
        let jan31_local = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 1, 31)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        let feb28_local = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 2, 28)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        let mar31_local = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 3, 31)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();

        let monthly = store
            .create_scheduled_message(&NewScheduledMessageInput {
                thread_id: "t-monthly".into(),
                thread_title: "Kế toán".into(),
                thread_kind: "group".into(),
                text: "Chốt lương cuối tháng".into(),
                attachment_path: String::new(),
                send_at_ts: jan31_local,
                recurrence: "monthly".into(),
                misfire_policy: "ask".into(),
                misfire_grace_secs: 600,
            })
            .unwrap();
        assert_eq!(monthly.anchor_day, Some(31));
        assert_eq!(monthly.next_attempt_ts, None);
        assert!(!monthly.bypass_misfire);

        // Claim at jan31_local, hit transient error -> retry sets next_attempt_ts (+30s) without mutating send_at_ts!
        let claimed = store
            .claim_due_scheduled_messages(jan31_local, 10)
            .unwrap();
        assert_eq!(claimed.len(), 1);
        // #67d: delete_scheduled_message MUST reject while status == 'sending'
        assert!(!store.delete_scheduled_message(&monthly.id).unwrap());
        assert!(store.get_scheduled_message(&monthly.id).unwrap().is_some());

        // #5 helper: revert_scheduled_message_to_pending reverts 'sending' -> 'pending' WITHOUT incrementing retry_count
        let reverted = store
            .revert_scheduled_message_to_pending(&monthly.id)
            .unwrap()
            .unwrap();
        assert_eq!(reverted.status, "pending");
        assert_eq!(reverted.retry_count, 0);
        assert_eq!(reverted.next_attempt_ts, None);
        assert!(!reverted.bypass_misfire);
        // Re-calling revert when not 'sending' returns None
        assert!(store
            .revert_scheduled_message_to_pending(&monthly.id)
            .unwrap()
            .is_none());

        // Claim again, fail with retry at +30s
        assert_eq!(
            store
                .claim_due_scheduled_messages(jan31_local, 10)
                .unwrap()
                .len(),
            1
        );
        let retried = store
            .fail_or_retry_scheduled_message(
                &monthly.id,
                "network blip",
                Some(jan31_local + 30_000),
                jan31_local + 1_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(retried.send_at_ts, jan31_local);
        assert_eq!(retried.next_attempt_ts, Some(jan31_local + 30_000));
        assert_eq!(retried.retry_count, 1);
        // Scheduler next_pending_schedule_ts respects next_attempt_ts and claim skips before +30s!
        assert_eq!(
            store.next_pending_schedule_ts().unwrap(),
            Some(jan31_local + 30_000)
        );
        assert!(store
            .claim_due_scheduled_messages(jan31_local + 29_999, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .claim_due_scheduled_messages(jan31_local + 30_000, 10)
                .unwrap()
                .len(),
            1
        );

        // Complete at +35s -> rolls to Feb 28 09:00 (NOT 09:00:30!) and keeps anchor_day = 31!
        let after_jan = store
            .complete_scheduled_message(&monthly.id, jan31_local + 35_000)
            .unwrap()
            .unwrap();
        assert_eq!(after_jan.status, "pending");
        assert_eq!(after_jan.send_at_ts, feb28_local);
        assert_eq!(after_jan.anchor_day, Some(31));
        assert_eq!(after_jan.next_attempt_ts, None);
        assert!(!after_jan.bypass_misfire);

        // Suppose Feb 28 misfires -> user clicks "Gửi ngay" at 14:37 -> send_at_ts stays Feb 28 09:00!
        let feb28_late = feb28_local + 5 * 3600 * 1000 + 37 * 60 * 1000;
        let _ = store
            .mark_scheduled_message_missed(&monthly.id, "Quá giờ", feb28_late)
            .unwrap();
        let resched = store
            .reschedule_now(&monthly.id, feb28_late + 1_000)
            .unwrap()
            .unwrap();
        assert_eq!(resched.status, "pending");
        assert_eq!(resched.send_at_ts, feb28_local);
        assert_eq!(resched.next_attempt_ts, Some(feb28_late + 1_000));
        assert!(resched.bypass_misfire);
        assert_eq!(resched.retry_count, 0);
        assert!(resched.last_error.is_empty());

        // Completing "Gửi ngay" rolls cleanly to Mar 31 09:00 using anchor_day = 31!
        let after_feb = store
            .complete_scheduled_message(&monthly.id, feb28_late + 2_000)
            .unwrap()
            .unwrap();
        assert_eq!(after_feb.send_at_ts, mar31_local);
        assert_eq!(after_feb.anchor_day, Some(31));
        assert_eq!(after_feb.next_attempt_ts, None);
        assert!(!after_feb.bypass_misfire);

        // 2. #67c: Updating send_at_ts or recurrence on a pending retrying item resets retry state
        //    and updates anchor_day; updating text alone leaves retry state untouched.
        let _ = store
            .fail_or_retry_scheduled_message(
                &monthly.id,
                "temp err",
                Some(mar31_local + 60_000),
                mar31_local + 1_000,
            )
            .unwrap();
        let text_only = store
            .update_scheduled_message(
                &monthly.id,
                &UpdateScheduledMessageInput {
                    text: Some("Chốt lương (đã sửa nội dung)".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(text_only.retry_count, 1);
        assert_eq!(text_only.last_error, "temp err");
        assert_eq!(text_only.next_attempt_ts, Some(mar31_local + 60_000));

        let apr15_local = resolve_local_datetime_in_tz(
            &Local,
            NaiveDate::from_ymd_opt(2026, 4, 15)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
        )
        .unwrap()
        .timestamp_millis();
        let ts_changed = store
            .update_scheduled_message(
                &monthly.id,
                &UpdateScheduledMessageInput {
                    send_at_ts: Some(apr15_local),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(ts_changed.retry_count, 0);
        assert!(ts_changed.last_error.is_empty());
        assert_eq!(ts_changed.next_attempt_ts, None);
        assert!(!ts_changed.bypass_misfire);
        assert_eq!(ts_changed.anchor_day, Some(15));

        // Now set retry state again and change ONLY recurrence -> also resets retry state!
        let _ = store
            .fail_or_retry_scheduled_message(
                &monthly.id,
                "temp err 2",
                Some(apr15_local + 60_000),
                apr15_local + 1_000,
            )
            .unwrap();
        let rec_changed = store
            .update_scheduled_message(
                &monthly.id,
                &UpdateScheduledMessageInput {
                    recurrence: Some("weekly".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(rec_changed.recurrence, "weekly");
        assert_eq!(rec_changed.retry_count, 0);
        assert!(rec_changed.last_error.is_empty());
        assert_eq!(rec_changed.next_attempt_ts, None);
        assert!(!rec_changed.bypass_misfire);

        // 3. #37: list_scheduled_messages_filtered applies allowed_thread_ids BEFORE LIMIT
        for i in 1..=3_i64 {
            store
                .create_scheduled_message(&NewScheduledMessageInput {
                    thread_id: "t-other".into(),
                    thread_title: "Other".into(),
                    thread_kind: "direct".into(),
                    text: format!("other {i}"),
                    attachment_path: String::new(),
                    send_at_ts: 1_750_000_000_000 + i * 1_000,
                    recurrence: "once".into(),
                    misfire_policy: "send_on_resume".into(),
                    misfire_grace_secs: 600,
                })
                .unwrap();
        }
        for i in 1..=2_i64 {
            store
                .create_scheduled_message(&NewScheduledMessageInput {
                    thread_id: "t-allowed".into(),
                    thread_title: "Allowed".into(),
                    thread_kind: "direct".into(),
                    text: format!("allowed {i}"),
                    attachment_path: String::new(),
                    send_at_ts: 1_750_000_100_000 + i * 1_000,
                    recurrence: "once".into(),
                    misfire_policy: "send_on_resume".into(),
                    misfire_grace_secs: 600,
                })
                .unwrap();
        }
        let filtered = store
            .list_scheduled_messages_filtered(Some("pending"), Some(&["t-allowed".to_string()]), 2)
            .unwrap();
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|s| s.thread_id == "t-allowed"));
        // Empty allowed_thread_ids slice behaves as unrestricted (matches ApiClientDto semantics)
        let unrestricted = store
            .list_scheduled_messages_filtered(Some("pending"), Some(&[]), 2)
            .unwrap();
        assert_eq!(unrestricted.len(), 2);
        assert!(unrestricted.iter().all(|s| s.thread_id == "t-other"));

        // 4. Reopening with NULL anchor_day triggers safe backfill migration
        store
            .conn
            .execute(
                "UPDATE scheduled_messages SET anchor_day = NULL WHERE id = ?1",
                params![&monthly.id],
            )
            .unwrap();
        drop(store);
        let reopened = MessageStore::open(&db_path).unwrap();
        let reloaded = reopened
            .get_scheduled_message(&monthly.id)
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.anchor_day, Some(15));
        assert!(reopened.delete_scheduled_message(&monthly.id).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_idempotency_hash_load_around_has_newer_and_preview_fallback_kind() {
        let dir = std::env::temp_dir().join(format!(
            "teza-store-idem-around-kind-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("ux_meta.db");
        let mut store = MessageStore::open(&db_path).unwrap();

        // #13: ApiIdempotencyRecord stores & returns request_hash while legacy helpers keep working
        let t0 = 1_760_000_000_000_i64;
        store
            .put_api_idempotency_with_hash(
                "cli_1",
                "send:key-1",
                200,
                r#"{"ok":true,"msg_id":"m1"}"#,
                "a1b2c3d4",
                t0,
            )
            .unwrap();
        let rec = store
            .get_api_idempotency_record("cli_1", "send:key-1", DAY_MS, t0 + 5_000)
            .unwrap()
            .unwrap();
        assert_eq!(
            rec,
            ApiIdempotencyRecord {
                status_code: 200,
                response_json: r#"{"ok":true,"msg_id":"m1"}"#.to_string(),
                request_hash: "a1b2c3d4".to_string(),
            }
        );
        // Legacy wrapper returns (status, response_json)
        assert_eq!(
            store
                .get_api_idempotency("cli_1", "send:key-1", DAY_MS, t0 + 5_000)
                .unwrap(),
            Some((200, r#"{"ok":true,"msg_id":"m1"}"#.to_string()))
        );
        // Legacy put stores empty request_hash
        store
            .put_api_idempotency("cli_1", "send:key-legacy", 201, r#"{"ok":true}"#, t0)
            .unwrap();
        let legacy_rec = store
            .get_api_idempotency_record("cli_1", "send:key-legacy", DAY_MS, t0 + 1_000)
            .unwrap()
            .unwrap();
        assert_eq!(legacy_rec.request_hash, "");
        assert_eq!(legacy_rec.status_code, 201);

        // #21: load_messages_around_with_meta accurately computes has_newer
        for i in 1..=10_i64 {
            store
                .upsert_message(&MessageDto {
                    msg_id: format!("m-{i:02}"),
                    cli_msg_id: None,
                    thread_id: "t-hist".into(),
                    sender_id: "u1".into(),
                    sender_name: "Alice".into(),
                    text: format!("message {i}"),
                    msg_type: "chat.text".into(),
                    ts: i * 1_000,
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
        let (slice_mid, has_newer_mid) = store
            .load_messages_around_with_meta("t-hist", 3_000, 2, 3)
            .unwrap();
        let mid_ids: Vec<&str> = slice_mid.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(mid_ids, vec!["m-01", "m-02", "m-03", "m-04", "m-05"]);
        assert!(has_newer_mid);

        // Exact boundary (8, 9, 10 == 3 items -> no newer items remain!)
        let (slice_end, has_newer_end) = store
            .load_messages_around_with_meta("t-hist", 8_000, 2, 3)
            .unwrap();
        let end_ids: Vec<&str> = slice_end.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(end_ids, vec!["m-06", "m-07", "m-08", "m-09", "m-10"]);
        assert!(!has_newer_end);

        // max_after == 0 still reports has_newer == true when messages exist at/after target_ts
        let (slice_zero, has_newer_zero) = store
            .load_messages_around_with_meta("t-hist", 3_000, 2, 0)
            .unwrap();
        let zero_ids: Vec<&str> = slice_zero.iter().map(|m| m.msg_id.as_str()).collect();
        assert_eq!(zero_ids, vec!["m-01", "m-02"]);
        assert!(has_newer_zero);

        // #32: update_thread_preview_with_fallback_kind_and_meta creates Group/Cloud rows accurately
        let created_grp = store
            .update_thread_preview_with_fallback_kind_and_meta(
                "g888123",
                "Bạn: Báo cáo tuần",
                t0,
                false,
                true,
                Some(ThreadKind::Group),
                Some("Nhóm Kỹ Thuật"),
                None,
            )
            .unwrap();
        assert!(created_grp);
        let grp_row = store.get_thread_row("g888123").unwrap().unwrap();
        assert_eq!(grp_row.kind, ThreadKind::Group);
        assert_eq!(grp_row.title, "Nhóm Kỹ Thuật");

        let created_cloud = store
            .update_thread_preview_with_fallback_kind_and_meta(
                "cloud_me",
                "Bạn: Ghi chú nhanh",
                t0,
                false,
                true,
                Some(ThreadKind::Cloud),
                Some("Cloud của tôi"),
                None,
            )
            .unwrap();
        assert!(created_cloud);
        let cloud_row = store.get_thread_row("cloud_me").unwrap().unwrap();
        assert_eq!(cloud_row.kind, ThreadKind::Cloud);
        assert_eq!(cloud_row.title, "Cloud của tôi");

        // Backward-compatible wrapper still creates Direct rows by default
        let created_direct = store
            .update_thread_preview_returning_created("u-direct-new", "Xin chào", t0, true, false)
            .unwrap();
        assert!(created_direct);
        let direct_row = store.get_thread_row("u-direct-new").unwrap().unwrap();
        assert_eq!(direct_row.kind, ThreadKind::Direct);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
