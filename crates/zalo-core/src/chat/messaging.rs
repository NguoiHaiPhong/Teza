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

fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

fn parse_single_sticker_value(
    it: &Value,
    fallback_id: Option<u64>,
    category: &str,
) -> Option<StickerItem> {
    if let Some(bare_id) = json_u64(it) {
        if bare_id == 0 {
            return None;
        }
        let url = sticker_image_url(bare_id, 0)?;
        return Some(StickerItem {
            id: bare_id,
            cate_id: 0,
            sticker_type: 1,
            url,
            category: category.to_string(),
        });
    }
    if !it.is_object() {
        return None;
    }

    let id = it
        .get("sticker_id")
        .or_else(|| it.get("stickerId"))
        .or_else(|| it.get("sid"))
        .or_else(|| it.get("id"))
        .and_then(json_u64)
        .filter(|&v| v > 0)
        .or_else(|| fallback_id.filter(|&v| v > 0))
        .unwrap_or(0);
    if id == 0 {
        return None;
    }
    let cate_id = it
        .get("cate_id")
        .or_else(|| it.get("cateId"))
        .or_else(|| it.get("cat_id"))
        .or_else(|| it.get("catId"))
        .and_then(json_u64)
        .unwrap_or(0);
    let sticker_type = it
        .get("type")
        .or_else(|| it.get("sticker_type"))
        .or_else(|| it.get("stickerType"))
        .and_then(json_u64)
        .unwrap_or(1);
    let direct_url = pick_str(
        it,
        &[
            "stickerWebpUrl",
            "sticker_webp_url",
            "stickerUrl",
            "sticker_url",
            "url",
        ],
    )
    .and_then(|u| crate::session::normalize_allowed_media_url(&u).ok())
    .map(|u| u.to_string());
    let url = direct_url.or_else(|| sticker_image_url(id, cate_id))?;
    Some(StickerItem {
        id,
        cate_id,
        sticker_type,
        url,
        category: category.to_string(),
    })
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
        for key in [
            "sugg_sticker",
            "sugg_sticker_v2",
            "suggSticker",
            "suggStickers",
            "stickers",
            "stickerList",
            "items",
            "list",
        ] {
            if let Some(arr) = data_node.get(key).and_then(Value::as_array) {
                raw_items.extend(arr.iter().cloned());
            }
        }
        for cate_key in ["cateList", "cate_list"] {
            if let Some(cates) = data_node.get(cate_key).and_then(Value::as_array) {
                for cat in cates {
                    for key in ["stickers", "items", "list"] {
                        if let Some(arr) = cat.get(key).and_then(Value::as_array) {
                            raw_items.extend(arr.iter().cloned());
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for it in raw_items {
        let Some(item) = parse_single_sticker_value(&it, None, category) else {
            continue;
        };
        if !seen.insert(item.id) {
            continue;
        }
        out.push(item);
    }
    out
}

pub fn parse_sticker_detail_item(
    dec: &Value,
    fallback_id: u64,
    category: &str,
) -> Option<StickerItem> {
    let data_owned: Value = match dec.get("data") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s.trim()).ok()?,
        Some(v) => v.clone(),
        None => dec.clone(),
    };
    let node = if let Some(arr) = data_owned.as_array() {
        arr.first()?.clone()
    } else {
        data_owned
    };
    let fallback = (fallback_id > 0).then_some(fallback_id);
    parse_single_sticker_value(&node, fallback, category)
}

async fn fetch_sticker_detail(
    session: &Session,
    sticker_id: u64,
    category: &str,
) -> Option<StickerItem> {
    let base = session.domains.sticker.trim_end_matches('/');
    if base.is_empty() || sticker_id == 0 {
        return None;
    }
    let body = obj(&[("sid", json!(sticker_id))]);
    let params = session.encoded_params(&body).ok()?;
    let url = format!(
        "{base}/api/message/sticker/sticker_detail?{}&params={params}",
        session.common_params()
    );
    let raw = tokio::time::timeout(std::time::Duration::from_secs(2), session.get_json(&url))
        .await
        .ok()?
        .ok()?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    parse_sticker_detail_item(&dec, sticker_id, category)
}

async fn hydrate_missing_sticker_details(
    session: &Session,
    mut items: Vec<StickerItem>,
    max_hydrate: usize,
) -> Vec<StickerItem> {
    let indices: Vec<(usize, u64, String)> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.cate_id == 0 && it.id > 30)
        .take(max_hydrate)
        .map(|(idx, it)| (idx, it.id, it.category.clone()))
        .collect();
    if indices.is_empty() {
        return items;
    }
    let mut set = tokio::task::JoinSet::new();
    for (idx, sid, cat) in indices {
        let sess_c = session.clone_shallow();
        set.spawn(async move { (idx, fetch_sticker_detail(&sess_c, sid, &cat).await) });
    }
    while let Some(res) = set.join_next().await {
        if let Ok((idx, Some(hydrated))) = res {
            if let Some(slot) = items.get_mut(idx) {
                if hydrated.cate_id > 0 {
                    slot.cate_id = hydrated.cate_id;
                }
                if hydrated.sticker_type > 0 {
                    slot.sticker_type = hydrated.sticker_type;
                }
                if crate::session::is_allowed_media_url(&hydrated.url) {
                    slot.url = hydrated.url;
                }
            }
        }
    }
    items
}

fn strip_vietnamese_diacritics(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ'
            | 'ặ' | 'ẳ' | 'ẵ' => 'a',
            'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' => 'e',
            'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' => 'i',
            'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ'
            | 'ợ' | 'ở' | 'ỡ' => 'o',
            'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' => 'u',
            'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
            'đ' => 'd',
            'À' | 'Á' | 'Ạ' | 'Ả' | 'Ã' | 'Â' | 'Ầ' | 'Ấ' | 'Ậ' | 'Ẩ' | 'Ẫ' | 'Ă' | 'Ằ' | 'Ắ'
            | 'Ặ' | 'Ẳ' | 'Ẵ' => 'a',
            'È' | 'É' | 'Ẹ' | 'Ẻ' | 'Ẽ' | 'Ê' | 'Ề' | 'Ế' | 'Ệ' | 'Ể' | 'Ễ' => 'e',
            'Ì' | 'Í' | 'Ị' | 'Ỉ' | 'Ĩ' => 'i',
            'Ò' | 'Ó' | 'Ọ' | 'Ỏ' | 'Õ' | 'Ô' | 'Ồ' | 'Ố' | 'Ộ' | 'Ổ' | 'Ỗ' | 'Ơ' | 'Ờ' | 'Ớ'
            | 'Ợ' | 'Ở' | 'Ỡ' => 'o',
            'Ù' | 'Ú' | 'Ụ' | 'Ủ' | 'Ũ' | 'Ư' | 'Ừ' | 'Ứ' | 'Ự' | 'Ử' | 'Ữ' => 'u',
            'Ỳ' | 'Ý' | 'Ỵ' | 'Ỷ' | 'Ỹ' => 'y',
            'Đ' => 'd',
            other => other,
        })
        .collect()
}

