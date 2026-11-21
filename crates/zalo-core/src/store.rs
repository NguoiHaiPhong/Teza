//! Encrypted SQLite persistence (AES-256-GCM with DPAPI-sealed key) + in-memory FTS5 search.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::chat::parser::normalize_ts_ms;
use crate::chat::{
    strip_vi_accents, ChatFolderDto, FolderFilter, MessageDto, QuoteRef, ReactionSummary,
    ThreadDto, ThreadKind,
};
use crate::{data_dir, dpapi};

const ENC_PREFIX: &str = "enc:v1:";
const KEY_CHECK_VALUE: &str = "litechat-db-v1";

struct DbCipher {
    cipher: Aes256Gcm,
}

impl DbCipher {
    fn from_raw_key(key: &[u8; 32]) -> Self {
        Self {
            cipher: Aes256Gcm::new_from_slice(key).expect("32-byte AES-256 key"),
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

pub struct MessageStore {
    conn: Connection,
    fts_conn: Connection,
    cipher: DbCipher,
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
            "#,
        )?;
        self.ensure_column("messages", "quote_json", "TEXT")?;
        self.ensure_column("messages", "reactions_json", "TEXT NOT NULL DEFAULT '[]'")?;
        self.ensure_column("threads", "last_read_in_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "peer_read_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "muted_until_ts", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("threads", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;

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
        self.upsert_thread_inner(t, false)
    }

    pub fn upsert_thread_synced(&self, t: &ThreadDto) -> Result<()> {
        self.upsert_thread_inner(t, true)
    }

    fn upsert_thread_inner(&self, t: &ThreadDto, overwrite_pinned: bool) -> Result<()> {
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
        let clear_unread = (t.unread_count == 0 && t.last_preview.starts_with("Bạn:")) as i64;
        let last_ts = normalize_ts_ms(t.last_ts);
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
                preview_empty,
                avatar_empty,
                title_fallback,
                clear_unread,
                overwrite_pinned as i64,
                normalize_ts_ms(t.last_read_in_ts),
                normalize_ts_ms(t.peer_read_ts),
                normalize_ts_ms(t.muted_until_ts),
                t.is_archived as i64,
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

        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO messages (msg_id, thread_id, sender_id, sender_name, text, text_norm, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name)
            VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ON CONFLICT(thread_id, msg_id) DO UPDATE SET
                text = excluded.text,
                text_norm = '',
                msg_type = excluded.msg_type,
                is_recalled = excluded.is_recalled,
                is_pinned = MAX(messages.is_pinned, excluded.is_pinned),
                quote_json = COALESCE(excluded.quote_json, messages.quote_json),
                reactions_json = CASE WHEN ?15 = 1 THEN messages.reactions_json ELSE excluded.reactions_json END,
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
                reactions_empty,
            ],
        )?;
        if !m.is_outgoing {
            tx.execute(
                "UPDATE threads SET peer_read_ts = MAX(peer_read_ts, ?2) WHERE id = ?1",
                params![m.thread_id, ts],
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

        let quote: Option<QuoteRef> = quote_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let reactions: Vec<ReactionSummary> =
            serde_json::from_str(&reactions_json).unwrap_or_default();

        Ok(MessageDto {
            msg_id,
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
             FROM messages WHERE thread_id = ?1 AND msg_id = ?2 LIMIT 1",
            params![thread_id, msg_id],
        )?;
        Ok(msgs.pop())
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
        tx.commit()?;

        self.fts_conn.execute(
            "DELETE FROM messages_fts WHERE thread_id = ?1 AND msg_id = ?2",
            params![thread_id, msg_id],
        )?;
        Ok(affected > 0)
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

    pub fn apply_reaction(
        &mut self,
        thread_id: &str,
        msg_id: &str,
        emoji: Option<&str>,
        me: bool,
        removed: bool,
    ) -> Result<Option<MessageDto>> {
        let Some(mut msg) = self.get_message(thread_id, msg_id)? else {
            return Ok(None);
        };

        if removed {
            let changed = if me {
                if let Some(pos) = msg.reactions.iter().position(|r| {
                    r.me && emoji.is_none_or(|target| r.emoji == target)
                }) {
                    msg.reactions[pos].me = false;
                    msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                    if msg.reactions[pos].count == 0 {
                        msg.reactions.remove(pos);
                    }
                    true
                } else {
                    false
                }
            } else if let Some(target) = emoji {
                if let Some(pos) = msg.reactions.iter().position(|r| r.emoji == target) {
                    msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                    if msg.reactions[pos].count == 0 {
                        msg.reactions.remove(pos);
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if changed {
                self.upsert_message(&msg)?;
            }
            return Ok(Some(msg));
        }

        let Some(target) = emoji.filter(|e| !e.trim().is_empty()) else {
            return Ok(Some(msg));
        };

        if me {
            if msg.reactions.iter().any(|r| r.emoji == target && r.me) {
                return Ok(Some(msg));
            }
            if let Some(prev_pos) = msg.reactions.iter().position(|r| r.me) {
                msg.reactions[prev_pos].me = false;
                msg.reactions[prev_pos].count = msg.reactions[prev_pos].count.saturating_sub(1);
                if msg.reactions[prev_pos].count == 0 {
                    msg.reactions.remove(prev_pos);
                }
            }
        }

        if let Some(existing) = msg.reactions.iter_mut().find(|r| r.emoji == target) {
            existing.count += 1;
            if me {
                existing.me = true;
            }
        } else {
            msg.reactions.push(ReactionSummary {
                emoji: target.to_string(),
                count: 1,
                me,
            });
        }
        self.upsert_message(&msg)?;
        Ok(Some(msg))
    }

    pub fn toggle_reaction(
        &mut self,
        thread_id: &str,
        msg_id: &str,
        emoji: &str,
    ) -> Result<Option<(MessageDto, bool)>> {
        let Some(msg) = self.get_message(thread_id, msg_id)? else {
            return Ok(None);
        };
        let already_me = msg.reactions.iter().any(|r| r.emoji == emoji && r.me);
        let updated = self
            .apply_reaction(thread_id, msg_id, Some(emoji), true, already_me)?
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
                     COALESCE((SELECT MAX(ts) FROM messages WHERE thread_id = ?1 AND is_outgoing = 0), last_ts)
                 )
             WHERE id = ?1",
            params![thread_id],
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
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
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
                 FROM messages
                 WHERE thread_id = ?1 AND (ts < ?2 OR (ts = ?2 AND msg_id < ?3))
                 ORDER BY ts DESC, msg_id DESC
                 LIMIT ?4",
                params![thread_id, cutoff_ts, cutoff_id, limit as i64],
            )?,
            _ => self.query_messages(
                "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
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
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
             FROM messages WHERE thread_id = ?1 AND ts < ?2
             ORDER BY ts DESC LIMIT ?3",
            params![thread_id, target_ts, context_before as i64],
        )?;
        msgs.reverse();

        let after = self.query_messages(
            "SELECT msg_id, thread_id, sender_id, sender_name, text, msg_type, ts, is_outgoing, is_recalled, is_pinned, quote_json, reactions_json, attachment_url, attachment_name
             FROM messages WHERE thread_id = ?1 AND ts >= ?2
             ORDER BY ts ASC LIMIT ?3",
            params![thread_id, target_ts, max_after as i64],
        )?;
        msgs.extend(after);
        Ok(msgs)
    }

    /// Accent-insensitive full-text search across decrypted in-memory index (`Ctrl+K`).
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<MessageDto>> {
        let clean = strip_vi_accents(query);
        let tokens: Vec<String> = clean
            .split_whitespace()
            .filter(|w| !w.is_empty())
            .map(|w| format!("\"{}\"*", w.replace('"', "")))
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
            .query_map(params![fts_query, limit as i64], |r| {
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

        let (media, files) = store.list_thread_media("t1", 20).unwrap();
        assert!(media.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].msg_id, "legacy-http-file");
        assert_eq!(
            files[0].attachment_url.as_deref(),
            Some("https://files.zalo.cloud/report.pdf")
        );

        // Resyncing the chat.link with attachment_url = None must clear the stale external URL
        store
            .upsert_message(&MessageDto {
                msg_id: "stale-link".into(),
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
}
