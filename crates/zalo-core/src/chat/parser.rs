//! Parsing Zalo thread/message JSON payloads and WebSocket events into clean DTOs.

use serde_json::Value;

use crate::chat::{MessageDto, QuoteRef, ReactionSummary, ThreadDto, ThreadKind};
use crate::socket::{
    WsEvent, CMD_CTRL_ACTION, CMD_CTRL_PULL_CTRL, CMD_CTRL_PUSH_CTRL, CMD_GET_MSG,
    CMD_PULL_MSG_1_1, CMD_PULL_MSG_513, CMD_PULL_MSG_514, CMD_PULL_MSG_515, CMD_PULL_MSG_517,
    CMD_PULL_MSG_518, CMD_PULL_MSG_GROUP, CMD_PUSH_MISS_MSG, CMD_PUSH_MSG_1_1, CMD_PUSH_MSG_GROUP,
};

#[derive(Debug, Clone)]
pub enum WsParsedAction {
    Message(Box<MessageDto>),
    Recalled { thread_id: String, msg_id: String },
    Read { thread_id: String, is_unread: bool },
    Reaction {
        thread_id: String,
        msg_id: String,
        emoji: Option<String>,
        me: bool,
        removed: bool,
    },
}

pub fn pick_str(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(s)) if !s.trim().is_empty() => return Some(s.trim().to_string()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

pub fn clean_group_id(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches("group_")
        .trim_start_matches('g')
        .trim_start_matches('G')
}

pub fn normalize_media_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let normalized = if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
        if host.ends_with(".zdn.vn")
            || host.ends_with(".zadn.vn")
            || host.ends_with(".zaloapp.com")
            || host.ends_with(".zalo.me")
            || host.ends_with(".zalo.vn")
        {
            format!("https://{rest}")
        } else {
            return String::new();
        }
    } else {
        trimmed.to_string()
    };
    if normalized.starts_with("https://") {
        normalized
    } else {
        String::new()
    }
}

pub fn normalize_avatar_url(raw: &str) -> String {
    let url = normalize_media_url(raw);
    if url.is_empty() || url.trim_end_matches('/').ends_with("/default") {
        String::new()
    } else {
        url
    }
}

pub fn is_group_flag(v: &Value) -> bool {
    v.get("isGroup")
        .map(|g| g.as_bool() == Some(true) || g.as_i64() == Some(1))
        .unwrap_or(false)
}

/// Chuẩn hóa Unix timestamp (giây 10 chữ số -> mili-giây) để so sánh và sắp xếp nhất quán.
pub fn normalize_ts_ms(ts: i64) -> i64 {
    if (1_000_000_000..10_000_000_000).contains(&ts) {
        ts.saturating_mul(1000)
    } else {
        ts
    }
}

pub fn is_raw_numeric_id(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 6 && t.chars().all(|c| c.is_ascii_digit())
}

/// Định dạng preview hội thoại giống Zalo PC (`Bạn: ...` hoặc `Tên: ...` trong nhóm).
pub fn format_thread_preview(
    text: &str,
    is_outgoing: bool,
    sender_name: &str,
    is_group: bool,
) -> String {
    let single_line = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if single_line.is_empty() {
        return String::new();
    }
    let clean_name = sender_name.trim();
    if is_outgoing {
        if single_line.starts_with("Bạn:") {
            single_line
        } else {
            format!("Bạn: {single_line}")
        }
    } else if is_group
        && !clean_name.is_empty()
        && clean_name != "Bạn"
        && !is_raw_numeric_id(clean_name)
    {
        format!("{clean_name}: {single_line}")
    } else {
        single_line
    }
}

pub fn parse_friend_thread(v: &Value) -> Option<ThreadDto> {
    let id = pick_str(v, &["userId", "uid", "id", "friendId", "globalId"])?;
    let title = pick_str(v, &["alias", "displayName", "zaloName", "display_name", "name"])
        .or_else(|| {
            v.get("profile")
                .and_then(|p| pick_str(p, &["alias", "displayName", "zaloName"]))
        })
        .unwrap_or_else(|| id.clone());
    let avatar = pick_str(v, &["avatar", "avt", "profile_avatar"])
        .map(|u| normalize_avatar_url(&u))
        .unwrap_or_default();

    Some(ThreadDto {
        id,
        title,
        avatar,
        kind: ThreadKind::Direct,
        last_preview: String::new(),
        // Không dùng `lastActionTime` của danh bạ vì đây là hoạt động profile, không phải tin nhắn cuối.
        last_ts: 0,
        unread_count: 0,
        is_pinned: false,
        is_muted: false,
    })
}