/// Produce ordered fallback suggestion keywords for Vietnamese/Korean/English shorthand
/// when the literal typed phrase returns no sticker suggestions.
pub fn canonical_sticker_fallback_keywords(keyword: &str) -> Vec<String> {
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let lower = trimmed.to_lowercase();
    let folded = strip_vietnamese_diacritics(&lower);

    let mut out: Vec<String> = Vec::new();
    let mut push_unique = |cand: &str| {
        let c = cand.trim();
        if !c.is_empty() && !c.eq_ignore_ascii_case(trimmed) && !out.iter().any(|x| x == c) {
            out.push(c.to_string());
        }
    };

    let mapped: Option<&[&str]> = match folded.as_str() {
        "tks" | "tk" | "ty" | "thx" | "cam on" | "camon" | "thank" | "thank you" | "감사"
        | "감사합니다" | "감사해요" | "고마워" | "고마워요" | "ㄱㅅ" => Some(&["thanks", "cảm ơn"]),
        "hihi" | "hehe" | "kaka" | "kkk" | "haha" | "hahaha" | "cuoi" | "lol" | "lmao"
        | "ㅋㅋ" | "ㅋㅋㅋ" | "ㅎㅎ" | "ㅎㅎㅎ" => Some(&["haha"]),
        "okie" | "oke" | "okela" | "oki" | "ok" | "duoc" | "dc" | "đc" | "알겠어"
        | "알겠습니다" | "오케이" | "ㅇㅋ" | "네" => Some(&["ok"]),
        "chao" | "xin chao" | "alo" | "helo" | "hello" | "hey" | "hi" | "안녕"
        | "안녕하세요" | "하이" | "ㅎㅇ" => Some(&["hi", "chào"]),
        "yeu" | "iu" | "thuong" | "tim" | "love" | "사랑" | "사랑해" | "좋아" | "좋아해" => {
            Some(&["love", "yêu"])
        }
        "buon" | "bun" | "khoc" | "huhu" | "sad" | "cry" | "ㅠㅠ" | "ㅜㅜ" | "슬퍼"
        | "울어" => Some(&["cry", "buồn"]),
        "gian" | "tuc" | "cay" | "angry" | "화나" | "짜증" => Some(&["angry", "giận"]),
        "ngu" | "ngu ngon" | "g9" | "gn" | "good night" | "sleep" | "잘자" | "굿나잇"
        | "졸려" => Some(&["sleep", "ngủ ngon"]),
        "tam biet" | "pp" | "bai" | "bye" | "goodbye" | "바이" | "잘가" | "안녕히" => {
            Some(&["bye", "tạm biệt"])
        }
        "chuc mung" | "cmsn" | "hpbd" | "vui" | "tuyet" | "happy" | "축하" | "축하해"
        | "행복" => Some(&["happy", "chúc mừng"]),
        "xin loi" | "sr" | "xl" | "sorry" | "미안" | "미안해" | "죄송" | "죄송합니다" => {
            Some(&["sorry", "xin lỗi"])
        }
        "de thuong" | "xinh" | "cute" | "귀여워" | "귀엽" => Some(&["cute"]),
        "doi" | "an" | "eat" | "배고파" | "밥" => Some(&["eat", "đói"]),
        "met" | "chan" | "tired" | "피곤" | "피곤해" => Some(&["tired", "mệt"]),
        _ => None,
    };

    if let Some(cands) = mapped {
        for cand in cands {
            push_unique(cand);
        }
    }
    if folded != lower {
        push_unique(&folded);
    }
    out
}

async fn fetch_sticker_suggest(
    session: &Session,
    keyword: &str,
    category: &str,
    hydrate_limit: usize,
) -> Vec<StickerItem> {
    let base = session.domains.sticker.trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }

    // Primary endpoint: modern Zalo Web sticker suggest endpoint (`/suggest/stickers`)
    let primary_body = obj(&[
        ("keyword", json!(keyword)),
        ("gif", json!(1)),
        ("guggy", json!(0)),
        ("imei", json!(session.imei)),
    ]);
    if let Ok(params) = session.encoded_params(&primary_body) {
        let url = format!(
            "{base}/api/message/sticker/suggest/stickers?{}&params={params}",
            session.common_params()
        );
        if let Ok(Ok(raw)) =
            tokio::time::timeout(std::time::Duration::from_secs(4), session.get_json(&url)).await
        {
            let dec = session.maybe_decrypt(raw, session.session_key_bytes());
            let items = parse_sticker_response_items(&dec, category);
            if !items.is_empty() {
                return hydrate_missing_sticker_details(session, items, hydrate_limit).await;
            }
        }
    }

    // Legacy fallback endpoint (`/suggest`)
    let body = obj(&[("keyword", json!(keyword)), ("imei", json!(session.imei))]);
    let Ok(params) = session.encoded_params(&body) else {
        return Vec::new();
    };
    let url = format!(
        "{base}/api/message/sticker/suggest?{}&params={params}",
        session.common_params()
    );
    let Ok(Ok(raw)) =
        tokio::time::timeout(std::time::Duration::from_secs(3), session.get_json(&url)).await
    else {
        return Vec::new();
    };
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    let items = parse_sticker_response_items(&dec, category);
    hydrate_missing_sticker_details(session, items, hydrate_limit).await
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
            set.spawn(async move { fetch_sticker_suggest(&sess_c, &kw_s, "Gợi ý", 4).await });
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
    let primary = fetch_sticker_suggest(session, trimmed, "Tìm kiếm", 12).await;
    if !primary.is_empty() {
        return Ok(primary);
    }
    for fallback in canonical_sticker_fallback_keywords(trimmed).into_iter().take(2) {
        let items = fetch_sticker_suggest(session, &fallback, "Tìm kiếm", 12).await;
        if !items.is_empty() {
            return Ok(items);
        }
    }
    Ok(Vec::new())
}

