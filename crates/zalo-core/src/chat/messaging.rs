//! Sending text, quote replies, reactions, recalls, local deletes, and image/file attachments.

use std::io::SeekFrom;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use md5::{Digest, Md5};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};

use crate::chat::parser::{clean_group_id, normalize_media_url, pick_str, sticker_image_url};
use crate::chat::{MessageDto, QuoteRef, ThreadKind};
use crate::session::{Session, SessionError};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StickerItem {
    pub id: u64,
    pub cate_id: u64,
    pub sticker_type: u64,
    pub url: String,
    #[serde(default)]
    pub category: String,
}

pub fn default_stickers() -> Vec<StickerItem> {
    (1_u64..=30)
        .filter_map(|id| {
            sticker_image_url(id, 0).map(|url| StickerItem {
                id,
                cate_id: 0,
                sticker_type: 1,
                url,
                category: "Mặc định".into(),
            })
        })
        .collect()
}

pub fn parse_sticker_response_items(dec: &Value, category: &str) -> Vec<StickerItem> {
    let data_owned: Option<Value> = match dec.get("data") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s.trim()).ok(),
        Some(v) => Some(v.clone()),
        None => Some(dec.clone()),
    };
    let Some(data_node) = data_owned else {
        return Vec::new();
    };

    let mut raw_items: Vec<Value> = Vec::new();
    if let Some(arr) = data_node.as_array() {
        raw_items.extend(arr.iter().cloned());
    } else {
        for key in ["suggSticker", "stickers", "items", "list"] {
            if let Some(arr) = data_node.get(key).and_then(Value::as_array) {
                raw_items.extend(arr.iter().cloned());
            }
        }
        if let Some(cates) = data_node.get("cateList").and_then(Value::as_array) {
            for cat in cates {
                for key in ["stickers", "items", "list"] {
                    if let Some(arr) = cat.get(key).and_then(Value::as_array) {
                        raw_items.extend(arr.iter().cloned());
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for it in raw_items {
        let id = it
            .get("stickerId")
            .or_else(|| it.get("id"))
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0);
        if id == 0 || !seen.insert(id) {
            continue;
        }
        let cate_id = it
            .get("cateId")
            .or_else(|| it.get("catId"))
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0);
        let sticker_type = it
            .get("type")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(1);
        let direct_url = pick_str(&it, &["stickerWebpUrl", "stickerUrl", "url"])
            .map(|u| normalize_media_url(&u))
            .filter(|u| u.starts_with("https://"));
        let Some(url) = direct_url.or_else(|| sticker_image_url(id, cate_id)) else {
            continue;
        };
        out.push(StickerItem {
            id,
            cate_id,
            sticker_type,
            url,
            category: category.to_string(),
        });
    }
    out
}

async fn fetch_sticker_suggest(session: &Session, keyword: &str, category: &str) -> Vec<StickerItem> {
    let base = session.domains.sticker.trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }
    let body = obj(&[("keyword", json!(keyword)), ("imei", json!(session.imei))]);
    let Ok(params) = session.encoded_params(&body) else {
        return Vec::new();
    };
    let url = format!(
        "{base}/api/message/sticker/suggest?{}&params={params}",
        session.common_params()
    );
    let Ok(Ok(raw)) =
        tokio::time::timeout(std::time::Duration::from_secs(4), session.get_json(&url)).await
    else {
        return Vec::new();
    };
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    parse_sticker_response_items(&dec, category)
}

pub async fn list_stickers(session: &Session, recent: Vec<StickerItem>) -> Result<Vec<StickerItem>> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for mut s in recent {
        if s.id > 0 && seen.insert(s.id) {
            if s.category.is_empty() {
                s.category = "Gần đây".into();
            }
            out.push(s);
        }
    }

    if session.is_authenticated() {
        let keywords = [
            "", "haha", "hi", "love", "ok", "cry", "thanks", "angry", "happy", "cute", "bye",
            "good",
        ];
        let mut set = tokio::task::JoinSet::new();
        for kw in keywords {
            let sess_c = session.clone_shallow();
            let kw_s = kw.to_string();
            set.spawn(async move { fetch_sticker_suggest(&sess_c, &kw_s, "Gợi ý").await });
        }
        while let Some(res) = set.join_next().await {
            if let Ok(items) = res {
                for s in items {
                    if seen.insert(s.id) {
                        out.push(s);
                    }
                }
            }
        }
    }

    for s in default_stickers() {
        if seen.insert(s.id) {
            out.push(s);
        }
    }
    Ok(out)
}