fn normalize_msg_type(it: &Value) -> String {
    pick_str(it, &["msgType", "type", "cmd"]).unwrap_or_else(|| "chat.text".to_string())
}

pub fn extract_content_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed == "sendBubbleMessage" {
                return None;
            }
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return extract_content_text(&parsed);
                }
            }
            Some(s.clone())
        }
        Value::Object(_) => {
            if let Some(s) = pick_str(
                v,
                &[
                    "description",
                    "caption",
                    "text",
                    "msg",
                    "title",
                    "content",
                    "message",
                    "body",
                    "question",
                    "topic",
                    "address",
                    "contactName",
                ],
            ) {
                if s.starts_with('{') && s.ends_with('}') {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                        return extract_content_text(&parsed);
                    }
                }
                return Some(s);
            }
            if let Some(params) = v.get("params") {
                if let Some(inner) = extract_content_text(params) {
                    return Some(inner);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_sticker_info(v: &Value) -> Option<(u64, u64, u32)> {
    match v {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return extract_sticker_info(&parsed);
                }
            }
            None
        }
        Value::Object(o) => {
            let parse_u64 = |val: &Value| -> Option<u64> {
                match val {
                    Value::Number(n) => n.as_u64(),
                    Value::String(s) => s.trim().parse::<u64>().ok(),
                    _ => None,
                }
            };
            let cat_id = o.get("catId").or_else(|| o.get("cateId")).and_then(parse_u64);
            let stk_id = o.get("stickerId").or_else(|| o.get("id")).and_then(parse_u64);
            let stk_type = o
                .get("type")
                .and_then(parse_u64)
                .map(|t| t as u32)
                .unwrap_or(7);
            if let Some(sid) = stk_id {
                if sid > 0
                    && (cat_id.is_some()
                        || o.contains_key("stickerId")
                        || o.contains_key("catId")
                        || o.contains_key("cateId"))
                {
                    return Some((sid, cat_id.unwrap_or(0), stk_type));
                }
            }
            None
        }
        _ => None,
    }
}

pub fn sticker_image_url(sid: u64, cate_id: u64) -> Option<String> {
    if sid == 0 {
        return None;
    }
    if cate_id == 0 && (1..=30).contains(&sid) {
        Some(format!(
            "https://stc-chat.zdn.vn/images/stickers/default/thumb/{sid}.png"
        ))
    } else {
        Some(format!(
            "https://api.zaloapp.com/api/emoticon/sticker/webpc?eid={sid}&size=130"
        ))
    }
}

fn extract_attachment(v: &Value, msg_type: &str) -> (Option<String>, Option<String>) {
    let obj = match v {
        Value::String(s) if s.trim().starts_with('{') => serde_json::from_str::<Value>(s.trim()).ok(),
        Value::Object(_) => Some(v.clone()),
        _ => None,
    };
    let Some(o) = obj else {
        return (None, None);
    };
    let params_val = o
        .get("params")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s.trim()).ok());
    let href = pick_str(
        &o,
        &["href", "oriUrl", "hdUrl", "normalUrl", "thumb", "stickerUrl"],
    )
    .or_else(|| {
        params_val
            .as_ref()
            .and_then(|p| pick_str(p, &["href", "oriUrl", "hdUrl", "normalUrl", "thumb"]))
    })
    .map(|u| normalize_media_url(&u))
    .filter(|u| !u.is_empty());

    let mut title = pick_str(&o, &["fileName", "name", "title"]);
    if title.is_none() {
        if let Some(ref p) = params_val {
            title = pick_str(p, &["fileName", "mediaTitle"]);
        }
    }
    if title.is_none() && (msg_type.contains("photo") || msg_type == "2") {
        title = Some("image.jpg".to_string());
    }
    (href, title)
}