/// Fast real-time sticker suggestion query while typing in the composer.
pub async fn suggest_stickers(
    session: &Session,
    keyword: &str,
    limit: usize,
) -> Result<Vec<StickerItem>> {
    let trimmed = keyword.trim();
    let max_items = limit.clamp(1, 24);
    if trimmed.is_empty() || trimmed.chars().count() > 32 || !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let mut items = fetch_sticker_suggest(session, trimmed, "Gợi ý", max_items.min(12)).await;
    if items.is_empty() {
        for fallback in canonical_sticker_fallback_keywords(trimmed).into_iter().take(2) {
            items = fetch_sticker_suggest(session, &fallback, "Gợi ý", max_items.min(12)).await;
            if !items.is_empty() {
                break;
            }
        }
    }
    items.truncate(max_items);
    Ok(items)
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
        cli_msg_id: Some(client_id.to_string()),
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
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "jfif" | "webp" | "gif" | "bmp"
    )
}

pub fn mime_from_extension(file_name: &str) -> &'static str {
    let ext = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
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

fn is_synthetic_media_placeholder_line(line: &str) -> bool {
    matches!(
        line.trim().to_lowercase().as_str(),
        "[hình ảnh]"
            | "[ảnh gif]"
            | "[video]"
            | "[tin nhắn thoại]"
            | "[nhãn dán]"
            | "[hình ảnh / tệp đính kèm]"
            | "[tập tin]"
    )
}

fn is_file_placeholder_prefix_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    lower == "[tập tin]" || lower.starts_with("[tập tin] ")
}

/// Strips synthetic Teza media/file placeholder text (`"[Hình ảnh]"`, `"[Ảnh GIF]"`,
/// `"[Video]"`, `"[Tin nhắn thoại]"`, `"[Nhãn dán]"`, `"[Hình ảnh / Tệp đính kèm]"`,
/// `"[Tập tin]"`, and `"[Tập tin] <filename>"` prefix lines), returning only the real
/// user-authored caption (empty string when there is no caption).
pub fn extract_forward_caption(
    msg_type: &str,
    text: &str,
    attachment_name: Option<&str>,
) -> String {
    let raw = text.trim();
    if raw.is_empty() {
        return String::new();
    }

    let mt = msg_type.trim().to_ascii_lowercase();
    if matches!(mt.as_str(), "chat.sticker" | "sticker" | "5" | "35" | "36")
        || mt.contains("sticker")
    {
        return String::new();
    }

    let (first_line, rest) = match raw.find('\n') {
        Some(idx) => (raw[..idx].trim(), Some(raw[idx + 1..].trim())),
        None => (raw, None),
    };

    if is_synthetic_media_placeholder_line(first_line)
        || is_file_placeholder_prefix_line(first_line)
    {
        return rest.unwrap_or("").to_string();
    }

    let is_plain_text_msg = matches!(mt.as_str(), "chat.text" | "webchat" | "text" | "1");
    if !is_plain_text_msg {
        if let Some(att) = attachment_name.map(str::trim).filter(|s| {
            !s.is_empty()
                && !s.starts_with("teza-poll:")
                && !s.starts_with("teza-reminder:")
                && !s.starts_with("teza-call:")
        }) {
            if first_line.to_lowercase() == att.to_lowercase() {
                return rest.unwrap_or("").to_string();
            }
        }
    }

    raw.to_string()
}

/// Parses `(cate_id, sticker_id)` from `attachment_name` matching `sticker_{cid}_{sid}.*`
/// (with `sid > 0`), or extracts `eid={sid}` / `/thumb/{sid}.png` from `attachment_url`
/// (with `cate_id = 0` when unknown).
pub fn parse_sticker_ids_from_message(
    attachment_name: Option<&str>,
    attachment_url: Option<&str>,
) -> Option<(i64, i64)> {
    if let Some(name) = attachment_name.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(rest) = name.strip_prefix("sticker_") {
            let stem = rest.split('.').next().unwrap_or(rest);
            let mut parts = stem.split('_');
            if let (Some(cid_s), Some(sid_s)) = (parts.next(), parts.next()) {
                if let (Ok(cid), Ok(sid)) = (cid_s.parse::<i64>(), sid_s.parse::<i64>()) {
                    if cid >= 0 && sid > 0 {
                        return Some((cid, sid));
                    }
                }
            }
        }
    }

    if let Some(url) = attachment_url.map(str::trim).filter(|u| !u.is_empty()) {
        if let Some((_, query)) = url.split_once('?') {
            let query_no_frag = query.split('#').next().unwrap_or(query);
            for pair in query_no_frag.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k.eq_ignore_ascii_case("eid") {
                        if let Ok(sid) = v.trim().parse::<i64>() {
                            if sid > 0 {
                                return Some((0, sid));
                            }
                        }
                    }
                }
            }
        }
        let path_only = url.split(['?', '#']).next().unwrap_or(url);
        if let Some((_, after_thumb)) = path_only.rsplit_once("/thumb/") {
            if !after_thumb.contains('/') {
                if let Some(stem) = after_thumb.split('.').next() {
                    if let Ok(sid) = stem.trim().parse::<i64>() {
                        if sid > 0 {
                            return Some((0, sid));
                        }
                    }
                }
            }
        }
    }

    None
}