pub async fn search_stickers(session: &Session, keyword: &str) -> Result<Vec<StickerItem>> {
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return list_stickers(session, Vec::new()).await;
    }
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    Ok(fetch_sticker_suggest(session, trimmed, "Tìm kiếm").await)
}

pub async fn send_sticker(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    sticker_id: u64,
    cate_id: u64,
    sticker_type: u64,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let client_id = next_client_id();
    let is_group = kind == ThreadKind::Group;
    let (url, body) = if is_group {
        let grid = clean_group_id(thread_id);
        let body = obj(&[
            ("grid", json!(grid)),
            ("stickerId", json!(sticker_id)),
            ("cateId", json!(cate_id)),
            ("type", json!(sticker_type)),
            ("clientId", json!(client_id)),
            ("imei", json!(session.imei)),
            ("zsource", json!(101)),
        ]);
        let url = format!(
            "{}/api/group/sticker?{}",
            session.domains.group.trim_end_matches('/'),
            session.common_params()
        );
        (url, body)
    } else {
        let host = resolve_chat_host(session);
        let body = obj(&[
            ("toid", json!(thread_id)),
            ("stickerId", json!(sticker_id)),
            ("cateId", json!(cate_id)),
            ("type", json!(sticker_type)),
            ("clientId", json!(client_id)),
            ("imei", json!(session.imei)),
            ("zsource", json!(101)),
        ]);
        let url = format!("{host}/api/message/sticker?{}", session.common_params());
        (url, body)
    };

    let params = session.encoded_params(&body)?;
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&dec)?;

    let msg_id = dec
        .get("data")
        .and_then(|d| pick_str(d, &["msgId", "globalMsgId"]))
        .unwrap_or_else(|| client_id.to_string());

    Ok(MessageDto {
        msg_id,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".into(),
        text: "[Nhãn dán]".into(),
        msg_type: "chat.sticker".into(),
        ts: client_id as i64,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: sticker_image_url(sticker_id, cate_id),
        attachment_name: Some(format!("sticker_{cate_id}_{sticker_id}.png")),
    })
}

/// Maximum file / image attachment size accepted by Zalo Web (100 MiB).
pub const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
const UPLOAD_CHUNK_SIZE: usize = 1_048_576;

pub fn is_image_extension(file_name: &str) -> bool {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp")
}

pub fn mime_from_extension(file_name: &str) -> &'static str {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

pub fn validate_attachment_size(total_size: u64) -> Result<()> {
    if total_size == 0 {
        anyhow::bail!("Tệp đính kèm rỗng (0 byte)");
    }
    if total_size > MAX_ATTACHMENT_BYTES {
        anyhow::bail!(
            "Tệp quá lớn ({:.1} MB, tối đa {} MB)",
            total_size as f64 / (1024.0 * 1024.0),
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        );
    }
    Ok(())
}

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

pub fn next_client_id() -> u64 {
    static LAST_ID: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
    let prev = LAST_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            Some(now.max(last.saturating_add(1)))
        })
        .unwrap_or_else(|x| x);
    now.max(prev.saturating_add(1))
}

fn resolve_chat_host(session: &Session) -> &str {
    if session.domains.chat.contains("-msg-chat.")
        || session.domains.chat.contains("tt-chat2-wpa")
        || session.domains.chat.is_empty()
    {
        "https://tt-chat3-wpa.api.zaloapp.com"
    } else {
        session.domains.chat.trim_end_matches('/')
    }
}

fn check_error_code(resp: &Value) -> Result<()> {
    if let Some(code) = resp.get("error_code").and_then(Value::as_i64) {
        if crate::session::is_session_expired_code(code) {
            return Err(SessionError::Expired { code }.into());
        }
        if code != 0 {
            let msg = resp
                .get("error_message")
                .and_then(Value::as_str)
                .unwrap_or("Lỗi máy chủ Zalo")
                .to_string();
            return Err(SessionError::Protocol { code, message: msg }.into());
        }
    }
    Ok(())
}