pub fn try_reaction_code_to_emoji(code: &str) -> Option<&'static str> {
    match code.trim() {
        "" | "/-none" | "/-remove" | "none" => None,
        "/-strong" | "👍" => Some("👍"),
        "/-heart" | "❤" | "❤️" => Some("❤️"),
        ":>" | "😆" => Some("😆"),
        ":o" | "😮" => Some("😮"),
        ":-((" | "😢" => Some("😢"),
        ":-h" | "😠" => Some("😠"),
        _ => Some("❤️"),
    }
}

pub fn reaction_code_to_emoji(code: &str) -> &'static str {
    try_reaction_code_to_emoji(code).unwrap_or("❤️")
}

fn is_self_reaction_uid(uid: &str, my_uid: &str) -> bool {
    let trimmed = uid.trim();
    trimmed == "0" || (!my_uid.is_empty() && trimmed == my_uid)
}

fn reaction_entry_is_me(r: &Value, my_uid: &str) -> bool {
    if r.get("me").and_then(Value::as_bool) == Some(true)
        || r.get("isMe").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    if let Some(uid) = pick_str(r, &["uid", "uidFrom", "fromUid", "userId", "ownerId", "id"]) {
        if is_self_reaction_uid(&uid, my_uid) {
            return true;
        }
    }
    if let Some(uids) = r.get("uids").and_then(Value::as_array) {
        if uids.iter().any(|u| {
            u.as_str()
                .map(|s| is_self_reaction_uid(s, my_uid))
                .or_else(|| u.as_i64().map(|n| is_self_reaction_uid(&n.to_string(), my_uid)))
                .unwrap_or(false)
        }) {
            return true;
        }
    }
    if let Some(users) = r.get("rUsers").and_then(Value::as_array) {
        if users.iter().any(|u| {
            pick_str(u, &["uid", "uidFrom", "userId", "id"])
                .is_some_and(|uid| is_self_reaction_uid(&uid, my_uid))
        }) {
            return true;
        }
    }
    false
}

fn reaction_entry_count(r: &Value) -> u32 {
    if let Some(total) = r
        .get("total")
        .or_else(|| r.get("count"))
        .and_then(|v| v.as_u64())
    {
        return (total as u32).max(1);
    }
    if let Some(uids) = r.get("uids").and_then(Value::as_array) {
        if !uids.is_empty() {
            return uids.len() as u32;
        }
    }
    if let Some(users) = r.get("rUsers").and_then(Value::as_array) {
        if !users.is_empty() {
            return users.len() as u32;
        }
    }
    1
}

fn extract_reactions(it: &Value, my_uid: &str) -> Vec<ReactionSummary> {
    let mut map: std::collections::BTreeMap<&'static str, (u32, bool)> =
        std::collections::BTreeMap::new();
    for key in ["reactions", "reacts"] {
        if let Some(arr) = it.get(key).and_then(Value::as_array) {
            for r in arr {
                if r.get("rType").and_then(Value::as_i64) == Some(-1) {
                    continue;
                }
                let code = pick_str(r, &["rIcon", "emoji", "icon"]).unwrap_or_default();
                let Some(e) = try_reaction_code_to_emoji(&code) else {
                    continue;
                };
                let count = reaction_entry_count(r);
                let is_me = reaction_entry_is_me(r, my_uid);
                let entry = map.entry(e).or_insert((0, false));
                entry.0 += count;
                entry.1 |= is_me;
            }
        }
    }
    map.into_iter()
        .map(|(emoji, (count, me))| ReactionSummary {
            emoji: emoji.to_string(),
            count,
            me,
        })
        .collect()
}

