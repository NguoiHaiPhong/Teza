//! Parsing Zalo thread/message JSON payloads and WebSocket events into clean DTOs.

use serde_json::Value;

use crate::chat::{
    MessageDto, PollDto, PollOptionDto, QuoteRef, ReactionSummary, ThreadDto, ThreadKind,
};
use crate::socket::{
    WsEvent, CMD_CTRL_ACTION, CMD_CTRL_PULL_CTRL, CMD_CTRL_PUSH_CTRL, CMD_DELIVERED_1_1,
    CMD_DELIVERED_GROUP, CMD_GET_MSG, CMD_PULL_MSG_1_1, CMD_PULL_MSG_513, CMD_PULL_MSG_514,
    CMD_PULL_MSG_515, CMD_PULL_MSG_517, CMD_PULL_MSG_518, CMD_PULL_MSG_GROUP, CMD_PUSH_MISS_MSG,
    CMD_PUSH_MSG_1_1, CMD_PUSH_MSG_GROUP, CMD_SEEN_1_1, CMD_SEEN_GROUP,
};

#[derive(Debug, Clone)]
pub enum WsParsedAction {
    Message(Box<MessageDto>),
    Recalled { thread_id: String, msg_id: String },
    Read { thread_id: String, is_unread: bool },
    PeerSeen {
        thread_id: String,
        seen_ts: i64,
        is_delivered_only: bool,
    },
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
    let Ok(url) = reqwest::Url::parse(&candidate) else {
        return String::new();
    };
    if !url.username().is_empty() || url.password().is_some() {
        return String::new();
    }
    match url.scheme() {
        "https" => {
            if url.port().is_some_and(|p| p != 443) || url.host_str().is_none() {
                String::new()
            } else {
                url.to_string()
            }
        }
        "http" => crate::session::normalize_allowed_media_url(&candidate)
            .map(|u| u.to_string())
            .unwrap_or_default(),
        _ => String::new(),
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
        last_read_in_ts: 0,
        peer_read_ts: 0,
        muted_until_ts: 0,
        is_archived: false,
    })
}