pub async fn send_text(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    text: &str,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let client_id = next_client_id();
    let is_group = kind == ThreadKind::Group;

    let (url, body) = if is_group {
        let grid = clean_group_id(thread_id);
        let body = obj(&[
            ("grid", json!(grid)),
            ("message", json!(text)),
            ("clientId", json!(client_id)),
            ("visibility", json!(0)),
            ("ttl", json!(0)),
            ("imei", json!(session.imei)),
        ]);
        let url = format!(
            "{}/api/group/sendmsg?{}",
            session.domains.group.trim_end_matches('/'),
            session.common_params()
        );
        (url, body)
    } else {
        let host = resolve_chat_host(session);
        let body = obj(&[
            ("toid", json!(thread_id)),
            ("message", json!(text)),
            ("clientId", json!(client_id)),
            ("imei", json!(session.imei)),
            ("ttl", json!(0)),
        ]);
        let url = format!("{host}/api/message/sms?{}", session.common_params());
        (url, body)
    };

    let params = session.encoded_params(&body)?;
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&dec)?;

    let msg_id = dec
        .get("data")
        .and_then(|d| pick_str(d, &["msgId", "globalMsgId"]))
        .unwrap_or_else(|| client_id.to_string());

    Ok(MessageDto {
        msg_id,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".into(),
        text: text.to_string(),
        msg_type: "chat.text".into(),
        ts: client_id as i64,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: None,
        attachment_name: None,
    })
}

pub async fn send_quote(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    text: &str,
    quoted: &MessageDto,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let client_id = next_client_id();
    let is_group = kind == ThreadKind::Group;

    let qmsg_owner = if quoted.is_outgoing {
        session.profile.uid.as_str()
    } else if !quoted.sender_id.is_empty() && quoted.sender_id.chars().all(|c| c.is_ascii_digit()) {
        quoted.sender_id.as_str()
    } else if !is_group {
        thread_id
    } else {
        "0"
    };
    let qmsg_id_num = quoted
        .msg_id
        .parse::<u64>()
        .unwrap_or(client_id.saturating_sub(1000));
    let qmsg_ts = if quoted.ts > 0 {
        quoted.ts as u64
    } else {
        client_id.saturating_sub(1000)
    };

    let (url, body) = if is_group {
        let grid = clean_group_id(thread_id);
        let body = obj(&[
            ("grid", json!(grid)),
            ("message", json!(text)),
            ("clientId", json!(client_id)),
            ("language", json!("vi")),
            ("qmsgOwner", json!(qmsg_owner)),
            ("qmsgId", json!(qmsg_id_num)),
            ("qmsgCliId", json!(qmsg_ts)),
            ("qmsgType", json!(1)),
            ("qmsg", json!(quoted.text)),
            ("qmsgTs", json!(qmsg_ts)),
            ("visibility", json!(0)),
            ("ttl", json!(0)),
            ("imei", json!(session.imei)),
            ("zsource", json!(101)),
        ]);
        let url = format!(
            "{}/api/group/quote?{}",
            session.domains.group.trim_end_matches('/'),
            session.common_params()
        );
        (url, body)
    } else {
        let host = resolve_chat_host(session);
        let body = obj(&[
            ("toid", json!(thread_id)),
            ("message", json!(text)),
            ("clientId", json!(client_id)),
            ("qmsgOwner", json!(qmsg_owner)),
            ("qmsgId", json!(qmsg_id_num)),
            ("qmsgCliId", json!(qmsg_ts)),
            ("qmsgType", json!(1)),
            ("qmsgTs", json!(qmsg_ts)),
            ("qmsg", json!(quoted.text)),
            ("imei", json!(session.imei)),
            ("ttl", json!(0)),
            ("zsource", json!(101)),
        ]);
        let url = format!("{host}/api/message/quote?{}", session.common_params());
        (url, body)
    };

    let params = session.encoded_params(&body)?;
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&dec)?;

    let msg_id = dec
        .get("data")
        .and_then(|d| pick_str(d, &["msgId", "globalMsgId"]))
        .unwrap_or_else(|| client_id.to_string());

    Ok(MessageDto {
        msg_id,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".into(),
        text: text.to_string(),
        msg_type: "chat.text".into(),
        ts: client_id as i64,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: Some(QuoteRef {
            msg_id: quoted.msg_id.clone(),
            sender_name: quoted.sender_name.clone(),
            snippet: quoted.text.clone(),
        }),
        reactions: Vec::new(),
        attachment_url: None,
        attachment_name: None,
    })
}