pub fn extract_thread_id(
    entry: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
) -> Option<(String, bool)> {
    let is_group = in_group_bucket || is_group_flag(entry);
    if let Some(grid) = pick_str(entry, &["grid", "groupId"]).filter(|s| s != "0") {
        return Some((clean_group_id(&grid).to_string(), true));
    }
    if is_group {
        if let Some(gid) = pick_str(entry, &["idTo", "toid", "toId", "threadId", "conversId"])
            .filter(|s| s != "0")
        {
            return Some((clean_group_id(&gid).to_string(), true));
        }
    }
    let uid_from = pick_str(entry, &["uidFrom", "fromUid", "fromId", "senderId"]).unwrap_or_default();
    let id_to = pick_str(entry, &["idTo", "toid", "toId"]).unwrap_or_default();

    if !send2me_id.is_empty()
        && ((id_to == "0" && uid_from == "0") || id_to == send2me_id || uid_from == send2me_id)
    {
        return Some((send2me_id.to_string(), false));
    }
    if !uid_from.is_empty() && uid_from != "0" && uid_from != my_uid && (id_to.is_empty() || id_to == "0" || id_to == my_uid) {
        Some((uid_from, false))
    } else if !id_to.is_empty() && id_to != "0" && id_to != my_uid {
        Some((id_to, false))
    } else {
        pick_str(entry, &["threadId", "conversId", "userId", "friendId"])
            .filter(|s| s != "0")
            .map(|s| (s, is_group))
    }
}

pub fn parse_chat_message(
    it: &Value,
    thread_id_override: Option<&str>,
    my_uid: &str,
    send2me_id: &str,
    in_group_bucket: bool,
) -> Option<MessageDto> {
    let mut msg_type = normalize_msg_type(it);
    let raw_content = it
        .get("content")
        .or_else(|| it.get("message").and_then(|m| m.get("content")))
        .or_else(|| it.get("message"))
        .or_else(|| it.get("msg"));

    let mut sticker_meta = raw_content.and_then(extract_sticker_info);
    if sticker_meta.is_none()
        && matches!(msg_type.as_str(), "chat.sticker" | "sticker" | "5")
    {
        sticker_meta = extract_sticker_info(it);
    }

    let (mut attachment_url, mut attachment_name) = raw_content
        .map(|rv| extract_attachment(rv, &msg_type))
        .unwrap_or((None, None));

    let is_undo = matches!(msg_type.as_str(), "chat.undo" | "undo" | "31");
    if is_undo {
        sticker_meta = None;
    } else if let Some((sid, cid, _)) = sticker_meta {
        msg_type = "chat.sticker".to_string();
        if attachment_url.is_none() {
            attachment_url = sticker_image_url(sid, cid);
        }
        if attachment_name.is_none() {
            attachment_name = Some(format!("sticker_{cid}_{sid}.png"));
        }
    }

    let mut text = if is_undo {
        "[Tin nhắn đã thu hồi]".to_string()
    } else if sticker_meta.is_some() {
        "[Nhãn dán]".to_string()
    } else {
        raw_content.and_then(extract_content_text).unwrap_or_default()
    };

    if text.is_empty() {
        text = match msg_type.as_str() {
            "chat.photo" | "photo" | "2" => "[Hình ảnh]".into(),
            "chat.gif" | "gif" => "[Ảnh GIF]".into(),
            "chat.sticker" | "sticker" | "5" => "[Nhãn dán]".into(),
            "chat.voice" | "voice" | "3" => "[Tin nhắn thoại]".into(),
            "chat.video" | "video" | "chat.video.msg" | "4" => "[Video]".into(),
            "chat.file" | "file" | "19" | "share.file" => {
                if let Some(ref name) = attachment_name {
                    format!("[Tập tin] {name}")
                } else {
                    "[Tập tin]".into()
                }
            }
            "chat.recommended" => "[Danh thiếp]".into(),
            "chat.location" => "[Vị trí]".into(),
            "chat.poll" => "[Bình chọn]".into(),
            "chat.reminder" => "[Nhắc hẹn]".into(),
            "chat.ecard" => "[Thiệp chúc]".into(),
            "chat.link" | "link" => "[Liên kết]".into(),
            t if t.starts_with("group.") || t.contains("group.action") => "[Thông báo nhóm]".into(),
            _ if attachment_url.is_some() => "[Hình ảnh / Tệp đính kèm]".into(),
            _ => String::new(),
        };
    }

    let msg_id = pick_str(it, &["msgId", "globalMsgId", "cliMsgId", "id"])?;
    if text.is_empty() && attachment_url.is_none() {
        return None;
    }

    let thread_id = thread_id_override
        .map(str::to_string)
        .or_else(|| extract_thread_id(it, in_group_bucket, my_uid, send2me_id).map(|(t, _)| t))?;

    let uid_from =
        pick_str(it, &["uidFrom", "fromUid", "fromId", "senderId"]).unwrap_or_default();
    let clean_uid_from = uid_from.split('_').next().unwrap_or(uid_from.as_str());
    let is_self_flag = [it.get("isSelf"), it.get("is_self")]
        .into_iter()
        .flatten()
        .any(|v| v.as_bool() == Some(true) || v.as_i64() == Some(1) || v.as_str() == Some("1"));
    let is_outgoing = is_self_flag
        || clean_uid_from == "0"
        || clean_uid_from.is_empty()
        || (!my_uid.is_empty() && (uid_from == my_uid || clean_uid_from == my_uid))
        || (!send2me_id.is_empty() && clean_uid_from == send2me_id);
    let sender_name = if is_outgoing {
        "Bạn".to_string()
    } else {
        pick_str(it, &["dName", "displayName", "fromName", "senderName"])
            .unwrap_or_else(|| uid_from.clone())
    };
    let ts = it
        .get("ts")
        .or_else(|| it.get("cliMsgId"))
        .or_else(|| it.get("sendTime"))
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .map(normalize_ts_ms)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

    let quote = it.get("quote").filter(|q| q.is_object()).map(|q| {
        let snippet = q
            .get("msg")
            .or_else(|| q.get("message"))
            .or_else(|| q.get("content"))
            .and_then(extract_content_text)
            .or_else(|| pick_str(q, &["msg", "message", "content"]))
            .filter(|s| !(s.trim_start().starts_with('{') && s.trim_end().ends_with('}')))
            .unwrap_or_else(|| "[Tin nhắn đính kèm]".to_string());
        QuoteRef {
            msg_id: pick_str(q, &["globalMsgId", "msgId", "cliMsgId"]).unwrap_or_default(),
            sender_name: pick_str(q, &["fromD", "dName", "ownerId", "fromUid"]).unwrap_or_default(),
            snippet,
        }
    });

    Some(MessageDto {
        msg_id,
        thread_id,
        sender_id: if is_outgoing { my_uid.to_string() } else { uid_from },
        sender_name,
        text,
        msg_type,
        ts,
        is_outgoing,
        is_recalled: is_undo,
        is_pinned: false,
        quote,
        reactions: extract_reactions(it, my_uid),
        attachment_url,
        attachment_name,
    })
}