fn unwrap_json_object(v: &Value) -> Option<Value> {
    match v {
        Value::Object(_) => Some(v.clone()),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                serde_json::from_str::<Value>(trimmed).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn value_as_bool_flag(v: &Value) -> bool {
    v.as_bool()
        .unwrap_or_else(|| v.as_i64().map(|n| n != 0).unwrap_or(false))
}

fn is_reminder_thumb_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/reminder_") || lower.contains("/reminder-")
}

fn extract_reminder_meta(
    raw: Option<&Value>,
    msg_type: &str,
) -> Option<(String, i64, i32, String, String)> {
    let lower_type = msg_type.to_ascii_lowercase();
    let explicit = lower_type.contains("reminder");
    let obj = raw.and_then(unwrap_json_object);
    let inner_params = obj
        .as_ref()
        .and_then(|o| o.get("params"))
        .and_then(unwrap_json_object);

    if !explicit {
        let action_is_reminder = [obj.as_ref(), inner_params.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|v| pick_str(v, &["action", "msgAction", "type"]))
            .any(|s| s.to_ascii_lowercase().contains("reminder"));
        let thumb_is_reminder = [obj.as_ref(), inner_params.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|v| pick_str(v, &["thumb", "href", "oriUrl"]))
            .any(|u| is_reminder_thumb_url(&u));
        let clock_card = obj.as_ref().is_some_and(|o| {
            pick_str(o, &["title"]).is_some_and(|t| t.trim_start().starts_with('⏰'))
                && (o.get("description").is_some() || o.get("thumb").is_some())
        });
        let looks_like_reminder = action_is_reminder
            || thumb_is_reminder
            || clock_card
            || obj.as_ref().is_some_and(|o| {
                o.get("reminderId").is_some()
                    || o.get("repeatMode").is_some()
                    || ((o.get("startTime").is_some() || o.get("remindTime").is_some())
                        && (o.get("title").is_some() || inner_params.is_some()))
            })
            || inner_params.as_ref().is_some_and(|p| {
                p.get("reminderId").is_some()
                    || ((p.get("startTime").is_some() || p.get("remindTime").is_some())
                        && p.get("title").is_some())
            });
        if !looks_like_reminder {
            return None;
        }
    }

    let find_str = |keys: &[&str]| -> Option<String> {
        inner_params
            .as_ref()
            .and_then(|p| pick_str(p, keys))
            .or_else(|| obj.as_ref().and_then(|o| pick_str(o, keys)))
    };
    let find_i64 = |keys: &[&str]| -> Option<i64> {
        for key in keys {
            for src in [inner_params.as_ref(), obj.as_ref()].into_iter().flatten() {
                if let Some(val) = src.get(*key) {
                    if let Some(n) = val.as_i64().or_else(|| val.as_str()?.trim().parse().ok()) {
                        return Some(n);
                    }
                }
            }
        }
        None
    };

    let raw_title = find_str(&["title", "topic", "content", "msg", "description"])
        .filter(|s| !(s.trim_start().starts_with('{') && s.trim_end().ends_with('}')))
        .unwrap_or_else(|| "Nhắc hẹn".to_string());
    let cleaned_title = raw_title
        .trim()
        .trim_start_matches('⏰')
        .trim()
        .to_string();
    let title = if cleaned_title.is_empty() {
        "Nhắc hẹn".to_string()
    } else {
        cleaned_title
    };
    let schedule_text = find_str(&["description", "desc", "subTitle"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != &title && s != &raw_title)
        .unwrap_or_default();
    let start_time = find_i64(&["startTime", "remindTime", "time", "triggerTime"])
        .map(normalize_ts_ms)
        .unwrap_or(0);
    let repeat = find_i64(&["repeat", "repeatMode"]).unwrap_or(0) as i32;
    let reminder_id = find_str(&["reminderId", "topicId", "id"]).unwrap_or_default();
    Some((title, start_time, repeat, reminder_id, schedule_text))
}

pub fn parse_poll_value(v: &Value, fallback_poll_id: i64) -> Option<PollDto> {
    let inner_params = v.get("params").and_then(unwrap_json_object);
    let src = inner_params.as_ref().unwrap_or(v);
    let read_i64 = |keys: &[&str]| -> Option<i64> {
        for key in keys {
            for node in [Some(src), Some(v)].into_iter().flatten() {
                if let Some(val) = node.get(*key) {
                    if let Some(n) = val.as_i64().or_else(|| val.as_str()?.trim().parse().ok()) {
                        return Some(n);
                    }
                }
            }
        }
        None
    };
    let read_bool = |keys: &[&str]| -> bool {
        for key in keys {
            for node in [Some(src), Some(v)].into_iter().flatten() {
                if let Some(val) = node.get(*key) {
                    return value_as_bool_flag(val);
                }
            }
        }
        false
    };

    let poll_id = read_i64(&["poll_id", "pollId", "id"]).unwrap_or(fallback_poll_id);
    let question = pick_str(src, &["question", "title", "content"])
        .or_else(|| pick_str(v, &["question", "title", "content"]))
        .unwrap_or_else(|| "Bình chọn".to_string());

    let raw_options = src
        .get("options")
        .or_else(|| v.get("options"))
        .and_then(|o| match o {
            Value::Array(arr) => Some(arr.clone()),
            Value::String(s) => serde_json::from_str::<Vec<Value>>(s.trim()).ok(),
            _ => None,
        })
        .unwrap_or_default();

    let mut options = Vec::new();
    for (idx, item) in raw_options.into_iter().enumerate() {
        match item {
            Value::String(label) => {
                let clean = label.trim();
                if !clean.is_empty() {
                    options.push(PollOptionDto {
                        id: (idx + 1) as i64,
                        content: clean.to_string(),
                        votes: 0,
                        voted: false,
                    });
                }
            }
            Value::Object(ref o) => {
                let id = o
                    .get("option_id")
                    .or_else(|| o.get("optionId"))
                    .or_else(|| o.get("id"))
                    .and_then(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok()))
                    .unwrap_or((idx + 1) as i64);
                let content = pick_str(&item, &["content", "title", "label", "name"])
                    .unwrap_or_else(|| format!("Lựa chọn {}", idx + 1));
                let votes = o
                    .get("total_vote")
                    .or_else(|| o.get("totalVote"))
                    .or_else(|| o.get("votes"))
                    .or_else(|| o.get("voteCount"))
                    .and_then(|x| {
                        x.as_u64()
                            .or_else(|| x.as_str()?.parse().ok())
                            .or_else(|| x.as_array().map(|a| a.len() as u64))
                    })
                    .or_else(|| {
                        o.get("voters")
                            .and_then(Value::as_array)
                            .map(|a| a.len() as u64)
                    })
                    .unwrap_or(0) as u32;
                let voted = o
                    .get("voted")
                    .or_else(|| o.get("is_voted"))
                    .map(value_as_bool_flag)
                    .unwrap_or(false);
                options.push(PollOptionDto {
                    id,
                    content,
                    votes,
                    voted,
                });
            }
            _ => {}
        }
    }

    if poll_id == 0 && options.is_empty() && question == "Bình chọn" {
        return None;
    }

    let expired_time = read_i64(&["expired_time", "expiredTime", "expiried_time"])
        .map(normalize_ts_ms)
        .unwrap_or(0);
    let closed = read_bool(&["closed", "is_closed", "isClosed"]);

    Some(PollDto {
        poll_id,
        question,
        options,
        allow_multi: read_bool(&["allow_multi_choices", "allowMultiChoices", "allow_multi"]),
        allow_add_option: read_bool(&[
            "allow_add_new_option",
            "allowAddNewOption",
            "allow_add_option",
        ]),
        is_anonymous: read_bool(&["is_anonymous", "isAnonymous"]),
        expired_time,
        closed,
    })
}

fn extract_poll_meta(raw: Option<&Value>, msg_type: &str, fallback_msg_id: &str) -> Option<PollDto> {
    let lower_type = msg_type.to_ascii_lowercase();
    let explicit = matches!(lower_type.as_str(), "chat.poll" | "group.poll" | "poll");
    let obj = raw.and_then(unwrap_json_object);
    if !explicit {
        let looks_like_poll = obj.as_ref().is_some_and(|o| {
            o.get("poll_id").is_some()
                || o.get("pollId").is_some()
                || (o.get("question").is_some() && o.get("options").is_some())
        });
        if !looks_like_poll {
            return None;
        }
    }
    let fallback_id = fallback_msg_id.trim().parse::<i64>().unwrap_or(0);
    obj.as_ref().and_then(|o| parse_poll_value(o, fallback_id)).or_else(|| {
        if explicit {
            Some(PollDto {
                poll_id: fallback_id,
                question: "Bình chọn".to_string(),
                options: Vec::new(),
                allow_multi: false,
                allow_add_option: false,
                is_anonymous: false,
                expired_time: 0,
                closed: false,
            })
        } else {
            None
        }
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

fn is_link_or_card_msg_type(msg_type: &str) -> bool {
    matches!(
        msg_type.to_ascii_lowercase().as_str(),
        "chat.link"
            | "link"
            | "chat.location"
            | "chat.location.new"
            | "chat.recommended"
            | "chat.ecard"
    )
}

fn extract_link_href(raw: Option<&Value>) -> Option<String> {
    let obj = raw.and_then(unwrap_json_object)?;
    let params_val = obj.get("params").and_then(unwrap_json_object);
    let candidate = pick_str(&obj, &["href", "url"])
        .or_else(|| params_val.as_ref().and_then(|p| pick_str(p, &["href", "url"])))?;
    let cleaned = candidate.trim().replace(r"\/", "/");
    let parsed = reqwest::Url::parse(&cleaned).ok()?;
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
        Some(cleaned)
    } else {
        None
    }
}

fn extract_attachment(v: &Value, msg_type: &str) -> (Option<String>, Option<String>) {
    let Some(o) = unwrap_json_object(v) else {
        return (None, None);
    };
    let params_val = o.get("params").and_then(unwrap_json_object);
    let is_link_or_card = is_link_or_card_msg_type(msg_type);
    let href = if is_link_or_card {
        // Never store external non-CDN hrefs in `attachment_url`; keep only verified CDN thumbnails.
        ["thumb", "thumbUrl"].iter().find_map(|key| {
            pick_str(&o, &[*key])
                .or_else(|| params_val.as_ref().and_then(|p| pick_str(p, &[*key])))
                .and_then(|u| crate::session::normalize_allowed_media_url(&u).ok())
                .map(|u| u.to_string())
        })
    } else {
        const URL_PRIORITY: &[&str] = &[
            "oriUrl",
            "hdUrl",
            "fileUrl",
            "href",
            "normalUrl",
            "url",
            "thumb",
            "stickerUrl",
        ];
        URL_PRIORITY.iter().find_map(|key| {
            pick_str(&o, &[*key])
                .or_else(|| params_val.as_ref().and_then(|p| pick_str(p, &[*key])))
                .map(|u| normalize_media_url(&u))
                .filter(|u| !u.is_empty())
        })
    };

    if is_link_or_card && href.is_none() {
        return (None, None);
    }

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

    let raw_msg_id = pick_str(it, &["msgId", "globalMsgId", "cliMsgId", "id"]);
    let reminder_meta = extract_reminder_meta(raw_content, &msg_type);
    let poll_meta = if reminder_meta.is_none() {
        extract_poll_meta(raw_content, &msg_type, raw_msg_id.as_deref().unwrap_or(""))
    } else {
        None
    };

    let (mut attachment_url, mut attachment_name) = if reminder_meta.is_some() || poll_meta.is_some() {
        (None, None)
    } else {
        raw_content
            .map(|rv| extract_attachment(rv, &msg_type))
            .unwrap_or((None, None))
    };

    let is_undo = matches!(msg_type.as_str(), "chat.undo" | "undo" | "31");
    if is_undo {
        sticker_meta = None;
    } else if let Some((ref title, start_time, repeat, ref rid, ref schedule_text)) = reminder_meta {
        msg_type = "chat.reminder".to_string();
        sticker_meta = None;
        attachment_url = None;
        let mut meta_obj = serde_json::json!({
            "title": title,
            "startTime": start_time,
            "repeat": repeat,
            "reminderId": rid,
        });
        if !schedule_text.is_empty() {
            meta_obj["scheduleText"] = Value::String(schedule_text.clone());
        }
        attachment_name = Some(format!("teza-reminder:{meta_obj}"));
    } else if let Some(ref poll) = poll_meta {
        msg_type = "chat.poll".to_string();
        sticker_meta = None;
        attachment_url = None;
        if let Ok(json_str) = serde_json::to_string(poll) {
            attachment_name = Some(format!("teza-poll:{json_str}"));
        }
    } else if let Some((sid, cid, _)) = sticker_meta {
        msg_type = "chat.sticker".to_string();
        if attachment_url.is_none() {
            attachment_url = sticker_image_url(sid, cid);
        }
        if attachment_name.is_none() {
            attachment_name = Some(format!("sticker_{cid}_{sid}.png"));
        }
    }

    let is_file_type = matches!(msg_type.as_str(), "chat.file" | "file" | "19" | "share.file");
    let mut text = if is_undo {
        "[Tin nhắn đã thu hồi]".to_string()
    } else if let Some((ref title, _, _, _, _)) = reminder_meta {
        if title == "Nhắc hẹn" {
            "[Nhắc hẹn]".to_string()
        } else {
            format!("Nhắc hẹn: {title}")
        }
    } else if let Some(ref poll) = poll_meta {
        if poll.question == "Bình chọn" {
            "[Bình chọn]".to_string()
        } else {
            format!("Bình chọn: {}", poll.question)
        }
    } else if sticker_meta.is_some() {
        "[Nhãn dán]".to_string()
    } else if is_file_type {
        raw_content
            .and_then(|c| {
                let caption = ["description", "caption", "desc"]
                    .iter()
                    .find_map(|k| c.get(*k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()))
                    .map(str::to_string)
                    .or_else(|| extract_content_text(c));
                caption.filter(|s| {
                    let trimmed = s.trim();
                    !trimmed.is_empty()
                        && attachment_name
                            .as_deref()
                            .map(|name| !trimmed.eq_ignore_ascii_case(name.trim()))
                            .unwrap_or(true)
                })
            })
            .unwrap_or_default()
    } else {
        raw_content.and_then(extract_content_text).unwrap_or_default()
    };

    if matches!(
        msg_type.to_ascii_lowercase().as_str(),
        "chat.link" | "link" | "chat.location" | "chat.location.new"
    ) {
        if let Some(link_url) = extract_link_href(raw_content) {
            if text.is_empty() {
                text = link_url;
            } else if !text.contains(&link_url) {
                text = format!("{text}\n{link_url}");
            }
        }
    }

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
    } else if matches!(
        ev.cmd,
        CMD_DELIVERED_1_1 | CMD_SEEN_1_1 | CMD_DELIVERED_GROUP | CMD_SEEN_GROUP
    ) {
        let is_group_cmd = matches!(ev.cmd, CMD_DELIVERED_GROUP | CMD_SEEN_GROUP);
        let default_seen = matches!(ev.cmd, CMD_SEEN_1_1 | CMD_SEEN_GROUP) || ev.sub_cmd == 1;
        collect_receipts_from_node(
            data,
            is_group_cmd,
            default_seen,
            my_uid,
            send2me_id,
            &mut actions,
        );
    }
    actions
}

fn parse_receipt_item(
    item: &Value,
    is_group_cmd: bool,
    is_seen: bool,
    my_uid: &str,
    send2me_id: &str,
) -> Option<WsParsedAction> {
    if let Some(uids) = item.get("seenUids").and_then(Value::as_array) {
        if !uids.is_empty() {
            let has_peer = uids.iter().any(|v| {
                let s = match v {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                };
                !s.is_empty() && s != "0" && (my_uid.is_empty() || s != my_uid)
            });
            if !has_peer {
                return None;
            }
        }
    } else {
        let uid_from = pick_str(item, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
        let id_to = pick_str(item, &["idTo", "toId"]).unwrap_or_default();
        if !my_uid.is_empty() && uid_from == my_uid && id_to == my_uid {
            return None;
        }
    }

    let is_group = is_group_cmd
        || item.get("grid").is_some()
        || item.get("groupId").is_some()
        || is_group_flag(item);
    let thread_id = if is_group {
        let raw_gid = pick_str(item, &["grid", "groupId", "idTo", "threadId"])?;
        format!("group_{}", clean_group_id(&raw_gid))
    } else {
        ["idTo", "uidFrom", "fromUid", "userId"]
            .iter()
            .filter_map(|k| pick_str(item, &[k]))
            .find(|s| !s.is_empty() && s != "0" && (my_uid.is_empty() || s != my_uid))
            .or_else(|| extract_thread_id(item, false, my_uid, send2me_id).map(|(t, _)| t))?
    };

    let parsed_ts = ["seenTs", "actionTs", "ts", "msgTs", "sendTime"]
        .iter()
        .find_map(|k| {
            item.get(*k)
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
        })
        .or_else(|| {
            item.get("cliMsgId")
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
                .filter(|n| *n >= 1_000_000_000)
        })
        .map(normalize_ts_ms)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

    Some(WsParsedAction::PeerSeen {
        thread_id,
        seen_ts: parsed_ts,
        is_delivered_only: !is_seen,
    })
}

fn collect_receipts_from_node(
    node: &Value,
    is_group_cmd: bool,
    default_seen: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let buckets = [
        ("seenMsgs", true),
        ("seen_msgs", true),
        ("deliverMsgs", false),
        ("deliver_msgs", false),
    ];
    let mut matched_bucket = false;
    for (key, is_seen) in buckets {
        if let Some(arr) = node.get(key).and_then(Value::as_array) {
            matched_bucket = true;
            for item in arr {
                if let Some(act) = parse_receipt_item(item, is_group_cmd, is_seen, my_uid, send2me_id)
                {
                    out.push(act);
                }
            }
        }
    }
    if matched_bucket {
        return;
    }

    if let Some(arr) = node.as_array() {
        for item in arr {
            if let Some(act) =
                parse_receipt_item(item, is_group_cmd, default_seen, my_uid, send2me_id)
            {
                out.push(act);
            }
        }
    } else if node.is_object() {
        let is_seen = default_seen || node.get("seenUids").is_some();
        if let Some(act) = parse_receipt_item(node, is_group_cmd, is_seen, my_uid, send2me_id) {
            out.push(act);
        }
    }
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
        } else if matches!(act_type.as_str(), "seen_msg" | "read_receipt") {
            let peer_uid = pick_str(data_node, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
            let is_peer = !peer_uid.is_empty()
                && peer_uid != "0"
                && (my_uid.is_empty() || peer_uid != my_uid);
            if is_peer {
                if let Some(act) =
                    parse_receipt_item(data_node, is_group_flag(data_node), true, my_uid, send2me_id)
                {
                    out.push(act);
                }
            }
        } else if matches!(
            act_type.as_str(),
            "seen" | "read" | "read_msg" | "mark_read" | "remove_unread_mark" | "unread" | "add_unread_mark" | "mark_unread"
        ) {
            let has_explicit_receipt = act_type == "seen"
                && (data_node.get("seenUids").is_some()
                    || data_node.get("seenTs").is_some()
                    || data_node.get("msgId").is_some()
                    || data_node.get("globalMsgId").is_some()
                    || data_node.get("realMsgId").is_some());
            let peer_uid = pick_str(data_node, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
            let is_peer = !peer_uid.is_empty()
                && peer_uid != "0"
                && (my_uid.is_empty() || peer_uid != my_uid);
            if has_explicit_receipt && is_peer {
                if let Some(act) =
                    parse_receipt_item(data_node, is_group_flag(data_node), true, my_uid, send2me_id)
                {
                    out.push(act);
                }
            } else if let Some((tid, _)) = extract_thread_id(data_node, false, my_uid, send2me_id) {
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

    #[test]
    fn parses_file_message_without_duplicating_filename_and_preserves_caption() {
        let no_caption = json!({
            "msgId": "f1",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.file",
            "content": {
                "title": "bao_cao.pdf",
                "href": "https://f10-zpc.zdn.vn/bao_cao.pdf",
                "params": "{\"checksum\":\"abc\"}"
            },
            "ts": 1_700_000_015_000_i64
        });
        let m1 = parse_chat_message(&no_caption, None, "999", "cloud-1", false).unwrap();
        assert_eq!(m1.attachment_name.as_deref(), Some("bao_cao.pdf"));
        assert_eq!(m1.text, "[Tập tin] bao_cao.pdf");

        let with_caption = json!({
            "msgId": "f2",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.file",
            "content": {
                "title": "bao_cao.pdf",
                "description": "Ban cap nhat moi nhat",
                "href": "https://f10-zpc.zdn.vn/bao_cao.pdf"
            },
            "ts": 1_700_000_016_000_i64
        });
        let m2 = parse_chat_message(&with_caption, None, "999", "cloud-1", false).unwrap();
        assert_eq!(m2.attachment_name.as_deref(), Some("bao_cao.pdf"));
        assert_eq!(m2.text, "Ban cap nhat moi nhat");
    }

    #[test]
    fn extracts_hd_photo_url_from_params_instead_of_root_thumb() {
        let photo = json!({
            "msgId": "img-hd-1",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.photo",
            "content": {
                "thumb": "https://f.zdn.vn/small_thumb.jpg",
                "href": "https://f.zdn.vn/normal_compressed.jpg",
                "params": "{\"oriUrl\":\"https://f.zdn.vn/original_hd.jpg\",\"hdUrl\":\"https://f.zdn.vn/hd.jpg\"}"
            },
            "ts": 1_700_000_018_000_i64
        });
        let parsed = parse_chat_message(&photo, None, "999", "cloud-1", false).unwrap();
        assert_eq!(
            parsed.attachment_url.as_deref(),
            Some("https://f.zdn.vn/original_hd.jpg")
        );

        let only_root_thumb_and_params_hd = json!({
            "msgId": "img-hd-2",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.photo",
            "content": {
                "thumb": "https://f.zdn.vn/small_thumb.jpg",
                "params": "{\"hdUrl\":\"https://f.zdn.vn/hd_from_params.jpg\"}"
            },
            "ts": 1_700_000_019_000_i64
        });
        let parsed2 =
            parse_chat_message(&only_root_thumb_and_params_hd, None, "999", "cloud-1", false)
                .unwrap();
        assert_eq!(
            parsed2.attachment_url.as_deref(),
            Some("https://f.zdn.vn/hd_from_params.jpg")
        );
    }

    #[test]
    fn parses_ws_seen_and_delivered_receipts() {
        let ev_seen_11 = WsEvent {
            cmd: CMD_SEEN_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "seenMsgs": [
                        { "idTo": "999", "uidFrom": "999", "seenTs": 1_700_000_050_i64 },
                        { "uidFrom": "456", "idTo": "456", "seenTs": 1_700_000_060_i64 }
                    ]
                }
            }),
        };
        let a1 = parse_ws_event(&ev_seen_11, "456", "");
        assert_eq!(a1.len(), 1);
        assert!(matches!(
            &a1[0],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "999" && *seen_ts == 1_700_000_050_000 && !*is_delivered_only
        ));

        let ev_seen_grp = WsEvent {
            cmd: CMD_SEEN_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "seenMsgs": [
                        { "grid": "888111", "seenUids": ["456", "777"], "ts": 1_700_000_090_000_i64 },
                        { "grid": "888222", "seenUids": ["456"], "ts": 1_700_000_091_000_i64 }
                    ]
                }
            }),
        };
        let a2 = parse_ws_event(&ev_seen_grp, "456", "");
        assert_eq!(a2.len(), 1);
        assert!(matches!(
            &a2[0],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "group_888111" && *seen_ts == 1_700_000_090_000 && !*is_delivered_only
        ));

        let ev_deliv = WsEvent {
            cmd: CMD_DELIVERED_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "deliverMsgs": [
                        { "idTo": "555", "ts": 1_700_000_120_000_i64 }
                    ]
                }
            }),
        };
        let a3 = parse_ws_event(&ev_deliv, "456", "");
        assert_eq!(a3.len(), 1);
        assert!(matches!(
            &a3[0],
            WsParsedAction::PeerSeen { thread_id, is_delivered_only, .. }
                if thread_id == "555" && *is_delivered_only
        ));
    }

    #[test]
    fn parses_reminder_message_without_thumb_image_and_extracts_metadata() {
        let rem = json!({
            "msgId": "7001",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.reminder",
            "ts": 1_700_000_000_000_i64,
            "content": {
                "title": "Họp giao ban tuần",
                "thumb": "https://stc-chat.zdn.vn/images/reminder_calendar_bg.png",
                "href": "https://chat.zalo.me",
                "params": "{\"reminderId\":\"r-42\",\"title\":\"Họp giao ban tuần\",\"startTime\":1700003600,\"repeat\":2}"
            }
        });
        let parsed = parse_chat_message(&rem, None, "999", "", false).unwrap();
        assert_eq!(parsed.msg_type, "chat.reminder");
        assert!(parsed.attachment_url.is_none());
        assert_eq!(parsed.text, "Nhắc hẹn: Họp giao ban tuần");
        let meta = parsed.attachment_name.as_deref().unwrap();
        assert!(meta.starts_with("teza-reminder:"));
        assert!(meta.contains("1700003600000"));
        assert!(meta.contains("r-42"));
    }

    #[test]
    fn parses_ecard_reminder_notification_with_clock_title_and_thumb_url() {
        let rem = json!({
            "msgId": "8514082898696",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.ecard",
            "ts": 1_763_712_000_000_i64,
            "content": {
                "title": "⏰ uống nước để lấy năng lượng...",
                "description": "Thứ Bảy, 21 tháng 11 lúc 15:00",
                "thumb": "https://res-zalo.zadn.vn/upload/media/2018/8/3/reminder_2_1533262255293_116971.png",
                "href": "https://chat.zalo.me"
            }
        });
        let parsed = parse_chat_message(&rem, None, "999", "", false).unwrap();
        assert_eq!(parsed.msg_type, "chat.reminder");
        assert!(parsed.attachment_url.is_none());
        assert_eq!(parsed.text, "Nhắc hẹn: uống nước để lấy năng lượng...");
        let meta = parsed.attachment_name.as_deref().unwrap();
        assert!(meta.starts_with("teza-reminder:"));
        assert!(meta.contains("uống nước để lấy năng lượng..."));
        assert!(meta.contains("Thứ Bảy, 21 tháng 11 lúc 15:00"));
    }

    #[test]
    fn parses_poll_message_metadata() {
        let poll = json!({
            "msgId": "7002",
            "uidFrom": "100",
            "grid": "888111",
            "msgType": "group.poll",
            "ts": 1_700_000_000_000_i64,
            "content": {
                "poll_id": 98765_i64,
                "question": "Trưa nay ăn gì?",
                "allow_multi_choices": true,
                "is_anonymous": false,
                "options": [
                    { "option_id": 1, "content": "Phở bò", "total_vote": 3, "voted": true },
                    { "option_id": 2, "content": "Bún chả", "total_vote": 1, "voted": false }
                ]
            }
        });
        let parsed = parse_chat_message(&poll, None, "999", "", true).unwrap();
        assert_eq!(parsed.msg_type, "chat.poll");
        assert!(parsed.attachment_url.is_none());
        assert_eq!(parsed.text, "Bình chọn: Trưa nay ăn gì?");
        let raw_meta = parsed.attachment_name.as_deref().unwrap();
        let json_str = raw_meta.strip_prefix("teza-poll:").unwrap();
        let dto: PollDto = serde_json::from_str(json_str).unwrap();
        assert_eq!(dto.poll_id, 98765);
        assert!(dto.allow_multi);
        assert_eq!(dto.options.len(), 2);
        assert_eq!(dto.options[0].votes, 3);
        assert!(dto.options[0].voted);
    }

    #[test]
    fn chat_link_never_stores_external_href_as_attachment_url() {
        let link_no_cdn_thumb = json!({
            "msgId": "link-1",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.link",
            "ts": 1_700_000_010_000_i64,
            "content": {
                "title": "Bài viết mới",
                "description": "Tóm tắt bài viết",
                "href": "https://vnexpress.net/bai-viet-123.html",
                "thumb": "https://i1-vnexpress.vnecdn.net/og.jpg"
            }
        });
        let parsed = parse_chat_message(&link_no_cdn_thumb, None, "999", "", false).unwrap();
        assert_eq!(parsed.msg_type, "chat.link");
        assert!(parsed.attachment_url.is_none());
        assert!(parsed.attachment_name.is_none());
        assert!(parsed.text.contains("Tóm tắt bài viết"));
        assert!(parsed.text.contains("https://vnexpress.net/bai-viet-123.html"));

        // If Zalo cached a thumbnail on an official CDN, keep ONLY the CDN thumb (not external href)
        let link_with_cdn_thumb = json!({
            "msgId": "link-2",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.link",
            "ts": 1_700_000_011_000_i64,
            "content": {
                "title": "Video hay",
                "href": "https://youtube.com/watch?v=abc",
                "thumb": "http://f10-zpc.zdn.vn/link-thumb.jpg"
            }
        });
        let parsed2 = parse_chat_message(&link_with_cdn_thumb, None, "999", "", false).unwrap();
        assert_eq!(
            parsed2.attachment_url.as_deref(),
            Some("https://f10-zpc.zdn.vn/link-thumb.jpg")
        );
        assert!(parsed2.text.contains("https://youtube.com/watch?v=abc"));

        // External location / ecard / recommended hrefs are also excluded from attachment_url
        let loc = json!({
            "msgId": "loc-1",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.location",
            "ts": 1_700_000_012_000_i64,
            "content": {
                "address": "Hà Nội",
                "href": "https://maps.google.com/?q=21.0285,105.8542",
                "thumb": "https://maps.googleapis.com/maps/api/staticmap?center=21,105"
            }
        });
        let parsed_loc = parse_chat_message(&loc, None, "999", "", false).unwrap();
        assert!(parsed_loc.attachment_url.is_none());
        assert!(parsed_loc.text.contains("https://maps.google.com/?q=21.0285,105.8542"));
    }

    #[test]
    fn normalize_media_url_and_object_params_file_url_extraction() {
        assert_eq!(
            normalize_media_url(r"HTTP:\/\/files.zalo.cloud\/a\/b.pdf"),
            "https://files.zalo.cloud/a/b.pdf"
        );
        assert_eq!(
            normalize_media_url("http://dl.zbox.vn:80/report.docx"),
            "https://dl.zbox.vn/report.docx"
        );
        assert_eq!(normalize_media_url("http://bucket.vngcloud.vn/x.pdf"), "");

        let file_msg = json!({
            "msgId": "f-obj-1",
            "uidFrom": "100",
            "idTo": "200",
            "msgType": "chat.file",
            "ts": 1_700_000_020_000_i64,
            "content": {
                "title": "bao_cao.xlsx",
                "params": {
                    "fileUrl": "http://files.zalo.cloud/dl/bao_cao.xlsx",
                    "fileName": "bao_cao.xlsx"
                }
            }
        });
        let parsed = parse_chat_message(&file_msg, None, "999", "", false).unwrap();
        assert_eq!(
            parsed.attachment_url.as_deref(),
            Some("https://files.zalo.cloud/dl/bao_cao.xlsx")
        );
        assert_eq!(parsed.attachment_name.as_deref(), Some("bao_cao.xlsx"));
    }
}
