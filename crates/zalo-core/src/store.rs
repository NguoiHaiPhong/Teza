//! Encrypted SQLite persistence (AES-256-GCM with DPAPI-sealed key) + in-memory FTS5 search.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::chat::parser::{canonicalize_reaction_emoji, clean_group_id, normalize_ts_ms};
use crate::chat::{
    strip_vi_accents, ChatFolderDto, FolderFilter, MessageDto, QuoteRef, ReactionSummary,
    ThreadDto, ThreadKind,
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
    /// True when more messages exist strictly after `max_message_ts()` beyond `limit_msgs`.
    #[serde(skip)]
    pub messages_truncated: bool,
    /// True when more cloud media links exist strictly after `max_cloud_media_cursor_ts`.
    #[serde(skip)]
    pub cloud_media_truncated: bool,
    /// Maximum `MAX(cloud_synced_ts, message_media.updated_ts)` across exported `cloud_media` rows.
    #[serde(skip)]
    pub max_cloud_media_cursor_ts: Option<i64>,
}

impl HistoryDeltaBatch {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn max_message_ts(&self) -> Option<i64> {
        self.messages.iter().map(|m| m.ts).max()
    }
}

pub struct MessageStore {
    conn: Connection,
    fts_conn: Connection,
    cipher: DbCipher,
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
                cloud_excluded INTEGER NOT NULL DEFAULT 0
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
            "#,
        )?;
        self.ensure_column("messages", "quote_json", "TEXT")?;
        self.ensure_column("messages", "reactions_json", "TEXT NOT NULL DEFAULT '[]'")?;
        self.ensure_column("messages", "cli_msg_id", "TEXT NOT NULL DEFAULT ''")?;
        self.ensure_column("messages", "reaction_actors_json", "TEXT NOT NULL DEFAULT ''")?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_thread_cli_msg_id ON messages(thread_id, cli_msg_id)",
            [],
        )?;
        self.ensure_column("threads", "last_read_in_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "peer_read_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "muted_until_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("media_objects", "cloud_excluded", "INTEGER NOT NULL DEFAULT 0")?;

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

    fn ensure_column(&self, table: &str, col: &str, def: &str) -> Result<()> {
        let sql = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&sql)?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(std::result::Result::ok)
            .any(|name| name == col);
        if !exists {
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

    pub fn upsert_message(&mut self, m: &MessageDto) -> Result<()> {
        let text_norm = strip_vi_accents(&m.text);
        let quote_json = m
            .quote
            .as_ref()
            .and_then(|q| serde_json::to_string(q).ok());
        let reactions_json =
            serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".to_string());
        let reactions_empty = m.reactions.is_empty() as i64;

        let aad = msg_aad(&m.thread_id, &m.msg_id);
        let enc_sender_id = self.cipher.encrypt_str(&aad, &m.sender_id)?;
        let enc_sender_name = self.cipher.encrypt_str(&aad, &m.sender_name)?;
        let enc_text = self.cipher.encrypt_str(&aad, &m.text)?;
        let enc_quote = self.cipher.encrypt_opt(&aad, quote_json.as_deref())?;
        let enc_reactions = self.cipher.encrypt_str(&aad, &reactions_json)?;
        let enc_att_url = self.cipher.encrypt_opt(&aad, m.attachment_url.as_deref())?;
        let enc_att_name = self.cipher.encrypt_opt(&aad, m.attachment_name.as_deref())?;
        let ts = normalize_ts_ms(m.ts);
        let cli_msg_id = m
            .cli_msg_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .unwrap_or("");

        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id)
            VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(thread_id, msg_id) DO UPDATE SET
                text = excluded.text,
                text_norm = '',
                msg_type = excluded.msg_type,
                is_recalled = excluded.is_recalled,
                is_pinned = MAX(messages.is_pinned, excluded.is_pinned),
                quote_json = COALESCE(excluded.quote_json, messages.quote_json),
                reactions_json = CASE WHEN ?16 = 1 THEN messages.reactions_json ELSE excluded.reactions_json END,
                cli_msg_id = CASE WHEN excluded.cli_msg_id != '' THEN excluded.cli_msg_id ELSE messages.cli_msg_id END,
                attachment_url = CASE
                    WHEN excluded.is_recalled = 1 OR LOWER(excluded.msg_type) IN ('chat.reminder', 'chat.poll') THEN NULL
                    WHEN LOWER(excluded.msg_type) IN ('chat.link', 'link', 'chat.location', 'chat.location.new', 'chat.recommended', 'chat.ecard') THEN excluded.attachment_url
                    ELSE COALESCE(excluded.attachment_url, messages.attachment_url)
                END,
                attachment_name = CASE
                    WHEN excluded.is_recalled = 1 THEN NULL
                    WHEN LOWER(excluded.msg_type) IN ('chat.link', 'link', 'chat.location', 'chat.location.new', 'chat.recommended', 'chat.ecard') AND excluded.attachment_url IS NULL THEN NULL
                    ELSE COALESCE(excluded.attachment_name, messages.attachment_name)
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
            ],
        )?;
        if !m.is_outgoing {
            tx.execute(
                "UPDATE threads SET peer_read_ts = MAX(peer_read_ts, ?2) WHERE id = ?1",
                params![m.thread_id, ts],
            )?;
        }
        if m.is_recalled {
            tx.execute(
                "DELETE FROM message_translations WHERE msg_id = ?1",
                params![m.msg_id],
            )?;
            tx.execute(
                "DELETE FROM message_media WHERE thread_id = ?1 AND msg_id = ?2",
                params![m.thread_id, m.msg_id],
            )?;
        }
        tx.commit()?;

        let fts_tx = self.fts_conn.transaction()?;
        fts_tx.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
            params![m.thread_id, m.msg_id],
        )?;
        fts_tx.execute(
            "INSERT INTO messages_fts (msg_id, thread_id, ts, sender_name, text_norm) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                m.msg_id,
                m.thread_id,
                ts,
                strip_vi_accents(&m.sender_name),
                text_norm
            ],
        )?;
        fts_tx.commit()?;
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
        self.upsert_message(&msg)?;
        Ok(Some(msg))
    }

    pub fn delete_message(&mut self, thread_id: &str, msg_id: &str) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let affected = tx.execute(
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

        let mut orphaned_paths: Vec<String> = Vec::new();
        let mut thumb_candidates: Vec<String> = Vec::new();

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
                if cloud_file_id.as_deref().is_some_and(|s| !s.is_empty()) {
                    tx.execute(
                        "UPDATE media_objects SET local_state = 0, local_rel_path = NULL, thumb_sha256 = NULL WHERE sha256 = ?1",
                        params![&sha],
                    )?;
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
                if cloud_file_id.as_deref().is_some_and(|s| !s.is_empty()) {
                    tx.execute(
                        "UPDATE media_objects SET local_state = 0, local_rel_path = NULL WHERE sha256 = ?1",
                        params![&tsha],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM media_objects WHERE sha256 = ?1",
                        params![&tsha],
                    )?;
                }
            }
        }

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

        tx.execute("DELETE FROM messages WHERE thread_id = ?1", params![tid])?;
        tx.execute("DELETE FROM threads WHERE id = ?1", params![tid])?;
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
        let affected = self.conn.execute(
            "UPDATE messages SET is_pinned = ?3 WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id, pinned as i64],
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
        self.conn.execute(
            "UPDATE messages
             SET reactions_json = ?3, reaction_actors_json = ?4
             WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id, enc_reactions, enc_actors],
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
        let ts = normalize_ts_ms(ts);
        let enc_preview = self.cipher.encrypt_str(&thread_aad(thread_id), preview)?;
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
                title: thread_id.to_string(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
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
        Ok(false)
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
    /// and up to `max_after` messages from `target_ts` onward) so jumping to a search hit renders it.
    pub fn load_messages_around(
        &self,
        thread_id: &str,
        target_ts: i64,
        context_before: usize,
        max_after: usize,
    ) -> Result<Vec<MessageDto>> {
        let mut msgs = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages WHERE thread_id = ?1 AND ts < ?2
             ORDER BY ts DESC LIMIT ?3",
            params![thread_id, target_ts, context_before as i64],
        )?;
        msgs.reverse();

        let after = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id
             FROM messages WHERE thread_id = ?1 AND ts >= ?2
             ORDER BY ts ASC LIMIT ?3",
            params![thread_id, target_ts, max_after as i64],
        )?;
        msgs.extend(after);
        Ok(msgs)
    }

    /// Accent-insensitive full-text search across decrypted in-memory index (`Ctrl+K`).
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let safe_limit = limit.clamp(1, 200) as i64;
        let clean = strip_vi_accents(query);
        let tokens: Vec<String> = clean
            .split(|c: char| c.is_whitespace() || c == '"' || c.is_control())
            .filter_map(|w| {
                let trimmed = w.trim_matches(|c: char| !c.is_alphanumeric());
                if trimmed.is_empty() || !trimmed.chars().any(char::is_alphanumeric) {
                    None
                } else {
                    Some(format!("\"{trimmed}\"*"))
                }
            })
            .take(32)
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let fts_query = tokens.join(" ");
        let mut stmt = self.fts_conn.prepare(
            "SELECT thread_id, msg_id
             FROM messages_fts
             WHERE messages_fts MATCH ?1
             ORDER BY CAST(ts AS INTEGER) DESC LIMIT ?2",
        )?;
        let pairs = stmt
            .query_map(params![fts_query, safe_limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut results = Vec::with_capacity(pairs.len());
        for (tid, mid) in pairs {
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
                cloud_synced_ts = COALESCE(excluded.cloud_synced_ts, media_objects.cloud_synced_ts)
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
        let now = chrono::Utc::now().timestamp_millis();
        let affected = self.conn.execute(
            "UPDATE message_media
             SET cdn_status = ?2, updated_ts = ?3
             WHERE url_hash = ?1",
            params![url_hash, status, now],
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
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO message_media (
                thread_id, msg_id, variant, url_hash, saved_user_path, updated_ts
             )
             VALUES (?1, ?2, ?3, '', ?4, ?5)
             ON CONFLICT(thread_id, msg_id, variant) DO UPDATE SET
                 saved_user_path = excluded.saved_user_path,
                 updated_ts = excluded.updated_ts",
            params![thread_id, msg_id, variant, enc_path, now],
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
    /// `media_objects` (plus their orphaned thumbnails), returning `local_rel_path`s to unlink outside DB lock.
    pub fn cleanup_orphaned_media_for_msg(
        &self,
        thread_id: &str,
        msg_id: &str,
    ) -> Result<Vec<String>> {
        let tx = self.conn.unchecked_transaction()?;
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

        let mut orphaned_paths = Vec::new();
        let mut thumb_candidates = Vec::new();

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
            struct OrphanRow {
                local_state: i32,
                local_rel_path: Option<String>,
                thumb_sha256: Option<String>,
                cloud_file_id: Option<String>,
            }
            let row: Option<OrphanRow> = tx
                .query_row(
                    "SELECT local_state, local_rel_path, thumb_sha256, cloud_file_id
                     FROM media_objects
                     WHERE sha256 = ?1
                     LIMIT 1",
                    params![&sha],
                    |r| {
                        Ok(OrphanRow {
                            local_state: r.get(0)?,
                            local_rel_path: r.get(1)?,
                            thumb_sha256: r.get(2)?,
                            cloud_file_id: r.get(3)?,
                        })
                    },
                )
                .optional()?;
            if let Some(OrphanRow {
                local_state,
                local_rel_path,
                thumb_sha256,
                cloud_file_id,
            }) = row
            {
                if local_state == MEDIA_LOCAL_STATE_LOCAL {
                    if let Some(rel) = local_rel_path {
                        orphaned_paths.push(rel);
                    }
                }
                if cloud_file_id.as_deref().is_some_and(|s| !s.is_empty()) {
                    tx.execute(
                        "UPDATE media_objects SET local_state = 0, local_rel_path = NULL, thumb_sha256 = NULL WHERE sha256 = ?1",
                        params![&sha],
                    )?;
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
                        orphaned_paths.push(rel);
                    }
                }
                if cloud_file_id.as_deref().is_some_and(|s| !s.is_empty()) {
                    tx.execute(
                        "UPDATE media_objects SET local_state = 0, local_rel_path = NULL WHERE sha256 = ?1",
                        params![&tsha],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM media_objects WHERE sha256 = ?1",
                        params![&tsha],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(orphaned_paths)
    }

    pub fn list_pending_cloud_uploads(&self, limit: usize) -> Result<Vec<MediaObjectRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT sha256, kind, mime, size_bytes, thumb_sha256, width, height, duration_ms,
                    local_state, local_rel_path, is_encrypted, pin_count, created_ts, last_accessed_ts,
                    cloud_provider, cloud_file_id, cloud_encrypted, cloud_key_ver, cloud_synced_ts
             FROM media_objects
             WHERE local_state = 1
               AND (cloud_file_id IS NULL OR cloud_file_id = '')
               AND cloud_excluded = 0
               AND LOWER(kind) != 'thumb'
             ORDER BY created_ts ASC, size_bytes ASC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], Self::decode_media_object_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
        self.conn.execute(
            "UPDATE media_objects
             SET cloud_provider = ?2,
                 cloud_file_id = ?3,
                 cloud_encrypted = 1,
                 cloud_key_ver = ?4,
                 cloud_synced_ts = ?5,
                 cloud_excluded = 0
             WHERE sha256 = ?1",
            params![sha256, cloud_provider, cloud_file_id, safe_ver, synced_ts],
        )?;
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
                     cloud_synced_ts = NULL
                 WHERE sha256 = ?1",
            )?;
            for sha in sha256s {
                affected += stmt.execute(params![sha])?;
            }
        }
        tx.commit()?;
        Ok(affected)
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
            for sha in sha256s {
                affected += stmt.execute(params![sha])?;
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

    pub fn export_history_delta_since(
        &self,
        uid: &str,
        device_id: &str,
        since_ts: i64,
        limit_msgs: usize,
    ) -> Result<HistoryDeltaBatch> {
        const HISTORY_CLOUD_MEDIA_BATCH_LIMIT: usize = 2000;

        let exported_ts = chrono::Utc::now().timestamp_millis();
        let (messages, messages_truncated) = if limit_msgs == 0 {
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
                params![since_ts, (HISTORY_CLOUD_MEDIA_BATCH_LIMIT + 1) as i64],
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
            messages_truncated,
            cloud_media_truncated,
            max_cloud_media_cursor_ts,
        })
    }

    pub fn import_history_delta_idempotent(&self, batch: &HistoryDeltaBatch) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;

        // 1. Upsert threads without regressing newer local timestamps, previews, or read watermarks.
        for t in &batch.threads {
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
            let last_ts = normalize_ts_ms(t.last_ts);
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

        // 2. Idempotently merge folders + thread_folders without discarding existing local thread_ids.
        if !batch.folders.is_empty() || !batch.thread_folders.is_empty() {
            let mut extra_links: HashMap<&str, Vec<&str>> = HashMap::new();
            for (fid, tid) in &batch.thread_folders {
                extra_links.entry(fid.as_str()).or_default().push(tid.as_str());
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

        // 3. Idempotently merge messages without un-recalling or duplicating.
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
            if !m.is_recalled {
                msg_text_map.insert(m.msg_id.as_str(), m.text.as_str());
            }
            let ts = normalize_ts_ms(m.ts);
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
                        "INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name, cli_msg_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                        tx.execute(
                            "DELETE FROM message_media WHERE thread_id = ?1 AND msg_id = ?2",
                            params![m.thread_id, m.msg_id],
                        )?;
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
                    let cur_reactions_empty = self
                        .cipher
                        .decrypt_or_legacy(&aad, &cur.reactions_json)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Vec<ReactionSummary>>(&s).ok())
                        .is_none_or(|v| v.is_empty());
                    let fill_reactions = cur_reactions_empty && !m.reactions.is_empty();

                    if pin_upgraded
                        || fill_quote
                        || fill_att_url
                        || fill_att_name
                        || fill_reactions
                        || fill_cli_msg_id
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
                        let new_reactions = if fill_reactions {
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
                        let new_att_name = if fill_att_name {
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

        // 5. Import cloud media metadata (local_state = 0 if absent; preserves local_state = 1 if already local)
        for cm in &batch.cloud_media {
            if cm.sha256.is_empty() || cm.cloud_file_id.is_empty() {
                continue;
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
                    cloud_provider = COALESCE(media_objects.cloud_provider, excluded.cloud_provider),
                    cloud_file_id = COALESCE(media_objects.cloud_file_id, excluded.cloud_file_id),
                    cloud_key_ver = MAX(media_objects.cloud_key_ver, excluded.cloud_key_ver),
                    cloud_synced_ts = COALESCE(media_objects.cloud_synced_ts, excluded.cloud_synced_ts)
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
        }

        tx.commit()?;

        if !fts_updates.is_empty() {
            let fts_tx = self.fts_conn.unchecked_transaction()?;
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

        Ok(applied_count)
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

        // Older than 25_000 -> img (10k) and vid (20k), skipping shared (30k)
        let older = store.list_cloud_items_older_than(25_000).unwrap();
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
}