pub fn parse_ws_event(ev: &WsEvent, my_uid: &str, send2me_id: &str) -> Vec<WsParsedAction> {
    let mut actions = Vec::new();
    let data = ev.payload.get("data").unwrap_or(&ev.payload);

    if matches!(
        ev.cmd,
        CMD_PUSH_MSG_1_1
            | CMD_PUSH_MSG_GROUP
            | CMD_PULL_MSG_1_1
            | CMD_PULL_MSG_GROUP
            | CMD_PULL_MSG_513
            | CMD_PULL_MSG_514
            | CMD_PULL_MSG_515
            | CMD_PULL_MSG_517
            | CMD_PULL_MSG_518
            | CMD_PUSH_MISS_MSG
            | CMD_GET_MSG
    ) {
        let is_group_cmd = matches!(ev.cmd, CMD_PUSH_MSG_GROUP | CMD_PULL_MSG_GROUP);
        collect_messages_from_node(data, is_group_cmd, my_uid, send2me_id, &mut actions);
    } else if matches!(ev.cmd, CMD_CTRL_PUSH_CTRL | CMD_CTRL_ACTION | CMD_CTRL_PULL_CTRL) {
        collect_controls_from_node(data, my_uid, send2me_id, &mut actions);
    }
    actions
}

fn collect_messages_from_node(
    node: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    if let Some(arr) = node.get("msgs").and_then(Value::as_array) {
        for item in arr {
            push_single_item(item, in_group_bucket, my_uid, send2me_id, out);
        }
    }
    if let Some(arr) = node.get("groupMsgs").and_then(Value::as_array) {
        for item in arr {
            push_single_item(item, true, my_uid, send2me_id, out);
        }
    }
    if node.is_object()
        && (node.get("msgId").is_some()
            || node.get("cliMsgId").is_some()
            || node.get("content").is_some())
    {
        push_single_item(node, in_group_bucket, my_uid, send2me_id, out);
    }
    if let Some(arr) = node.as_array() {
        for item in arr {
            push_single_item(item, in_group_bucket, my_uid, send2me_id, out);
        }
    }
}