/// Returns `true` when the message represents a real forwardable media/file attachment
/// (`chat.photo`, `chat.gif`, `chat.file`, `share.file`, `file`, `chat.video`,
/// `chat.video.msg`, `chat.voice`, `"2"`, `"3"`, `"4"`, `"19"`, etc.), and `false` for
/// link/card/poll/reminder/call/sticker/text messages whose `attachment_url` or
/// `attachment_name` is only a thumbnail or structured metadata.
pub fn is_forwardable_media_message(
    msg_type: &str,
    attachment_url: Option<&str>,
    attachment_name: Option<&str>,
) -> bool {
    let has_url = attachment_url.map(str::trim).is_some_and(|s| !s.is_empty());
    let clean_name = attachment_name.map(str::trim).filter(|s| !s.is_empty());
    if !has_url && clean_name.is_none() {
        return false;
    }
    if let Some(name) = clean_name {
        if name.starts_with("teza-poll:")
            || name.starts_with("teza-reminder:")
            || name.starts_with("teza-call:")
        {
            return false;
        }
    }

    let mt = msg_type.trim().to_ascii_lowercase();
    if mt.is_empty()
        || matches!(
            mt.as_str(),
            "chat.text"
                | "webchat"
                | "text"
                | "1"
                | "chat.link"
                | "link"
                | "38"
                | "chat.location"
                | "chat.location.new"
                | "43"
                | "chat.recommended"
                | "chat.ecard"
                | "chat.poll"
                | "group.poll"
                | "poll"
                | "chat.reminder"
                | "reminder"
                | "chat.call"
                | "call"
                | "chat.sticker"
                | "sticker"
                | "5"
                | "35"
                | "36"
        )
        || mt.contains("poll")
        || mt.contains("reminder")
        || mt.contains("call")
        || mt.contains("sticker")
        || mt.contains("undo")
        || mt.starts_with("group.")
    {
        return false;
    }

    matches!(
        mt.as_str(),
        "chat.photo"
            | "photo"
            | "2"
            | "32"
            | "chat.gif"
            | "gif"
            | "49"
            | "chat.file"
            | "share.file"
            | "file"
            | "19"
            | "46"
            | "chat.video"
            | "chat.video.msg"
            | "video"
            | "4"
            | "44"
            | "chat.voice"
            | "voice"
            | "chat.audio"
            | "audio"
            | "3"
            | "31"
            | "chat.doodle"
            | "37"
    ) || mt.contains("photo")
        || mt.contains("gif")
        || mt.contains("file")
        || mt.contains("video")
        || mt.contains("voice")
        || mt.contains("audio")
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
        cli_msg_id: Some(client_id.to_string()),
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
    let qmsg_cli_id = quoted
        .cli_msg_id
        .as_deref()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(qmsg_ts);

    let (url, body) = if is_group {
        let grid = clean_group_id(thread_id);
        let body = obj(&[
            ("grid", json!(grid)),
            ("message", json!(text)),
            ("clientId", json!(client_id)),
            ("language", json!("vi")),
            ("qmsgOwner", json!(qmsg_owner)),
            ("qmsgId", json!(qmsg_id_num)),
            ("qmsgCliId", json!(qmsg_cli_id)),
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
            ("qmsgCliId", json!(qmsg_cli_id)),
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
        cli_msg_id: Some(client_id.to_string()),
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

/// Map message `msg_type` to Zalo's numeric client message type (`getClientMessageType`).
pub fn reaction_client_msg_type(msg_type: &str) -> i32 {
    match msg_type.trim().to_ascii_lowercase().as_str() {
        "webchat" | "chat.text" | "1" => 1,
        "chat.voice" | "voice" | "3" | "31" => 31,
        "chat.photo" | "photo" | "2" | "32" => 32,
        "chat.sticker" | "sticker" | "5" | "36" => 36,
        "chat.doodle" | "37" => 37,
        "chat.recommended" | "chat.link" | "link" | "38" => 38,
        "chat.location" | "chat.location.new" | "43" => 43,
        "chat.video.msg" | "chat.video" | "video" | "4" | "44" => 44,
        "share.file" | "chat.file" | "file" | "19" | "46" => 46,
        "chat.gif" | "gif" | "49" => 49,
        _ => 1,
    }
}

fn known_reaction_code_and_type(trimmed: &str) -> Option<(&'static str, i32)> {
    match trimmed {
        "👍" | "/-strong" => Some(("/-strong", 3)),
        "❤️" | "❤" | "/-heart" => Some(("/-heart", 5)),
        "😆" | ":>" => Some((":>", 0)),
        "😮" | ":o" => Some((":o", 32)),
        "😢" | "😭" | ":-((" => Some((":-((", 2)),
        "😠" | "😡" | ":-h" => Some((":-h", 20)),
        "😘" | ":-*" => Some((":-*", 8)),
        "😂" | ":')" => Some((":')", 7)),
        "💩" | "/-shit" => Some(("/-shit", 66)),
        "🌹" | "/-rose" => Some(("/-rose", 120)),
        "💔" | "/-break" => Some(("/-break", 65)),
        "👎" | "/-weak" => Some(("/-weak", 4)),
        "😍" | ";xx" => Some((";xx", 29)),
        "😕" | ";-/" => Some((";-/", 51)),
        "😉" | ";-)" => Some((";-)", 45)),
        "🥀" | "/-fade" => Some(("/-fade", 121)),
        "☀️" | "☀" | "/-li" => Some(("/-li", 67)),
        "🎂" | "/-bd" => Some(("/-bd", 126)),
        "💣" | "/-bome" => Some(("/-bome", 127)),
        "👌" | "/-ok" => Some(("/-ok", 68)),
        "✌️" | "✌" | "/-v" => Some(("/-v", 69)),
        "🙏" | "/-thanks" => Some(("/-thanks", 70)),
        "_()_" => Some(("_()_", 73)),
        "👊" | "/-punch" => Some(("/-punch", 71)),
        "/-share" => Some(("/-share", 72)),
        "🙅" | "/-no" => Some(("/-no", 131)),
        "/-bad" => Some(("/-bad", 132)),
        "/-loveu" => Some(("/-loveu", 133)),
        "--b" => Some(("--b", 1)),
        ":((" => Some((":((", 16)),
        "x-)" => Some(("x-)", 21)),
        "🤓" | "8-)" => Some(("8-)", 22)),
        "😁" | ";-d" => Some((";-d", 23)),
        "😎" | "b-)" => Some(("b-)", 26)),
        "😐" | ":--|" => Some((":--|", 30)),
        "p-(" => Some(("p-(", 35)),
        "👋" | ":-bye" => Some((":-bye", 36)),
        "😴" | "|-)" => Some(("|-)", 38)),
        ":wipe" => Some((":wipe", 39)),
        ":-dig" => Some((":-dig", 42)),
        "&-(" => Some(("&-(", 44)),
        "👏" | ":handclap" => Some((":handclap", 46)),
        ">-|" => Some((">-|", 47)),
        ":-f" => Some((":-f", 48)),
        ":-l" => Some((":-l", 49)),
        ":-r" => Some((":-r", 50)),
        "🤐" | ";-x" => Some((";-x", 52)),
        ":-o" => Some((":-o", 53)),
        "😳" | ";-s" => Some((";-s", 54)),
        "😨" | ";-a" => Some((";-a", 60)),
        ":-<" => Some((":-<", 61)),
        ":))" => Some((":))", 62)),
        "🤑" | "$-)" => Some(("$-)", 63)),
        "🍺" | "/-beer" => Some(("/-beer", 99)),
        _ => None,
    }
}

pub fn reaction_code_and_type(emoji: &str) -> (&'static str, i32) {
    known_reaction_code_and_type(emoji.trim()).unwrap_or(("/-heart", 5))
}

/// Resolve `(rIcon, rType)` for the Zalo `/api/{message,group}/reaction` wire payload.
/// Removal (`remove == true`) always sends `("", -1)` (`Reactions.NONE` in `zca-js`).
/// Known emojis map to exact `Reactions` shortcodes + `rType`; custom Unicode emojis
/// pass through verbatim with `rType = 75`.
pub fn resolve_reaction_wire(emoji: &str, remove: bool) -> (String, i32) {
    if remove {
        return (String::new(), -1);
    }
    let trimmed = emoji.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "/-none" | "/-remove" | "none"
        )
    {
        return (String::new(), -1);
    }
    if let Some((code, r_type)) = known_reaction_code_and_type(trimmed) {
        return (code.to_string(), r_type);
    }
    if !trimmed.is_ascii() {
        return (trimmed.to_string(), 75);
    }
    ("/-heart".to_string(), 5)
}

/// Build the `(endpoint_path, body_json)` for `/api/message/reaction` (DM/Cloud) or
/// `/api/group/reaction` (Group) matching Zalo Web / `zca-js` `addReaction.ts`.
pub fn build_reaction_request_payload(
    thread_id: &str,
    kind: ThreadKind,
    msg: &MessageDto,
    emoji: &str,
    remove: bool,
    imei: &str,
    client_id: u64,
) -> Result<(&'static str, Value)> {
    let raw_mid = msg.msg_id.trim();
    if raw_mid.is_empty() || raw_mid.starts_with("local-") || raw_mid.starts_with("out_") {
        anyhow::bail!("Tin nhắn chưa đồng bộ ID máy chủ, không thể thả cảm xúc");
    }
    let g_msg_id = raw_mid
        .parse::<i64>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| anyhow::anyhow!("Mã tin nhắn không hợp lệ để thả cảm xúc"))?;

    let c_msg_id = msg
        .cli_msg_id
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(if msg.ts > 0 { msg.ts } else { client_id as i64 });

    let wire_msg_type = reaction_client_msg_type(&msg.msg_type);
    let (r_icon, r_type) = resolve_reaction_wire(emoji, remove);

    let inner = json!({
        "rMsg": [{
            "gMsgID": g_msg_id,
            "cMsgID": c_msg_id,
            "msgType": wire_msg_type,
        }],
        "rIcon": r_icon,
        "rType": r_type,
        "source": 6,
    })
    .to_string();
    let react_list = json!([{ "message": inner, "clientId": client_id }]);

    if kind == ThreadKind::Group {
        let grid = clean_group_id(thread_id);
        if grid.is_empty() || grid == "0" {
            anyhow::bail!("Mã nhóm không hợp lệ để thả cảm xúc");
        }
        Ok((
            "/api/group/reaction",
            obj(&[
                ("react_list", react_list),
                ("grid", json!(grid)),
                ("imei", json!(imei)),
            ]),
        ))
    } else {
        let toid = thread_id.trim();
        if toid.is_empty() || toid == "0" {
            anyhow::bail!("Mã hội thoại không hợp lệ để thả cảm xúc");
        }
        Ok((
            "/api/message/reaction",
            obj(&[("react_list", react_list), ("toid", json!(toid))]),
        ))
    }
}

fn reaction_candidate_hosts(session: &Session) -> Vec<String> {
    let mut hosts = Vec::new();
    let push_unique = |list: &mut Vec<String>, raw: &str| {
        let trimmed = raw.trim().trim_end_matches('/');
        if !trimmed.is_empty() && !list.iter().any(|h| h.eq_ignore_ascii_case(trimmed)) {
            list.push(trimmed.to_string());
        }
    };
    for candidate in session.domains.candidates("reaction") {
        push_unique(&mut hosts, &candidate);
    }
    if let Some(svc) = crate::endpoints::service("reaction") {
        push_unique(&mut hosts, svc);
    }
    push_unique(&mut hosts, crate::endpoints::REACTION_FALLBACK);
    hosts
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
    let (path, body) =
        build_reaction_request_payload(thread_id, kind, msg, emoji, remove, &session.imei, now)?;
    let params = session.encoded_params(&body)?;
    let hosts = reaction_candidate_hosts(session);

    let mut last_err = anyhow::anyhow!("Không có máy chủ phản hồi cho yêu cầu thả cảm xúc");
    for host in &hosts {
        let url = format!("{host}{path}?{}", session.common_params());
        match session.post_form(&url, format!("params={params}")).await {
            Ok(raw) => {
                let dec = session.maybe_decrypt(raw, session.session_key_bytes());
                if dec.is_object() {
                    return check_error_code(&dec);
                }
                last_err = anyhow::anyhow!("Phản hồi không hợp lệ từ máy chủ cảm xúc");
            }
            Err(e) => {
                if matches!(e.downcast_ref::<SessionError>(), Some(SessionError::Expired { .. })) {
                    return Err(e);
                }
                last_err = e;
            }
        }
    }
    Err(last_err)
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
    let cli_id = msg
        .cli_msg_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "0")
        .map(str::to_string)
        .unwrap_or_else(|| {
            if msg.ts > 0 {
                msg.ts.to_string()
            } else {
                now.to_string()
            }
        });

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
        cli_msg_id: Some(ctx.client_id.to_string()),
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
        cli_msg_id: Some(ctx.client_id.to_string()),
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

#[allow(clippy::too_many_arguments)]
async fn send_attachment_reader<R: AsyncRead + AsyncSeek + Unpin>(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    file_name: &str,
    total_size: u64,
    caption: &str,
    force_image: Option<bool>,
    reader: &mut R,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    validate_attachment_size(total_size)?;
    let send_as_image = force_image.unwrap_or_else(|| is_image_extension(file_name));
    let mut mime = mime_from_extension(file_name);
    if send_as_image && !mime.starts_with("image/") {
        mime = "image/jpeg";
    }
    let is_group = kind == ThreadKind::Group;
    let ctx = UploadTarget::new(
        session, thread_id, is_group, file_name, mime, total_size, caption,
    );
    if send_as_image {
        send_photo_reader(session, thread_id, &ctx, reader).await
    } else {
        send_file_reader(session, thread_id, &ctx, reader).await
    }
}

pub async fn send_attachment_file_named(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    path: &Path,
    file_name: &str,
    caption: &str,
    force_image: Option<bool>,
) -> Result<MessageDto> {
    let meta = tokio::fs::metadata(path).await?;
    if !meta.is_file() {
        anyhow::bail!("Đường dẫn không phải tệp hợp lệ");
    }
    let total_size = meta.len();
    validate_attachment_size(total_size)?;
    let effective_name = Path::new(file_name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
        })
        .unwrap_or("attachment.bin");
    let mut file = tokio::fs::File::open(path).await?;
    send_attachment_reader(
        session,
        thread_id,
        kind,
        effective_name,
        total_size,
        caption,
        force_image,
        &mut file,
    )
    .await
}

pub async fn send_attachment_file(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    path: &Path,
    caption: &str,
) -> Result<MessageDto> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("attachment.bin");
    send_attachment_file_named(session, thread_id, kind, path, file_name, caption, None).await
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
        None,
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

    #[test]
    fn parse_sticker_response_items_supports_snake_case_sugg_sticker_and_bare_ids() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "sugg_sticker": [
                    { "sticker_id": 23339, "cate_id": 10425, "type": 7, "sticker_webp_url": "//stk.zdn.vn/23339.webp" },
                    { "sticker_id": "23340", "cate_id": "10425", "sticker_type": "3" },
                    23341,
                    { "sticker_id": 23339, "cate_id": 10425 }
                ]
            }
        });
        let parsed = parse_sticker_response_items(&resp, "Gợi ý");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].id, 23339);
        assert_eq!(parsed[0].cate_id, 10425);
        assert_eq!(parsed[0].sticker_type, 7);
        assert_eq!(parsed[0].url, "https://stk.zdn.vn/23339.webp");
        assert_eq!(parsed[1].id, 23340);
        assert_eq!(parsed[1].cate_id, 10425);
        assert_eq!(parsed[1].sticker_type, 3);
        assert_eq!(parsed[2].id, 23341);
        assert_eq!(parsed[2].cate_id, 0);
    }

    #[test]
    fn parse_sticker_detail_item_extracts_cate_type_and_webp_url() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "id": 23341,
                "cateId": 10888,
                "type": 7,
                "stickerWebpUrl": "https://api.zaloapp.com/api/emoticon/sticker/webpc?eid=23341&size=130"
            }
        });
        let item = parse_sticker_detail_item(&resp, 23341, "Gợi ý").expect("detail item");
        assert_eq!(item.id, 23341);
        assert_eq!(item.cate_id, 10888);
        assert_eq!(item.sticker_type, 7);
        assert!(item.url.contains("eid=23341"));
    }

    #[test]
    fn parse_sticker_detail_item_applies_fallback_id_and_rejects_untrusted_direct_urls() {
        // 1. Response omitting id/sid must hydrate using fallback_id before id==0 guard & URL fallback
        let omitted_id = json!({
            "error_code": 0,
            "data": {
                "cateId": 10888,
                "type": 7
            }
        });
        let hydrated =
            parse_sticker_detail_item(&omitted_id, 23341, "Gợi ý").expect("fallback_id should hydrate");
        assert_eq!(hydrated.id, 23341);
        assert_eq!(hydrated.cate_id, 10888);
        assert_eq!(hydrated.sticker_type, 7);
        assert!(crate::session::is_allowed_media_url(&hydrated.url));
        assert!(hydrated.url.contains("eid=23341"));

        // 2. Response with id == 0 also uses fallback_id
        let zero_id = json!({
            "error_code": 0,
            "data": {
                "id": 0,
                "cate_id": 10999,
                "sticker_type": 3
            }
        });
        let hydrated_zero =
            parse_sticker_detail_item(&zero_id, 23342, "Gợi ý").expect("zero id should use fallback_id");
        assert_eq!(hydrated_zero.id, 23342);
        assert_eq!(hydrated_zero.cate_id, 10999);
        assert_eq!(hydrated_zero.sticker_type, 3);

        // 3. Non-object null payload must NOT fabricate an item from fallback_id
        let null_payload = json!({ "error_code": 0, "data": null });
        assert!(parse_sticker_detail_item(&null_payload, 23341, "Gợi ý").is_none());

        // 4. Third-party direct_url hosts must be rejected and fall back to trusted Zalo CDN URL
        let untrusted = json!({
            "error_code": 0,
            "data": {
                "suggSticker": [
                    {
                        "stickerId": 2105,
                        "cateId": 55,
                        "type": 7,
                        "stickerWebpUrl": "https://evil.example/tracker.webp"
                    },
                    {
                        "stickerId": 2106,
                        "cateId": 56,
                        "type": 7,
                        "url": "http://stk.zdn.vn/2106.webp"
                    }
                ]
            }
        });
        let items = parse_sticker_response_items(&untrusted, "Gợi ý");
        assert_eq!(items.len(), 2);
        assert!(
            !items[0].url.contains("evil.example"),
            "third-party direct_url must be discarded, got: {}",
            items[0].url
        );
        assert!(crate::session::is_allowed_media_url(&items[0].url));
        assert!(items[0].url.contains("eid=2105"));
        assert_eq!(items[1].url, "https://stk.zdn.vn/2106.webp");
    }

    #[test]
    fn canonical_sticker_fallback_keywords_handles_vi_ko_and_shorthand() {
        let vi = canonical_sticker_fallback_keywords("cảm ơn");
        assert!(vi.contains(&"thanks".to_string()));
        assert!(vi.contains(&"cam on".to_string()));

        let short = canonical_sticker_fallback_keywords("okie");
        assert_eq!(short, vec!["ok".to_string()]);

        let ko = canonical_sticker_fallback_keywords("감사합니다");
        assert!(ko.contains(&"thanks".to_string()));

        let ko_laugh = canonical_sticker_fallback_keywords("ㅋㅋㅋ");
        assert_eq!(ko_laugh, vec!["haha".to_string()]);

        let gn = canonical_sticker_fallback_keywords("g9");
        assert!(gn.contains(&"sleep".to_string()));
    }

    #[test]
    fn build_reaction_request_payload_matches_zca_js_for_dm_group_icons_and_unreact() {
        let msg = MessageDto {
            msg_id: "7970889894523".into(),
            cli_msg_id: Some("1782286269667".into()),
            thread_id: "999888777".into(),
            sender_id: "999888777".into(),
            sender_name: "Alice".into(),
            text: "hello".into(),
            msg_type: "chat.text".into(),
            ts: 1_782_286_279_518,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        };

        // 1. Verify all 6 primary palette emojis + aliases + custom emoji
        let expected = [
            ("❤️", "/-heart", 5),
            ("❤", "/-heart", 5),
            ("👍", "/-strong", 3),
            ("😆", ":>", 0),
            ("😮", ":o", 32),
            ("😢", ":-((", 2),
            ("😭", ":-((", 2),
            ("😠", ":-h", 20),
            ("😡", ":-h", 20),
            ("🔥", "🔥", 75),
        ];
        for (emoji, exp_icon, exp_type) in expected {
            let (path, body) = build_reaction_request_payload(
                "999888777",
                ThreadKind::Direct,
                &msg,
                emoji,
                false,
                "imei-1",
                1_782_286_300_000,
            )
            .unwrap();
            assert_eq!(path, "/api/message/reaction");
            assert_eq!(body["toid"], "999888777");
            assert!(body.get("grid").is_none());
            let inner: Value =
                serde_json::from_str(body["react_list"][0]["message"].as_str().unwrap()).unwrap();
            assert_eq!(inner["rIcon"], exp_icon);
            assert_eq!(inner["rType"], exp_type);
            assert_eq!(inner["source"], 6);
            assert_eq!(inner["rMsg"][0]["gMsgID"], json!(7_970_889_894_523_i64));
            assert_eq!(inner["rMsg"][0]["cMsgID"], json!(1_782_286_269_667_i64));
            assert!(inner["rMsg"][0]["gMsgID"].is_i64());
            assert!(inner["rMsg"][0]["cMsgID"].is_i64());
            assert_eq!(inner["rMsg"][0]["msgType"], 1);
            assert_eq!(body["react_list"][0]["clientId"], 1_782_286_300_000_u64);
        }

        // 2. Group endpoint strips `group_`/`g` prefix, includes `grid` + `imei`, maps photo msgType=32
        let mut photo_msg = msg.clone();
        photo_msg.msg_type = "chat.photo".into();
        let (path, body) = build_reaction_request_payload(
            "group_7560225984834507848",
            ThreadKind::Group,
            &photo_msg,
            "👍",
            false,
            "imei-grp",
            1_782_286_300_001,
        )
        .unwrap();
        assert_eq!(path, "/api/group/reaction");
        assert_eq!(body["grid"], "7560225984834507848");
        assert_eq!(body["imei"], "imei-grp");
        assert!(body.get("toid").is_none());
        let inner: Value =
            serde_json::from_str(body["react_list"][0]["message"].as_str().unwrap()).unwrap();
        assert_eq!(inner["rMsg"][0]["msgType"], 32);
        assert_eq!(inner["rIcon"], "/-strong");
        assert_eq!(inner["rType"], 3);

        // 3. Unreact MUST send `rIcon: ""` and `rType: -1` (zca-js Reactions.NONE)
        let (path, body) = build_reaction_request_payload(
            "999888777",
            ThreadKind::Direct,
            &msg,
            "❤️",
            true,
            "imei-1",
            1_782_286_300_002,
        )
        .unwrap();
        assert_eq!(path, "/api/message/reaction");
        let inner: Value =
            serde_json::from_str(body["react_list"][0]["message"].as_str().unwrap()).unwrap();
        assert_eq!(inner["rIcon"], "");
        assert_eq!(inner["rType"], -1);
        assert_eq!(inner["source"], 6);

        // 4. Legacy message without `cli_msg_id` falls back cleanly to `msg.ts`
        let mut legacy = msg.clone();
        legacy.cli_msg_id = None;
        let (_, body) = build_reaction_request_payload(
            "999888777",
            ThreadKind::Cloud,
            &legacy,
            "❤️",
            false,
            "imei-1",
            1_782_286_300_003,
        )
        .unwrap();
        let inner: Value =
            serde_json::from_str(body["react_list"][0]["message"].as_str().unwrap()).unwrap();
        assert_eq!(inner["rMsg"][0]["cMsgID"], json!(1_782_286_279_518_i64));

        // 5. Rejects unsynced local placeholder IDs
        let mut local = msg.clone();
        local.msg_id = "local-1782286279518".into();
        assert!(build_reaction_request_payload(
            "999888777",
            ThreadKind::Direct,
            &local,
            "❤️",
            false,
            "imei-1",
            1_782_286_300_004,
        )
        .is_err());

        // 6. Client message type mapping
        assert_eq!(reaction_client_msg_type("chat.text"), 1);
        assert_eq!(reaction_client_msg_type("chat.voice"), 31);
        assert_eq!(reaction_client_msg_type("chat.photo"), 32);
        assert_eq!(reaction_client_msg_type("chat.sticker"), 36);
        assert_eq!(reaction_client_msg_type("chat.link"), 38);
        assert_eq!(reaction_client_msg_type("chat.location.new"), 43);
        assert_eq!(reaction_client_msg_type("chat.video"), 44);
        assert_eq!(reaction_client_msg_type("chat.file"), 46);
        assert_eq!(reaction_client_msg_type("chat.gif"), 49);
    }

    #[test]
    fn jfif_extension_is_recognized_as_jpeg_image() {
        assert!(is_image_extension("a.jfif"));
        assert!(is_image_extension("PHOTO.JFIF"));
        assert_eq!(mime_from_extension("a.jfif"), "image/jpeg");
        assert_eq!(mime_from_extension("PHOTO.JFIF"), "image/jpeg");
    }

    #[test]
    fn extract_forward_caption_strips_synthetic_placeholders_and_preserves_real_captions() {
        assert_eq!(extract_forward_caption("chat.photo", "[Hình ảnh]", Some("a.jpg")), "");
        assert_eq!(extract_forward_caption("chat.gif", "[Ảnh GIF]", None), "");
        assert_eq!(extract_forward_caption("chat.video", "[Video]", None), "");
        assert_eq!(extract_forward_caption("chat.voice", "[Tin nhắn thoại]", None), "");
        assert_eq!(extract_forward_caption("chat.sticker", "[Nhãn dán]", None), "");
        assert_eq!(extract_forward_caption("chat.sticker", "Bất kỳ văn bản nào", None), "");
        assert_eq!(
            extract_forward_caption("chat.file", "[Hình ảnh / Tệp đính kèm]", None),
            ""
        );
        assert_eq!(extract_forward_caption("chat.file", "[Tập tin]", Some("doc.pdf")), "");
        assert_eq!(
            extract_forward_caption("chat.file", "[Tập tin] bao_cao.pdf", Some("bao_cao.pdf")),
            ""
        );
        assert_eq!(
            extract_forward_caption(
                "chat.file",
                "[Tập tin] bao_cao.pdf\nGửi anh bản chốt",
                Some("bao_cao.pdf")
            ),
            "Gửi anh bản chốt"
        );
        assert_eq!(
            extract_forward_caption("chat.file", "bao_cao.pdf", Some("bao_cao.pdf")),
            ""
        );
        assert_eq!(
            extract_forward_caption("chat.file", "bao_cao.pdf\nXem giúp em", Some("bao_cao.pdf")),
            "Xem giúp em"
        );
        assert_eq!(
            extract_forward_caption("chat.photo", "Ảnh chụp sáng nay", Some("img.jpg")),
            "Ảnh chụp sáng nay"
        );
        assert_eq!(
            extract_forward_caption("chat.photo", "[Hình ảnh]\nChuyến đi Đà Lạt", Some("img.jpg")),
            "Chuyến đi Đà Lạt"
        );
        assert_eq!(
            extract_forward_caption("chat.text", "report.pdf", Some("report.pdf")),
            "report.pdf"
        );
    }

    #[test]
    fn parse_sticker_ids_from_message_extracts_from_attachment_name_and_url() {
        assert_eq!(
            parse_sticker_ids_from_message(Some("sticker_1002_2005.png"), None),
            Some((1002, 2005))
        );
        assert_eq!(
            parse_sticker_ids_from_message(Some("sticker_0_15.webp"), None),
            Some((0, 15))
        );
        assert_eq!(
            parse_sticker_ids_from_message(Some("sticker_10_0.png"), None),
            None
        );
        assert_eq!(
            parse_sticker_ids_from_message(
                None,
                Some("https://api.zaloapp.com/api/emoticon/sticker/webpc?eid=2005&size=130")
            ),
            Some((0, 2005))
        );
        assert_eq!(
            parse_sticker_ids_from_message(
                None,
                Some("https://stc-chat.zdn.vn/images/stickers/default/thumb/15.png")
            ),
            Some((0, 15))
        );
        assert_eq!(
            parse_sticker_ids_from_message(
                Some("sticker_10_0.png"),
                Some("https://api.zaloapp.com/api/emoticon/sticker/webpc?size=130&eid=777")
            ),
            Some((0, 777))
        );
        assert_eq!(
            parse_sticker_ids_from_message(
                Some("photo.jpg"),
                Some("https://cdn.zdn.vn/photo/12345.jpg")
            ),
            None
        );
    }

    #[test]
    fn is_forwardable_media_message_matches_real_media_and_rejects_cards_or_text() {
        for mt in [
            "chat.photo",
            "chat.gif",
            "chat.file",
            "share.file",
            "file",
            "chat.video",
            "chat.video.msg",
            "chat.voice",
            "2",
            "3",
            "4",
            "19",
            "31",
            "32",
            "44",
            "46",
            "49",
        ] {
            assert!(
                is_forwardable_media_message(mt, Some("https://cdn.zdn.vn/media/a.bin"), None),
                "expected {mt} to be forwardable media"
            );
        }
        // Vault-only file with attachment_name but no remote URL is still forwardable.
        assert!(is_forwardable_media_message("chat.file", None, Some("report.pdf")));

        // Recalled / empty media without URL or filename is NOT forwardable.
        assert!(!is_forwardable_media_message("chat.photo", None, None));

        // Link/card/poll/reminder/call/sticker/text messages are NOT forwardable media.
        for mt in [
            "chat.link",
            "link",
            "chat.location",
            "chat.location.new",
            "chat.recommended",
            "chat.ecard",
            "chat.poll",
            "chat.reminder",
            "chat.call",
            "chat.text",
            "webchat",
            "chat.sticker",
            "5",
            "36",
            "group.action",
        ] {
            assert!(
                !is_forwardable_media_message(
                    mt,
                    Some("https://cdn.zdn.vn/thumb.jpg"),
                    Some("thumb.jpg")
                ),
                "expected {mt} NOT to be forwardable media"
            );
        }
        assert!(!is_forwardable_media_message(
            "chat.file",
            None,
            Some("teza-poll:{\"poll_id\":1}")
        ));
    }
}