pub fn reaction_code_and_type(emoji: &str) -> (&'static str, i32) {
    match emoji.trim() {
        "👍" | "/-strong" => ("/-strong", 3),
        "❤️" | "❤" | "/-heart" => ("/-heart", 5),
        "😆" | ":>" => (":>", 0),
        "😮" | ":o" => (":o", 32),
        "😢" | ":-(( " | ":-((" => (":-((", 2),
        "😠" | ":-h" => (":-h", 20),
        _ => ("/-heart", 5),
    }
}

pub async fn send_reaction(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg: &MessageDto,
    emoji: &str,
    remove: bool,
) -> Result<()> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let now = next_client_id();
    let (r_icon, r_type) = if remove {
        ("/-none", -1)
    } else {
        reaction_code_and_type(emoji)
    };
    let is_local = msg.msg_id.starts_with("local-") || msg.msg_id.starts_with("out_");
    let g_msg_id = if is_local { "" } else { msg.msg_id.as_str() };
    let c_msg_id = if msg.ts > 0 { msg.ts as u64 } else { now };

    let inner = json!({
        "rMsg": [{
            "gMsgID": g_msg_id,
            "cMsgID": c_msg_id,
            "msgType": 1,
        }],
        "rIcon": r_icon,
        "rType": r_type,
        "source": 6,
    })
    .to_string();
    let react_list = json!([{ "message": inner, "clientId": now }]);

    let react_host = if session.domains.reaction.is_empty() {
        crate::endpoints::REACTION_FALLBACK
    } else {
        session.domains.reaction.trim_end_matches('/')
    };

    let (url, body) = if kind == ThreadKind::Group {
        (
            format!("{react_host}/api/group/reaction?{}", session.common_params()),
            obj(&[
                ("react_list", react_list),
                ("grid", json!(clean_group_id(thread_id))),
                ("imei", json!(session.imei)),
            ]),
        )
    } else {
        (
            format!("{react_host}/api/message/reaction?{}", session.common_params()),
            obj(&[("react_list", react_list), ("toid", json!(thread_id))]),
        )
    };
    let params = session.encoded_params(&body)?;
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&dec)
}

pub async fn recall_message(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg: &MessageDto,
) -> Result<()> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let now = next_client_id();
    let cli_id = if msg.ts > 0 {
        msg.ts.to_string()
    } else {
        now.to_string()
    };

    let (url, body) = if kind == ThreadKind::Group {
        let body = obj(&[
            ("msgId", json!(msg.msg_id)),
            ("grid", json!(clean_group_id(thread_id))),
            ("clientId", json!(now)),
            ("visibility", json!(0)),
            ("cliMsgIdUndo", json!(cli_id)),
            ("imei", json!(session.imei)),
        ]);
        let url = format!(
            "{}/api/group/undomsg?{}",
            session.domains.group.trim_end_matches('/'),
            session.common_params()
        );
        (url, body)
    } else {
        let host = resolve_chat_host(session);
        let body = obj(&[
            ("msgId", json!(msg.msg_id)),
            ("toid", json!(thread_id)),
            ("clientId", json!(now)),
            ("cliMsgIdUndo", json!(cli_id)),
        ]);
        let url = format!("{host}/api/message/undo?{}", session.common_params());
        (url, body)
    };
    let params = session.encoded_params(&body)?;
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&dec)
}