fn push_single_item(
    item: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    if let Some(msg) = parse_chat_message(item, None, my_uid, send2me_id, in_group_bucket) {
        if msg.is_recalled {
            out.push(WsParsedAction::Recalled {
                thread_id: msg.thread_id,
                msg_id: msg.msg_id,
            });
        } else {
            out.push(WsParsedAction::Message(Box::new(msg)));
        }
    }
}

fn collect_controls_from_node(
    node: &Value,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let mut items: Vec<&Value> = Vec::new();
    if let Some(arr) = node.get("controls").and_then(Value::as_array) {
        items.extend(arr.iter());
    } else if let Some(arr) = node.as_array() {
        items.extend(arr.iter());
    } else if node.is_object() {
        items.push(node);
    }

    for item in items {
        let act_type = pick_str(item, &["act_type", "type"]).unwrap_or_default();
        let data_node = item
            .get("content")
            .and_then(|c| c.get("data").or(Some(c)))
            .or_else(|| item.get("data"))
            .unwrap_or(item);
        if act_type == "undo_msg" || act_type == "undo" {
            if let (Some((tid, _)), Some(mid)) = (
                extract_thread_id(data_node, false, my_uid, send2me_id),
                pick_str(data_node, &["globalMsgId", "msgId", "cliMsgId"]),
            ) {
                out.push(WsParsedAction::Recalled {
                    thread_id: tid,
                    msg_id: mid,
                });
            }
        } else if act_type == "reaction" {
            if let (Some((tid, _)), Some(mid)) = (
                extract_thread_id(data_node, false, my_uid, send2me_id),
                pick_str(data_node, &["msgId", "globalMsgId", "gMsgID", "cliMsgId"]),
            ) {
                let r_icon = pick_str(data_node, &["rIcon", "icon", "emoji"]).unwrap_or_default();
                let r_type = data_node.get("rType").and_then(Value::as_i64);
                let removed =
                    r_type == Some(-1) || matches!(r_icon.trim(), "/-none" | "/-remove" | "none");
                let emoji = try_reaction_code_to_emoji(&r_icon).map(str::to_string);
                if removed || emoji.is_some() {
                    let uid_from = pick_str(data_node, &["uidFrom", "fromUid"]).unwrap_or_default();
                    out.push(WsParsedAction::Reaction {
                        thread_id: tid,
                        msg_id: mid,
                        emoji,
                        me: is_self_reaction_uid(&uid_from, my_uid),
                        removed,
                    });
                }
            }
        } else if matches!(
            act_type.as_str(),
            "seen" | "read" | "read_msg" | "mark_read" | "remove_unread_mark" | "unread" | "add_unread_mark" | "mark_unread"
        ) {
            if let Some((tid, _)) = extract_thread_id(data_node, false, my_uid, send2me_id) {
                let is_unread = matches!(act_type.as_str(), "unread" | "add_unread_mark" | "mark_unread");
                out.push(WsParsedAction::Read {
                    thread_id: tid,
                    is_unread,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sticker_message_with_image_url() {
        let val = json!({
            "msgId": "m1",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.sticker",
            "content": "{\"catId\":1002,\"id\":2005,\"type\":1}",
            "ts": 1_700_000_000
        });
        let msg = parse_chat_message(&val, None, "456", "", false).unwrap();
        assert_eq!(msg.msg_type, "chat.sticker");
        assert_eq!(msg.text, "[Nhãn dán]");
        assert_eq!(
            msg.attachment_url.as_deref(),
            Some("https://api.zaloapp.com/api/emoticon/sticker/webpc?eid=2005&size=130")
        );
    }

    #[test]
    fn normalizes_default_avatar_and_http_zalo_urls() {
        assert_eq!(normalize_avatar_url("https://s160-ava-talk.zadn.vn/default"), "");
        assert_eq!(
            normalize_avatar_url("http://s160-26-ava-talk.zadn.vn/14/abc.jpg"),
            "https://s160-26-ava-talk.zadn.vn/14/abc.jpg"
        );
        assert_eq!(normalize_media_url("javascript:alert(1)"), "");
        assert_eq!(normalize_media_url("http://evil.example/a.png"), "");
        assert_eq!(normalize_media_url("file:///C:/Windows/win.ini"), "");
    }

    #[test]
    fn extract_reactions_marks_me_and_handles_counts() {
        let val = json!({
            "msgId": "m2",
            "uidFrom": "999",
            "idTo": "456",
            "content": "hello",
            "ts": 1_700_000_001,
            "reactions": [
                { "rIcon": "/-strong", "uids": ["111", "456"] },
                { "rIcon": "/-heart", "uid": "222", "total": 2 },
                { "rIcon": "/-none", "rType": -1, "uid": "456" }
            ]
        });
        let msg = parse_chat_message(&val, None, "456", "", false).unwrap();
        assert_eq!(msg.reactions.len(), 2);
        let like = msg.reactions.iter().find(|r| r.emoji == "👍").unwrap();
        assert_eq!(like.count, 2);
        assert!(like.me);
        let heart = msg.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 2);
        assert!(!heart.me);
    }

    #[test]
    fn parses_ws_reaction_add_and_remove_controls() {
        let ev = WsEvent {
            cmd: CMD_CTRL_ACTION,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "controls": [
                    {
                        "act_type": "reaction",
                        "data": {
                            "uidFrom": "456",
                            "idTo": "999",
                            "msgId": "m10",
                            "rIcon": "/-none",
                            "rType": -1
                        }
                    },
                    {
                        "act_type": "reaction",
                        "data": {
                            "uidFrom": "999",
                            "idTo": "456",
                            "msgId": "m10",
                            "rIcon": ":>",
                            "rType": 0
                        }
                    }
                ]
            }),
        };
        let actions = parse_ws_event(&ev, "456", "");
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            WsParsedAction::Reaction {
                msg_id,
                emoji,
                me,
                removed,
                ..
            } => {
                assert_eq!(msg_id, "m10");
                assert!(emoji.is_none());
                assert!(*me);
                assert!(*removed);
            }
            other => panic!("unexpected action: {other:?}"),
        }
        match &actions[1] {
            WsParsedAction::Reaction {
                msg_id,
                emoji,
                me,
                removed,
                ..
            } => {
                assert_eq!(msg_id, "m10");
                assert_eq!(emoji.as_deref(), Some("😆"));
                assert!(!*me);
                assert!(!*removed);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn parses_ws_undo_and_read_unread_control_actions() {
        let ev = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "controls": [
                    {
                        "act_type": "undo_msg",
                        "data": { "uidFrom": "999", "idTo": "456", "globalMsgId": "gm-99" }
                    },
                    {
                        "act_type": "seen",
                        "data": { "uidFrom": "999", "idTo": "456" }
                    },
                    {
                        "act_type": "mark_unread",
                        "data": { "uidFrom": "456", "idTo": "888" }
                    }
                ]
            }),
        };
        let actions = parse_ws_event(&ev, "456", "");
        assert_eq!(actions.len(), 3);
        assert!(matches!(
            &actions[0],
            WsParsedAction::Recalled { thread_id, msg_id } if thread_id == "999" && msg_id == "gm-99"
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::Read { thread_id, is_unread } if thread_id == "999" && !*is_unread
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::Read { thread_id, is_unread } if thread_id == "888" && *is_unread
        ));
    }

    #[test]
    fn parses_cloud_self_messages_and_quote_snippets() {
        let val = json!({
            "msgId": "cm1",
            "uidFrom": "456",
            "idTo": "send2me_77",
            "content": "Ghi chu ca nhan",
            "ts": "1700000012",
            "quote": {
                "globalMsgId": "cm0",
                "fromD": "Tôi",
                "msg": "Tin goc"
            }
        });
        let msg = parse_chat_message(&val, None, "456", "send2me_77", false).unwrap();
        assert_eq!(msg.thread_id, "send2me_77");
        assert!(msg.is_outgoing);
        assert_eq!(msg.ts, 1_700_000_012_000);
        let q = msg.quote.unwrap();
        assert_eq!(q.msg_id, "cm0");
        assert_eq!(q.snippet, "Tin goc");
    }
}