async fn read_chunk_exact<R: AsyncRead + Unpin>(reader: &mut R, max_len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; max_len];
    let mut filled = 0;
    while filled < max_len {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

struct UploadTarget<'a> {
    file_host: &'a str,
    target: &'a str,
    target_type: i32,
    is_group: bool,
    client_id: u64,
    file_name: &'a str,
    mime: &'a str,
    total_size: u64,
    caption: &'a str,
}

impl<'a> UploadTarget<'a> {
    fn new(
        session: &'a Session,
        thread_id: &'a str,
        is_group: bool,
        file_name: &'a str,
        mime: &'a str,
        total_size: u64,
        caption: &'a str,
    ) -> Self {
        Self {
            file_host: if session.domains.file.is_empty() {
                "https://tt-files-wpa.chat.zalo.me"
            } else {
                session.domains.file.trim_end_matches('/')
            },
            target: if is_group {
                clean_group_id(thread_id)
            } else {
                thread_id
            },
            target_type: if is_group { 11 } else { 2 },
            is_group,
            client_id: next_client_id(),
            file_name,
            mime,
            total_size,
            caption,
        }
    }
}

async fn stream_chunks_upload<R: AsyncRead + Unpin>(
    session: &Session,
    reader: &mut R,
    ctx: &UploadTarget<'_>,
    upload_ep: &str,
    extra_photo_fields: bool,
    mut hasher: Option<&mut Md5>,
) -> Result<Value> {
    let total_chunks = (ctx.total_size as usize).div_ceil(UPLOAD_CHUNK_SIZE).max(1);
    let mut last_up = Value::Null;

    for idx in 0..total_chunks {
        let chunk = read_chunk_exact(reader, UPLOAD_CHUNK_SIZE).await?;
        if chunk.is_empty() {
            break;
        }
        if let Some(ref mut h) = hasher {
            h.update(&chunk);
        }

        let mut m = Map::new();
        m.insert("totalChunk".into(), json!(total_chunks));
        m.insert("fileName".into(), json!(ctx.file_name));
        m.insert("clientId".into(), json!(ctx.client_id));
        m.insert("totalSize".into(), json!(ctx.total_size));
        m.insert("imei".into(), json!(session.imei));
        m.insert("isE2EE".into(), json!(0));
        if extra_photo_fields {
            m.insert("jxl".into(), json!(0));
        }
        m.insert("chunkId".into(), json!(idx + 1));
        if ctx.is_group {
            m.insert("grid".into(), json!(ctx.target));
        } else {
            m.insert("toid".into(), json!(ctx.target));
        }
        let params = session.encoded_params(&Value::Object(m))?;
        let url = format!(
            "{}{upload_ep}?{}&params={params}&type={}",
            ctx.file_host,
            session.common_params(),
            ctx.target_type,
        );
        let raw = session
            .post_multipart(&url, "chunkContent", ctx.file_name, ctx.mime, &chunk)
            .await?;
        last_up = session.maybe_decrypt(raw, session.session_key_bytes());
        check_error_code(&last_up)?;
    }

    Ok(last_up)
}

async fn send_photo_reader<R: AsyncRead + AsyncSeek + Unpin>(
    session: &Session,
    thread_id: &str,
    ctx: &UploadTarget<'_>,
    reader: &mut R,
) -> Result<MessageDto> {
    let header_len = ctx.total_size.min(65_536) as usize;
    let header = read_chunk_exact(reader, header_len).await?;
    let (width, height) = detect_image_dimensions(&header).unwrap_or((800, 600));
    reader.seek(SeekFrom::Start(0)).await?;

    let upload_ep = if ctx.is_group {
        "/api/group/photo_original/upload"
    } else {
        "/api/message/photo_original/upload"
    };

    let last_up = stream_chunks_upload(session, reader, ctx, upload_ep, true, None).await?;

    let data_node = last_up.get("data").unwrap_or(&last_up);
    let photo_id = pick_str(data_node, &["photoId", "clientFileId"]);
    let normal_url = pick_str(data_node, &["normalUrl", "hdUrl", "oriUrl", "url"]).unwrap_or_default();
    let hd_url = pick_str(data_node, &["hdUrl", "normalUrl", "oriUrl"]).unwrap_or_else(|| normal_url.clone());
    let ori_url = pick_str(data_node, &["oriUrl", "hdUrl", "normalUrl"]).unwrap_or_else(|| normal_url.clone());
    let thumb_url = pick_str(data_node, &["thumbUrl", "normalUrl", "hdUrl"]).unwrap_or_else(|| normal_url.clone());

    let send_dec = if let Some(pid) = photo_id.filter(|_| !normal_url.is_empty()) {
        let send_ep = if ctx.is_group {
            "/api/group/photo_original/send"
        } else {
            "/api/message/photo_original/send"
        };
        let pid_val = pid.parse::<u64>().map(|n| json!(n)).unwrap_or_else(|_| json!(pid));
        let mut send_obj = Map::new();
        send_obj.insert("photoId".into(), pid_val);
        send_obj.insert("clientId".into(), json!(ctx.client_id));
        send_obj.insert("desc".into(), json!(ctx.caption));
        send_obj.insert("width".into(), json!(width));
        send_obj.insert("height".into(), json!(height));
        send_obj.insert("rawUrl".into(), json!(normal_url));
        send_obj.insert("hdUrl".into(), json!(hd_url));
        send_obj.insert("thumbUrl".into(), json!(thumb_url));
        send_obj.insert("oriUrl".into(), json!(ori_url));
        send_obj.insert("normalUrl".into(), json!(normal_url));
        send_obj.insert("hdSize".into(), json!(ctx.total_size));
        send_obj.insert("isHD".into(), json!(1));
        send_obj.insert("hd".into(), json!(1));
        send_obj.insert("isOriginal".into(), json!(1));
        send_obj.insert("zsource".into(), json!(-1));
        send_obj.insert("jcp".into(), json!("{\"convertible\":true}"));
        send_obj.insert("imei".into(), json!(session.imei));
        if ctx.is_group {
            send_obj.insert("grid".into(), json!(ctx.target));
            send_obj.insert("visibility".into(), json!(0));
        } else {
            send_obj.insert("toid".into(), json!(ctx.target));
        }
        let params = session.encoded_params(&Value::Object(send_obj))?;
        let url = format!("{}{send_ep}?{}", ctx.file_host, session.common_params());
        let raw = session.post_form(&url, format!("params={params}")).await?;
        session.maybe_decrypt(raw, session.session_key_bytes())
    } else if ctx.total_size <= UPLOAD_CHUNK_SIZE as u64 {
        reader.seek(SeekFrom::Start(0)).await?;
        let small_bytes = read_chunk_exact(reader, ctx.total_size as usize).await?;
        let single_ep = if ctx.is_group {
            "/api/group/sendpt"
        } else {
            "/api/message/photo"
        };
        let mut body_obj = Map::new();
        body_obj.insert("clientId".into(), json!(ctx.client_id));
        body_obj.insert("desc".into(), json!(ctx.caption));
        body_obj.insert("width".into(), json!(width));
        body_obj.insert("height".into(), json!(height));
        body_obj.insert("imei".into(), json!(session.imei));
        if ctx.is_group {
            body_obj.insert("grid".into(), json!(ctx.target));
            body_obj.insert("visibility".into(), json!(0));
        } else {
            body_obj.insert("toid".into(), json!(ctx.target));
        }
        let params = session.encoded_params(&Value::Object(body_obj))?;
        let url = format!(
            "{}{single_ep}?{}&params={params}&type={}",
            ctx.file_host,
            session.common_params(),
            ctx.target_type,
        );
        let raw = session
            .post_multipart(&url, "fileContent", ctx.file_name, ctx.mime, &small_bytes)
            .await?;
        session.maybe_decrypt(raw, session.session_key_bytes())
    } else {
        anyhow::bail!("Upload ảnh phân mảnh không trả về photoId hợp lệ");
    };

    check_error_code(&send_dec)?;

    let msg_id = send_dec
        .get("data")
        .and_then(|d| pick_str(d, &["msgId", "globalMsgId"]))
        .unwrap_or_else(|| ctx.client_id.to_string());
    let raw_final_url = if !ori_url.is_empty() {
        ori_url
    } else if !hd_url.is_empty() {
        hd_url
    } else {
        normal_url
    };
    let final_url = normalize_media_url(&raw_final_url);

    Ok(MessageDto {
        msg_id,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".into(),
        text: if ctx.caption.trim().is_empty() {
            "[Hình ảnh]".into()
        } else {
            ctx.caption.trim().to_string()
        },
        msg_type: "chat.photo".into(),
        ts: ctx.client_id as i64,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: if final_url.is_empty() { None } else { Some(final_url) },
        attachment_name: Some(ctx.file_name.to_string()),
    })
}

async fn send_file_reader<R: AsyncRead + Unpin>(
    session: &Session,
    thread_id: &str,
    ctx: &UploadTarget<'_>,
    reader: &mut R,
) -> Result<MessageDto> {
    let upload_ep = if ctx.is_group {
        "/api/group/asyncfile/upload"
    } else {
        "/api/message/asyncfile/upload"
    };

    let mut hasher = Md5::new();
    let last_up = stream_chunks_upload(
        session,
        reader,
        ctx,
        upload_ep,
        false,
        Some(&mut hasher),
    )
    .await?;

    let checksum = format!("{:x}", hasher.finalize());
    let data_node = last_up.get("data").unwrap_or(&last_up);
    let file_id = pick_str(data_node, &["fileId", "clientFileId"])
        .ok_or_else(|| anyhow::anyhow!("Máy chủ không trả về fileId sau khi tải tệp lên"))?;
    let file_url = pick_str(data_node, &["fileUrl", "url", "href"]).unwrap_or_default();
    let extension = Path::new(ctx.file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let send_ep = if ctx.is_group {
        "/api/group/asyncfile/msg"
    } else {
        "/api/message/asyncfile/msg"
    };
    let fid_val = file_id.parse::<u64>().map(|n| json!(n)).unwrap_or_else(|_| json!(file_id));
    let mut send_obj = Map::new();
    send_obj.insert("fileId".into(), fid_val);
    send_obj.insert("fileName".into(), json!(ctx.file_name));
    send_obj.insert("fileSize".into(), json!(ctx.total_size));
    send_obj.insert("checksum".into(), json!(checksum));
    send_obj.insert("extension".into(), json!(extension));
    send_obj.insert("clientId".into(), json!(ctx.client_id));
    send_obj.insert("ttl".into(), json!(0));
    send_obj.insert("zsource".into(), json!(400));
    send_obj.insert("imei".into(), json!(session.imei));
    if !file_url.is_empty() {
        send_obj.insert("fileUrl".into(), json!(file_url));
    }
    if !ctx.caption.trim().is_empty() {
        send_obj.insert("desc".into(), json!(ctx.caption.trim()));
    }
    if ctx.is_group {
        send_obj.insert("grid".into(), json!(ctx.target));
        send_obj.insert("visibility".into(), json!(0));
    } else {
        send_obj.insert("toid".into(), json!(ctx.target));
    }

    let params = session.encoded_params(&Value::Object(send_obj))?;
    let url = format!("{}{send_ep}?{}", ctx.file_host, session.common_params());
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let send_dec = session.maybe_decrypt(raw, session.session_key_bytes());
    check_error_code(&send_dec)?;

    let msg_id = send_dec
        .get("data")
        .and_then(|d| pick_str(d, &["msgId", "globalMsgId"]))
        .unwrap_or_else(|| ctx.client_id.to_string());
    let raw_final_url = send_dec
        .get("data")
        .and_then(|d| pick_str(d, &["fileUrl", "url", "href"]))
        .unwrap_or(file_url);
    let final_url = normalize_media_url(&raw_final_url);

    let text = if ctx.caption.trim().is_empty() {
        format!("[Tập tin] {}", ctx.file_name)
    } else {
        format!("[Tập tin] {}\n{}", ctx.file_name, ctx.caption.trim())
    };

    Ok(MessageDto {
        msg_id,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".into(),
        text,
        msg_type: "chat.file".into(),
        ts: ctx.client_id as i64,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: if final_url.is_empty() { None } else { Some(final_url) },
        attachment_name: Some(ctx.file_name.to_string()),
    })
}

async fn send_attachment_reader<R: AsyncRead + AsyncSeek + Unpin>(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    file_name: &str,
    total_size: u64,
    caption: &str,
    reader: &mut R,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    validate_attachment_size(total_size)?;
    let mime = mime_from_extension(file_name);
    let is_group = kind == ThreadKind::Group;
    let ctx = UploadTarget::new(
        session, thread_id, is_group, file_name, mime, total_size, caption,
    );
    if is_image_extension(file_name) {
        send_photo_reader(session, thread_id, &ctx, reader).await
    } else {
        send_file_reader(session, thread_id, &ctx, reader).await
    }
}

pub async fn send_attachment_file(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    path: &Path,
    caption: &str,
) -> Result<MessageDto> {
    let meta = tokio::fs::metadata(path).await?;
    if !meta.is_file() {
        anyhow::bail!("Đường dẫn không phải tệp hợp lệ");
    }
    let total_size = meta.len();
    validate_attachment_size(total_size)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("attachment.bin");
    let mut file = tokio::fs::File::open(path).await?;
    send_attachment_reader(
        session,
        thread_id,
        kind,
        file_name,
        total_size,
        caption,
        &mut file,
    )
    .await
}

pub async fn send_attachment_bytes(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    file_name: &str,
    bytes: &[u8],
    caption: &str,
) -> Result<MessageDto> {
    let total_size = bytes.len() as u64;
    validate_attachment_size(total_size)?;
    let mut cursor = std::io::Cursor::new(bytes);
    send_attachment_reader(
        session,
        thread_id,
        kind,
        file_name,
        total_size,
        caption,
        &mut cursor,
    )
    .await
}

fn detect_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // PNG
    if bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    // GIF
    if bytes.len() >= 10 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some((w, h));
    }
    // JPEG SOF0/SOF2
    if bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xC0 || marker == 0xC2 {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            if marker == 0xD9 || marker == 0xDA {
                break;
            }
            let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if seg_len < 2 {
                break;
            }
            i += 2 + seg_len;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_attachment_sizes_and_extensions() {
        assert!(validate_attachment_size(0).is_err());
        assert!(validate_attachment_size(1).is_ok());
        assert!(validate_attachment_size(MAX_ATTACHMENT_BYTES).is_ok());
        assert!(validate_attachment_size(MAX_ATTACHMENT_BYTES + 1).is_err());

        assert!(is_image_extension("photo.PNG"));
        assert!(is_image_extension("a.jpeg"));
        assert!(!is_image_extension("report.pdf"));
        assert_eq!(mime_from_extension("report.pdf"), "application/pdf");
        assert_eq!(
            mime_from_extension("archive.zip"),
            "application/zip"
        );
        assert_eq!(
            mime_from_extension("unknown.xyz"),
            "application/octet-stream"
        );
    }

    #[test]
    fn reads_chunks_across_partial_reads_without_dropping_bytes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut cursor = std::io::Cursor::new(vec![42u8; 10]);
            let first = read_chunk_exact(&mut cursor, 4).await.unwrap();
            let second = read_chunk_exact(&mut cursor, 4).await.unwrap();
            let third = read_chunk_exact(&mut cursor, 4).await.unwrap();
            assert_eq!(first.len(), 4);
            assert_eq!(second.len(), 4);
            assert_eq!(third.len(), 2);
        });
    }

    #[test]
    fn parse_sticker_response_items_extracts_nested_and_direct_urls() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "suggSticker": [
                    { "stickerId": 2101, "cateId": 45, "type": 7, "stickerWebpUrl": "//stk.zdn.vn/2101.webp" }
                ],
                "cateList": [
                    {
                        "stickers": [
                            { "id": "2102", "catId": "46", "type": 1 },
                            { "stickerId": 2101, "cateId": 45 }
                        ]
                    }
                ]
            }
        });
        let parsed = parse_sticker_response_items(&resp, "Gợi ý");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, 2101);
        assert_eq!(parsed[0].cate_id, 45);
        assert_eq!(parsed[0].url, "https://stk.zdn.vn/2101.webp");
        assert_eq!(parsed[0].category, "Gợi ý");
        assert_eq!(parsed[1].id, 2102);
        assert_eq!(parsed[1].cate_id, 46);
    }
}
