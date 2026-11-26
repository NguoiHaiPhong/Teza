//! Parsing Zalo thread/message JSON payloads and WebSocket events into clean DTOs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::{
    MessageDto, PollDto, PollOptionDto, QuoteRef, ReactionSummary, ThreadDto, ThreadKind,
};
use crate::socket::{
    WsEvent, CMD_AUTHEN, CMD_CLEAR_UNREAD_1_1, CMD_CLEAR_UNREAD_GROUP, CMD_CTRL_ACTION,
    CMD_CTRL_PULL_610, CMD_CTRL_PULL_CTRL, CMD_CTRL_PULL_VOIP, CMD_CTRL_PUSH_CTRL,
    CMD_DELIVERED_1_1, CMD_DELIVERED_GROUP, CMD_E2EE_DELIVER_SEEN_1_1,
    CMD_E2EE_DELIVER_SEEN_GROUP, CMD_GET_MSG, CMD_PING, CMD_PULL_ACK_GROUP, CMD_PULL_MSG_1_1,
    CMD_PULL_MSG_513, CMD_PULL_MSG_514, CMD_PULL_MSG_515, CMD_PULL_MSG_517, CMD_PULL_MSG_518,
    CMD_PULL_MSG_E2EE_1_1, CMD_PULL_MSG_E2EE_GROUP, CMD_PULL_MSG_GROUP, CMD_PUSH_MISS_MSG,
    CMD_PUSH_MSG_1_1, CMD_PUSH_MSG_E2EE_1_1, CMD_PUSH_MSG_E2EE_GROUP, CMD_PUSH_MSG_GROUP,
    CMD_SEEN_1_1, CMD_SEEN_GROUP,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoipControlEvent {
    pub act: String,
    pub data: Value,
}

impl VoipControlEvent {
    pub fn call_id(&self) -> i64 {
        self.data
            .get("callId")
            .or_else(|| self.data.get("id"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .unwrap_or(0)
    }

    pub fn status(&self) -> Option<i64> {
        self.data.get("status").and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
    }

    /// Parse the nested JSON string (or object) carried inside `data.params`.
    pub fn media_params(&self) -> Value {
        match self.data.get("params") {
            Some(Value::Object(_)) => self.data.get("params").cloned().unwrap_or(Value::Null),
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.starts_with('{') || trimmed.starts_with('[') {
                    serde_json::from_str::<Value>(trimmed).unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        }
    }
}

/// Zalo WebSocket command IDs that carry real-time (`612`) or pulled (`610`/`611`) message reactions.
pub const CMD_REACTION_PULL_1_1: u16 = 610;
pub const CMD_REACTION_PULL_GROUP: u16 = 611;
pub const CMD_REACTION_PUSH: u16 = 612;

/// Returns `true` for Zalo WebSocket command IDs carrying live or queued reaction frames.
pub fn is_reaction_ws_cmd(cmd: u16) -> bool {
    matches!(
        cmd,
        CMD_REACTION_PULL_1_1 | CMD_REACTION_PULL_GROUP | CMD_REACTION_PUSH
    )
}

#[derive(Debug, Clone)]
pub enum WsParsedAction {
    Message(Box<MessageDto>),
    Recalled { thread_id: String, msg_id: String },
    Read {
        thread_id: String,
        is_unread: bool,
        last_msg_id: Option<String>,
    },
    PeerSeen {
        thread_id: String,
        seen_ts: i64,
        is_delivered_only: bool,
    },
    Reaction {
        thread_id: String,
        msg_id: String,
        cli_msg_id: Option<String>,
        actor_id: Option<String>,
        emoji: Option<String>,
        me: bool,
        removed: bool,
    },
    Voip(VoipControlEvent),
    SocialEvent {
        kind: String,
        thread_id: Option<String>,
        user_id: Option<String>,
        self_removed: bool,
    },
    PollUpdated {
        thread_id: String,
        poll_id: Option<i64>,
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
        .or_else(|| v.as_i64().map(|n| n != 0))
        .or_else(|| match v.as_str().map(str::trim) {
            Some("true") | Some("TRUE") | Some("1") => Some(true),
            Some("false") | Some("FALSE") | Some("0") => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

fn is_reminder_thumb_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/reminder_") || lower.contains("/reminder-")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallCardMeta {
    call_type: &'static str,
    outcome: &'static str,
    duration_secs: u64,
}

fn format_call_mm_ss(secs: u64) -> String {
    let mins = secs / 60;
    let rem = secs % 60;
    format!("{mins:02}:{rem:02}")
}

fn parse_duration_secs_from_text(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if !bytes[i].is_ascii_digit() {
            continue;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b':' {
            let mut k = j + 1;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > j + 1 {
                if let (Ok(m), Ok(s)) = (
                    text[i..j].parse::<u64>(),
                    text[j + 1..k].parse::<u64>(),
                ) {
                    if s < 60 {
                        return Some(m.saturating_mul(60).saturating_add(s));
                    }
                }
            }
        }
    }
    None
}

fn extract_call_meta(
    raw: Option<&Value>,
    msg_type: &str,
    is_outgoing_hint: bool,
) -> Option<CallCardMeta> {
    let lower_type = msg_type.to_ascii_lowercase();
    let explicit_type = matches!(
        lower_type.as_str(),
        "chat.call" | "call.voice" | "call.video" | "chat.voip" | "call" | "voip"
    ) || lower_type.starts_with("call.");
    let obj = raw.and_then(unwrap_json_object);
    let inner_params = obj
        .as_ref()
        .and_then(|o| o.get("params"))
        .and_then(unwrap_json_object);

    if !explicit_type {
        let looks_like_call = [obj.as_ref(), inner_params.as_ref()]
            .into_iter()
            .flatten()
            .any(|node| {
                node.get("callId").is_some()
                    || node.get("call_id").is_some()
                    || node.get("callType").is_some()
                    || node.get("call_type").is_some()
                    || node.get("callStatus").is_some()
                    || node.get("call_status").is_some()
            });
        if !looks_like_call {
            return None;
        }
    }

    let find_str = |keys: &[&str]| -> Option<String> {
        inner_params
            .as_ref()
            .and_then(|p| pick_str(p, keys))
            .or_else(|| obj.as_ref().and_then(|o| pick_str(o, keys)))
    };
    let find_u64 = |keys: &[&str]| -> Option<u64> {
        for key in keys {
            for node in [inner_params.as_ref(), obj.as_ref()].into_iter().flatten() {
                if let Some(v) = node.get(*key) {
                    if let Some(n) = v
                        .as_u64()
                        .or_else(|| v.as_i64().filter(|x| *x >= 0).map(|x| x as u64))
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
                    {
                        return Some(n);
                    }
                }
            }
        }
        None
    };

    let raw_text_hint = raw.and_then(extract_content_text).unwrap_or_default();
    let raw_call_type_str = find_str(&["callType", "call_type", "mediaType"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    let raw_call_type_num = find_u64(&["callType", "call_type"]);
    let is_video_flag = [inner_params.as_ref(), obj.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|node| node.get("isVideo").or_else(|| node.get("is_video")))
        .any(value_as_bool_flag);
    let is_video = lower_type.contains("video")
        || is_video_flag
        || raw_call_type_str.contains("video")
        || raw_call_type_num == Some(3)
        || raw_text_hint.to_ascii_lowercase().contains("video");
    let call_type = if is_video { "video" } else { "voice" };

    let duration_secs = find_u64(&["duration", "callDuration", "call_duration", "durationSecs"])
        .or_else(|| parse_duration_secs_from_text(&raw_text_hint))
        .unwrap_or(0);

    let status_text = find_str(&[
        "callStatus",
        "call_status",
        "reason",
        "state",
        "subType",
        "status",
        "title",
        "description",
    ])
    .unwrap_or_default()
    .to_ascii_lowercase();
    let status_code = find_u64(&["callStatus", "call_status", "status", "reasonCode"]);
    let combined_hint = format!("{status_text} {}", raw_text_hint.to_ascii_lowercase());

    let outcome = if duration_secs > 0 {
        "completed"
    } else if combined_hint.contains("miss")
        || combined_hint.contains("no_answer")
        || combined_hint.contains("timeout")
        || combined_hint.contains("nhỡ")
        || combined_hint.contains("bỏ lỡ")
        || combined_hint.contains("không trả lời")
        || status_code == Some(6)
    {
        "missed"
    } else if combined_hint.contains("cancel") || combined_hint.contains("hủy") || status_code == Some(2) {
        "cancelled"
    } else if combined_hint.contains("reject")
        || combined_hint.contains("declin")
        || combined_hint.contains("busy")
        || combined_hint.contains("từ chối")
        || combined_hint.contains("bận")
        || matches!(status_code, Some(3 | 4))
    {
        "declined"
    } else if combined_hint.contains("end")
        || combined_hint.contains("complet")
        || combined_hint.contains("answer")
        || combined_hint.contains("kết thúc")
    {
        "completed"
    } else if is_outgoing_hint {
        "cancelled"
    } else {
        "missed"
    };

    Some(CallCardMeta {
        call_type,
        outcome,
        duration_secs,
    })
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

fn collect_poll_nodes_inner(obj: &Value, depth: u8, out: &mut Vec<Value>) {
    if depth > 0 {
        for key in [
            "pollDetail",
            "poll_detail",
            "pollInfo",
            "poll",
            "topic",
            "params",
            "extra",
            "data",
        ] {
            if let Some(child) = obj.get(key).and_then(unwrap_json_object) {
                collect_poll_nodes_inner(&child, depth - 1, out);
            }
        }
    }
    out.push(obj.clone());
}

fn collect_poll_nodes(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(root) = unwrap_json_object(v) {
        collect_poll_nodes_inner(&root, 3, &mut out);
    }
    out
}

/// Returns `true` when `raw` looks like a Zalo poll vote / update / lock / add-option activity
/// status notification (NOT a poll creation notice and NOT a real poll question).
pub fn is_poll_update_activity_text(raw: &str) -> bool {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const UPDATE_PATTERNS: &[&str] = &[
        "đã tham gia bình chọn",
        "da tham gia binh chon",
        "đã bình chọn",
        "da binh chon",
        "bình chọn đã kết thúc",
        "binh chon da ket thuc",
        "kết thúc bình chọn",
        "ket thuc binh chon",
        "đã khóa bình chọn",
        "da khoa binh chon",
        "khóa bình chọn",
        "khoa binh chon",
        "đã đóng bình chọn",
        "da dong binh chon",
        "đóng bình chọn",
        "dong binh chon",
        "thêm phương án",
        "them phuong an",
        "thêm lựa chọn",
        "them lua chon",
        "cập nhật bình chọn",
        "cap nhat binh chon",
        "voted",
        "poll ended",
        "poll closed",
        "poll locked",
        "added an option",
        "added option",
        "updated the poll",
        "updated poll",
    ];
    UPDATE_PATTERNS.iter().any(|pat| lower.contains(pat))
}

fn is_poll_creation_banner_text(raw: &str) -> bool {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const CREATION_PATTERNS: &[&str] = &[
        "đã tạo bình chọn",
        "da tao binh chon",
        "tạo bình chọn mới",
        "tao binh chon moi",
        "created a poll",
        "created poll",
    ];
    CREATION_PATTERNS.iter().any(|pat| lower.contains(pat))
}

/// Returns `true` when `raw` is a poll system/activity notification string rather than a real
/// poll question (used by `parse_poll_value` and `merge_poll_dto` to prevent question clobbering).
pub fn is_poll_activity_text(raw: &str) -> bool {
    is_poll_update_activity_text(raw) || is_poll_creation_banner_text(raw)
}

fn is_poll_closing_text(raw: &str) -> bool {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    const CLOSE_PATTERNS: &[&str] = &[
        "bình chọn đã kết thúc",
        "binh chon da ket thuc",
        "kết thúc bình chọn",
        "ket thuc binh chon",
        "đã khóa bình chọn",
        "da khoa binh chon",
        "khóa bình chọn",
        "khoa binh chon",
        "đã đóng bình chọn",
        "da dong binh chon",
        "đóng bình chọn",
        "dong binh chon",
        "poll ended",
        "poll closed",
        "poll locked",
    ];
    CLOSE_PATTERNS.iter().any(|pat| lower.contains(pat))
}

fn parse_json_i64(val: &Value) -> Option<i64> {
    val.as_i64()
        .or_else(|| val.as_u64().and_then(|u| i64::try_from(u).ok()))
        .or_else(|| val.as_str()?.trim().parse::<i64>().ok())
}

pub fn parse_poll_value(v: &Value, fallback_poll_id: i64) -> Option<PollDto> {
    let nodes = collect_poll_nodes(v);
    if nodes.is_empty() {
        return None;
    }

    let read_i64 = |keys: &[&str]| -> Option<i64> {
        for key in keys {
            for node in &nodes {
                if let Some(val) = node.get(*key) {
                    if let Some(n) = parse_json_i64(val) {
                        return Some(n);
                    }
                }
            }
        }
        None
    };
    let read_pos_i64 = |keys: &[&str]| -> Option<i64> {
        for key in keys {
            for node in &nodes {
                if let Some(val) = node.get(*key) {
                    if let Some(n) = parse_json_i64(val).filter(|&x| x > 0) {
                        return Some(n);
                    }
                }
            }
        }
        None
    };
    let read_bool = |keys: &[&str]| -> bool {
        for key in keys {
            for node in &nodes {
                if let Some(val) = node.get(*key) {
                    if !val.is_null() {
                        return value_as_bool_flag(val);
                    }
                }
            }
        }
        false
    };
    let read_question = || -> Option<String> {
        for key in ["question", "title", "content"] {
            for node in &nodes {
                if let Some(s) = pick_str(node, &[key]) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty()
                        && !(trimmed.starts_with('{') && trimmed.ends_with('}'))
                        && !(trimmed.starts_with('[') && trimmed.ends_with(']'))
                        && !is_poll_activity_text(trimmed)
                    {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
        None
    };

    let poll_id = read_pos_i64(&["poll_id", "pollId"])
        .or_else(|| read_pos_i64(&["topicId", "topic_id", "id"]))
        .unwrap_or_else(|| fallback_poll_id.max(0));
    let question = read_question().unwrap_or_else(|| "Bình chọn".to_string());

    let mut raw_options: Vec<Value> = Vec::new();
    for node in &nodes {
        if let Some(opts) = node.get("options").and_then(|o| match o {
            Value::Array(arr) => Some(arr.clone()),
            Value::String(s) => serde_json::from_str::<Vec<Value>>(s.trim()).ok(),
            _ => None,
        }) {
            if !opts.is_empty() {
                raw_options = opts;
                break;
            }
        }
    }

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
                    .and_then(parse_json_i64)
                    .unwrap_or((idx + 1) as i64);
                let content = pick_str(&item, &["content", "title", "label", "name"])
                    .unwrap_or_else(|| format!("Lựa chọn {}", idx + 1));
                let explicit_votes = [
                    "total_vote",
                    "totalVote",
                    "vote_count",
                    "voteCount",
                    "votes",
                ]
                .iter()
                .find_map(|k| {
                    o.get(*k).and_then(|x| {
                        x.as_u64()
                            .or_else(|| x.as_i64().filter(|n| *n >= 0).map(|n| n as u64))
                            .or_else(|| x.as_str()?.trim().parse::<u64>().ok())
                            .or_else(|| x.as_array().map(|a| a.len() as u64))
                    })
                });
                let voters_count = o
                    .get("voters")
                    .map(unwrap_json_value)
                    .and_then(|v| v.as_array().map(|a| a.len() as u64))
                    .unwrap_or(0);
                let votes = explicit_votes.unwrap_or(0).max(voters_count) as u32;
                let voted = o
                    .get("voted")
                    .or_else(|| o.get("is_voted"))
                    .or_else(|| o.get("isVoted"))
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

    let expired_time = read_i64(&[
        "expired_time",
        "expiredTime",
        "expiried_time",
        "expired_at",
        "expiredAt",
    ])
    .filter(|&t| t > 0)
    .map(normalize_ts_ms)
    .unwrap_or(0);
    let status = read_i64(&["status"]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let is_expired = expired_time > 0 && expired_time < now_ms;
    let closed = read_bool(&["closed", "is_closed", "isClosed", "is_locked", "isLocked"])
        || status == Some(2)
        || is_expired;

    Some(PollDto {
        poll_id,
        question,
        options,
        allow_multi: read_bool(&[
            "allow_multi_choices",
            "allowMultiChoices",
            "allow_multi",
            "allowMulti",
        ]),
        allow_add_option: read_bool(&[
            "allow_add_new_option",
            "allowAddNewOption",
            "allow_add_option",
            "allowAddOption",
        ]),
        is_anonymous: read_bool(&["is_anonymous", "isAnonymous"]),
        expired_time,
        closed,
    })
}

fn extract_poll_meta(
    raw: Option<&Value>,
    msg_type: &str,
    outer_action: Option<&str>,
) -> Option<PollDto> {
    let lower_type = msg_type.to_ascii_lowercase();
    let explicit_type = lower_type.contains("poll");
    let obj = raw.and_then(unwrap_json_object);
    let nodes = obj.as_ref().map(collect_poll_nodes).unwrap_or_default();

    let action_is_poll = outer_action.is_some_and(|a| a.to_ascii_lowercase().contains("poll"))
        || nodes.iter().any(|node| {
            pick_str(
                node,
                &["action", "msgAction", "subType", "type", "board_type", "boardType"],
            )
            .is_some_and(|s| s.to_ascii_lowercase().contains("poll"))
        });

    let is_actionlist = lower_type.contains("msginfo.actionlist")
        || outer_action.is_some_and(|a| a.to_ascii_lowercase().contains("msginfo.actionlist"))
        || nodes.iter().any(|node| {
            pick_str(node, &["action", "msgAction"])
                .is_some_and(|s| s.to_ascii_lowercase().contains("msginfo.actionlist"))
        });

    let explicit = explicit_type || action_is_poll;
    if !explicit {
        let looks_like_poll = nodes.iter().any(|node| {
            node.get("poll_id").is_some()
                || node.get("pollId").is_some()
                || node.get("pollDetail").is_some()
                || node.get("poll_detail").is_some()
                || node.get("pollInfo").is_some()
                || (node.get("question").is_some() && node.get("options").is_some())
                || (is_actionlist
                    && node.get("options").is_some()
                    && (node.get("title").is_some() || node.get("id").is_some()))
        });
        if !looks_like_poll {
            return None;
        }
    }

    // Never fabricate `poll_id = msg_id`; leave `poll_id = 0` or return `None` when neither a
    // real poll ID nor options/question are present so vote notices never invent fake IDs.
    obj.as_ref().and_then(|o| parse_poll_value(o, 0))
}

fn is_poll_creation_action_token(tok: &str) -> bool {
    matches!(
        tok,
        "new_poll"
            | "create_poll"
            | "poll_create"
            | "poll.create"
            | "group.poll.create"
            | "chat.poll.create"
            | "newpoll"
            | "createpoll"
    ) || tok.contains("new_poll")
        || tok.contains("create_poll")
        || tok.contains("poll_create")
        || tok.contains("poll.create")
}

fn is_explicit_poll_update_action_token(tok: &str, has_poll_context: bool) -> bool {
    const EXPLICIT_TOKENS: &[&str] = &[
        "vote_poll",
        "poll_vote",
        "poll.vote",
        "votepoll",
        "update_poll",
        "poll_update",
        "poll.update",
        "updatepoll",
        "end_poll",
        "poll_end",
        "poll.end",
        "endpoll",
        "close_poll",
        "poll_close",
        "poll.close",
        "closepoll",
        "lock_poll",
        "poll_lock",
        "poll.lock",
        "lockpoll",
        "add_option",
        "add_poll_option",
        "poll_add_option",
        "poll.add_option",
        "addoption",
    ];
    if EXPLICIT_TOKENS.iter().any(|p| tok.contains(p)) {
        return true;
    }
    has_poll_context
        && (tok.contains("vote")
            || matches!(
                tok,
                "update" | "end" | "close" | "lock" | "add_opt" | "new_option"
            ))
}

fn is_poll_closing_action_token(tok: &str) -> bool {
    const CLOSE_TOKENS: &[&str] = &[
        "end_poll",
        "poll_end",
        "poll.end",
        "endpoll",
        "close_poll",
        "poll_close",
        "poll.close",
        "closepoll",
        "lock_poll",
        "poll_lock",
        "poll.lock",
        "lockpoll",
    ];
    CLOSE_TOKENS.iter().any(|p| tok.contains(p)) || matches!(tok, "end" | "close" | "lock")
}

/// Detect whether a raw group/chat message item represents a poll **vote / update / add-option /
/// end / lock** activity notice rather than an original poll creation card.
///
/// Returns `Some((thread_id, poll_id))` for activity notices so WebSocket ingestion emits
/// `WsParsedAction::PollUpdated` instead of spawning duplicate poll messages, and remote history
/// parsing folds or skips them cleanly.
pub(crate) fn extract_poll_activity_update(
    item: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
) -> Option<(String, Option<i64>)> {
    let raw_content = item
        .get("content")
        .or_else(|| item.get("message").and_then(|m| m.get("content")))
        .or_else(|| item.get("message"))
        .or_else(|| item.get("msg"));
    let content_obj = raw_content.and_then(unwrap_json_object);
    let nodes = content_obj
        .as_ref()
        .map(collect_poll_nodes)
        .unwrap_or_default();

    let mut action_tokens: Vec<String> = Vec::new();
    for key in [
        "msgType",
        "type",
        "action",
        "msgAction",
        "act_type",
        "act",
        "subType",
        "eventType",
    ] {
        if let Some(s) = pick_str(item, &[key]) {
            action_tokens.push(s.to_ascii_lowercase());
        }
    }
    for node in &nodes {
        for key in [
            "action",
            "msgAction",
            "act_type",
            "act",
            "subType",
            "msgType",
            "type",
            "board_type",
            "boardType",
            "eventType",
        ] {
            if let Some(s) = pick_str(node, &[key]) {
                action_tokens.push(s.to_ascii_lowercase());
            }
        }
    }

    if action_tokens.iter().any(|t| is_poll_creation_action_token(t)) {
        return None;
    }

    let lower_msg_type = normalize_msg_type(item).to_ascii_lowercase();
    let has_poll_field = item.get("poll_id").is_some()
        || item.get("pollId").is_some()
        || nodes.iter().any(|node| {
            node.get("poll_id").is_some()
                || node.get("pollId").is_some()
                || node.get("pollDetail").is_some()
                || node.get("poll_detail").is_some()
                || node.get("pollInfo").is_some()
        });
    let has_poll_token = action_tokens.iter().any(|t| t.contains("poll"));
    let has_poll_context = has_poll_field || has_poll_token;

    let is_action_envelope = lower_msg_type.contains("group.action")
        || lower_msg_type.contains("actionlist")
        || action_tokens
            .iter()
            .any(|t| t.contains("msginfo.actionlist") || t.contains("group.action"));
    let is_group_action_or_poll_envelope =
        has_poll_context || is_action_envelope || lower_msg_type.starts_with("group.");

    let has_poll_update_action = action_tokens
        .iter()
        .any(|t| is_explicit_poll_update_action_token(t, has_poll_context));

    let mut notice_texts: Vec<String> = Vec::new();
    if let Some(Value::String(s)) = raw_content {
        let trimmed = s.trim();
        if !trimmed.is_empty() && !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
            notice_texts.push(trimmed.to_string());
        }
    }
    for node in &nodes {
        for key in ["title", "description", "msg", "text"] {
            if let Some(s) = pick_str(node, &[key]) {
                let trimmed = s.trim();
                if !trimmed.is_empty() && !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
                    notice_texts.push(trimmed.to_string());
                }
            }
        }
    }

    let has_poll_update_text = notice_texts.iter().any(|t| is_poll_update_activity_text(t));
    let has_creation_text = notice_texts.iter().any(|t| is_poll_creation_banner_text(t));

    let parsed_poll = content_obj.as_ref().and_then(|o| parse_poll_value(o, 0));
    let has_creation_payload = parsed_poll.as_ref().is_some_and(|p| {
        !p.options.is_empty()
            || (!p.question.trim().is_empty()
                && p.question != "Bình chọn"
                && !is_poll_activity_text(&p.question))
    });

    let is_poll_activity = has_poll_update_action
        || (is_group_action_or_poll_envelope && has_poll_update_text && !has_creation_text)
        || (is_action_envelope
            && has_poll_context
            && !has_creation_text
            && !has_creation_payload);

    if !is_poll_activity {
        return None;
    }

    let gid_keys = [
        "grid",
        "groupId",
        "group_id",
        "idTo",
        "toid",
        "toId",
        "threadId",
        "conversId",
    ];
    let thread_id = extract_thread_id(item, in_group_bucket, my_uid, send2me_id)
        .map(|(t, _)| t)
        .or_else(|| {
            nodes
                .iter()
                .find_map(|n| pick_str(n, &gid_keys))
                .map(|raw| clean_group_id(&raw).to_string())
        })
        .filter(|t| !t.is_empty() && t != "0")?;

    let poll_id = ["poll_id", "pollId"]
        .iter()
        .find_map(|key| {
            nodes
                .iter()
                .chain(std::iter::once(item))
                .find_map(|n| n.get(*key).and_then(parse_json_i64))
                .filter(|&id| id > 0)
        })
        .or_else(|| {
            ["topicId", "topic_id", "id"].iter().find_map(|key| {
                nodes
                    .iter()
                    .find_map(|n| n.get(*key).and_then(parse_json_i64))
                    .filter(|&id| id > 0 && id.to_string() != thread_id)
            })
        });

    Some((thread_id, poll_id))
}

/// Extract any partial `PollDto` update (updated options, `closed` flag, etc.) carried inside a
/// poll activity notification item so remote history folding can apply it to the canonical card.
pub(crate) fn extract_poll_activity_dto(item: &Value, known_poll_id: Option<i64>) -> Option<PollDto> {
    let raw_content = item
        .get("content")
        .or_else(|| item.get("message").and_then(|m| m.get("content")))
        .or_else(|| item.get("message"))
        .or_else(|| item.get("msg"));
    let content_obj = raw_content.and_then(unwrap_json_object);
    let nodes = content_obj
        .as_ref()
        .map(collect_poll_nodes)
        .unwrap_or_default();

    let mut is_closing = ["msgType", "type", "action", "msgAction", "act_type", "act", "subType"]
        .iter()
        .filter_map(|k| pick_str(item, &[k]))
        .any(|s| is_poll_closing_action_token(&s.to_ascii_lowercase()))
        || nodes.iter().any(|node| {
            ["action", "msgAction", "act_type", "act", "subType", "msgType", "type"]
                .iter()
                .filter_map(|k| pick_str(node, &[k]))
                .any(|s| is_poll_closing_action_token(&s.to_ascii_lowercase()))
        });
    if !is_closing {
        if let Some(Value::String(s)) = raw_content {
            if is_poll_closing_text(s) {
                is_closing = true;
            }
        }
    }
    if !is_closing {
        is_closing = nodes.iter().any(|node| {
            ["title", "description", "msg", "text"]
                .iter()
                .filter_map(|k| pick_str(node, &[k]))
                .any(|s| is_poll_closing_text(&s))
        });
    }

    let fallback_id = known_poll_id.unwrap_or(0).max(0);
    let mut dto = content_obj
        .as_ref()
        .and_then(|o| parse_poll_value(o, fallback_id))
        .or_else(|| {
            if fallback_id > 0 && is_closing {
                Some(PollDto {
                    poll_id: fallback_id,
                    question: "Bình chọn".to_string(),
                    options: Vec::new(),
                    allow_multi: false,
                    allow_add_option: false,
                    is_anonymous: false,
                    expired_time: 0,
                    closed: true,
                })
            } else {
                None
            }
        })?;
    if is_closing {
        dto.closed = true;
    }
    if dto.poll_id <= 0 {
        return None;
    }
    Some(dto)
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
    let trimmed = code.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "/-none" | "/-remove" | "none"
        )
    {
        return None;
    }
    match trimmed {
        "/-strong" | "👍" => Some("👍"),
        "/-heart" | "❤" | "❤️" | "/-loveu" => Some("❤️"),
        ":>" | ":))" | "😆" => Some("😆"),
        ":o" | ":-o" | "😮" => Some("😮"),
        ":-((" | ":((" | "--b" | "p-(" | ":-<" | "😢" | "😭" => Some("😢"),
        ":-h" | ">-|" | "😠" | "😡" => Some("😠"),
        ":-*" | "😘" => Some("😘"),
        ":')" | "😂" => Some("😂"),
        "/-shit" | "💩" => Some("💩"),
        "/-rose" | "🌹" => Some("🌹"),
        "/-break" | "💔" => Some("💔"),
        "/-weak" | "/-bad" | "👎" => Some("👎"),
        ";xx" | "😍" => Some("😍"),
        ";-/" | "😕" => Some("😕"),
        ";-)" | "😉" => Some("😉"),
        "/-fade" | "🥀" => Some("🥀"),
        "/-li" | "☀" | "☀️" => Some("☀️"),
        "/-bd" | "🎂" => Some("🎂"),
        "/-bome" | "💣" => Some("💣"),
        "/-ok" | "👌" => Some("👌"),
        "/-v" | "✌" | "✌️" => Some("✌️"),
        "/-thanks" | "_()_" | "🙏" => Some("🙏"),
        "/-punch" | "👊" => Some("👊"),
        "/-no" | "🙅" => Some("🙅"),
        "x-)" | "b-)" | "😎" => Some("😎"),
        "8-)" | "🤓" => Some("🤓"),
        ";-d" | "😁" => Some("😁"),
        ":--|" | "😐" => Some("😐"),
        ":-bye" | "👋" => Some("👋"),
        "|-)" | "😴" => Some("😴"),
        ":handclap" | "👏" => Some("👏"),
        ";-x" | "🤐" => Some("🤐"),
        ";-s" | "😳" => Some("😳"),
        ";-a" | "😨" => Some("😨"),
        "$-)" | "🤑" => Some("🤑"),
        "/-beer" | "🍺" => Some("🍺"),
        _ => None,
    }
}

pub fn reaction_type_to_emoji(r_type: i64) -> Option<&'static str> {
    match r_type {
        5 | 133 => Some("❤️"),
        3 => Some("👍"),
        0 | 62 => Some("😆"),
        32 | 53 => Some("😮"),
        2 | 1 | 16 | 35 | 61 => Some("😢"),
        20 | 47 => Some("😠"),
        8 => Some("😘"),
        7 => Some("😂"),
        66 => Some("💩"),
        120 => Some("🌹"),
        65 => Some("💔"),
        4 | 132 => Some("👎"),
        29 => Some("😍"),
        51 => Some("😕"),
        45 => Some("😉"),
        121 => Some("🥀"),
        67 => Some("☀️"),
        126 => Some("🎂"),
        127 => Some("💣"),
        68 => Some("👌"),
        69 => Some("✌️"),
        70 | 73 => Some("🙏"),
        71 => Some("👊"),
        131 => Some("🙅"),
        21 | 26 => Some("😎"),
        22 => Some("🤓"),
        23 => Some("😁"),
        30 => Some("😐"),
        36 => Some("👋"),
        38 => Some("😴"),
        46 => Some("👏"),
        52 => Some("🤐"),
        54 => Some("😳"),
        60 => Some("😨"),
        63 => Some("🤑"),
        99 => Some("🍺"),
        _ => None,
    }
}

/// Normalize a Zalo wire reaction (`rIcon` + optional `rType`) into a canonical display emoji.
/// Returns `None` for removal/unreact events (`rType == -1` or removal sentinel `rIcon`).
/// Preserves custom Unicode emojis sent by Zalo Mobile verbatim.
pub fn reaction_to_emoji(code: &str, r_type: Option<i64>) -> Option<String> {
    if r_type == Some(-1) {
        return None;
    }
    let trimmed = code.trim();
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "/-none" | "/-remove" | "none"
    ) {
        return None;
    }
    if let Some(mapped) = try_reaction_code_to_emoji(trimmed) {
        return Some(mapped.to_string());
    }
    if !trimmed.is_empty() && !trimmed.is_ascii() {
        return Some(trimmed.to_string());
    }
    r_type.and_then(reaction_type_to_emoji).map(str::to_string)
}

/// Normalize a user-supplied or wire reaction emoji/code into the canonical display emoji.
pub fn canonicalize_reaction_emoji(input: &str) -> Option<String> {
    reaction_to_emoji(input, None)
}

pub fn reaction_code_to_emoji(code: &str) -> &'static str {
    try_reaction_code_to_emoji(code).unwrap_or("❤️")
}

fn is_self_reaction_uid(uid: &str, my_uid: &str) -> bool {
    let clean = clean_bare_uid(uid);
    if clean.is_empty() {
        return false;
    }
    clean == "0" || (!my_uid.is_empty() && clean == my_uid)
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
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
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
    let mut map: std::collections::BTreeMap<String, (u32, bool)> =
        std::collections::BTreeMap::new();
    for key in ["reactions", "reacts", "reactionList", "react"] {
        let Some(raw) = it.get(key) else {
            continue;
        };
        let unwrapped = unwrap_json_value(raw);
        let items: Vec<&Value> = match &unwrapped {
            Value::Array(arr) => arr.iter().collect(),
            Value::Object(_) => vec![&unwrapped],
            _ => Vec::new(),
        };
        for r in items {
            let r_type = r
                .get("rType")
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
            if r_type == Some(-1) {
                continue;
            }
            let code = pick_str(r, &["rIcon", "emoji", "icon"]).unwrap_or_default();
            let Some(e) = reaction_to_emoji(&code, r_type) else {
                continue;
            };
            let count = reaction_entry_count(r);
            let is_me = reaction_entry_is_me(r, my_uid);
            let entry = map.entry(e).or_insert((0, false));
            entry.0 += count;
            entry.1 |= is_me;
        }
    }
    map.into_iter()
        .map(|(emoji, (count, me))| ReactionSummary { emoji, count, me })
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
        let clean = clean_group_id(&grid);
        let clean = clean.split('_').next().unwrap_or(clean).trim();
        if !clean.is_empty() && clean != "0" {
            return Some((clean.to_string(), true));
        }
    }
    if is_group {
        if let Some(gid) = pick_str(entry, &["idTo", "toid", "toId", "threadId", "conversId"])
            .filter(|s| s != "0")
        {
            let clean = clean_group_id(&gid);
            let clean = clean.split('_').next().unwrap_or(clean).trim();
            if !clean.is_empty() && clean != "0" {
                return Some((clean.to_string(), true));
            }
        }
    }
    let raw_uid_from =
        pick_str(entry, &["uidFrom", "fromUid", "fromId", "senderId"]).unwrap_or_default();
    let raw_id_to = pick_str(entry, &["idTo", "toid", "toId"]).unwrap_or_default();

    if !send2me_id.is_empty()
        && ((raw_id_to == "0" && raw_uid_from == "0")
            || raw_id_to == send2me_id
            || raw_uid_from == send2me_id)
    {
        return Some((send2me_id.to_string(), false));
    }

    let clean_my_uid = clean_bare_uid(my_uid);
    let uid_from = clean_bare_uid(&raw_uid_from);
    let id_to = clean_bare_uid(&raw_id_to);

    if !send2me_id.is_empty() && (id_to == "0" && uid_from == "0") {
        return Some((send2me_id.to_string(), false));
    }

    let is_self_from = !my_uid.is_empty() && (raw_uid_from == my_uid || uid_from == clean_my_uid);
    let is_self_to = !my_uid.is_empty() && (raw_id_to == my_uid || id_to == clean_my_uid);

    if !uid_from.is_empty()
        && uid_from != "0"
        && !is_self_from
        && (id_to.is_empty() || id_to == "0" || is_self_to)
    {
        Some((uid_from, false))
    } else if !id_to.is_empty() && id_to != "0" && !is_self_to {
        Some((id_to, false))
    } else {
        pick_str(entry, &["threadId", "conversId", "userId", "friendId"]).and_then(|s| {
            if !send2me_id.is_empty() && s == send2me_id {
                return Some((send2me_id.to_string(), false));
            }
            let cleaned = if is_group {
                let g = clean_group_id(&s);
                g.split('_').next().unwrap_or(g).trim().to_string()
            } else {
                clean_bare_uid(&s)
            };
            if cleaned.is_empty() || cleaned == "0" {
                None
            } else {
                Some((cleaned, is_group))
            }
        })
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
    if msg_type.eq_ignore_ascii_case("chat.reaction") {
        return None;
    }
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

    let uid_from_hint =
        pick_str(it, &["uidFrom", "fromUid", "fromId", "senderId"]).unwrap_or_default();
    let clean_uid_from_hint = uid_from_hint.split('_').next().unwrap_or(uid_from_hint.as_str());
    let is_self_flag_hint = [it.get("isSelf"), it.get("is_self")]
        .into_iter()
        .flatten()
        .any(|v| v.as_bool() == Some(true) || v.as_i64() == Some(1) || v.as_str() == Some("1"));
    let is_outgoing_hint = is_self_flag_hint
        || clean_uid_from_hint == "0"
        || clean_uid_from_hint.is_empty()
        || (!my_uid.is_empty() && (uid_from_hint == my_uid || clean_uid_from_hint == my_uid))
        || (!send2me_id.is_empty() && clean_uid_from_hint == send2me_id);

    let reminder_meta = extract_reminder_meta(raw_content, &msg_type);
    let outer_action = pick_str(it, &["action", "msgAction", "subType"]);
    let poll_meta = if reminder_meta.is_none() {
        extract_poll_meta(raw_content, &msg_type, outer_action.as_deref())
    } else {
        None
    };
    let is_undo = matches!(msg_type.as_str(), "chat.undo" | "undo" | "31");
    let call_meta = if !is_undo && reminder_meta.is_none() && poll_meta.is_none() {
        extract_call_meta(raw_content, &msg_type, is_outgoing_hint)
    } else {
        None
    };

    let (mut attachment_url, mut attachment_name) =
        if reminder_meta.is_some() || poll_meta.is_some() || call_meta.is_some() {
            (None, None)
        } else {
            raw_content
                .map(|rv| extract_attachment(rv, &msg_type))
                .unwrap_or((None, None))
        };

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
    } else if let Some(call) = call_meta {
        msg_type = "chat.call".to_string();
        sticker_meta = None;
        attachment_url = None;
        let meta_obj = serde_json::json!({
            "callType": call.call_type,
            "outcome": call.outcome,
            "durationSecs": call.duration_secs,
        });
        attachment_name = Some(format!("teza-call:{meta_obj}"));
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
    } else if let Some(call) = call_meta {
        let noun = if call.call_type == "video" {
            "Cuộc gọi video"
        } else {
            "Cuộc gọi thoại"
        };
        if call.duration_secs > 0 {
            format!("{noun} ({})", format_call_mm_ss(call.duration_secs))
        } else {
            match call.outcome {
                "missed" => format!("{noun} nhỡ"),
                "cancelled" => format!("Đã hủy {}", noun.to_lowercase()),
                "declined" => format!("{noun} bị từ chối"),
                _ => noun.to_string(),
            }
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

    let g_msg_id = pick_str(it, &["msgId", "globalMsgId", "realMsgId", "id"])
        .filter(|s| s != "0");
    let cli_msg_id = pick_str(it, &["cliMsgId", "clientMsgId", "cMsgID", "cMsgId"])
        .filter(|s| s != "0");
    let msg_id = g_msg_id.or_else(|| cli_msg_id.clone())?;
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
        cli_msg_id,
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
            | CMD_PULL_MSG_E2EE_1_1
            | CMD_PULL_MSG_513
            | CMD_PULL_MSG_514
            | CMD_PULL_MSG_515
            | CMD_PULL_MSG_E2EE_GROUP
            | CMD_PULL_MSG_517
            | CMD_PULL_MSG_518
            | CMD_PUSH_MISS_MSG
            | CMD_PUSH_MSG_E2EE_1_1
            | CMD_PUSH_MSG_E2EE_GROUP
            | CMD_GET_MSG
    ) {
        let is_group_cmd = matches!(
            ev.cmd,
            CMD_PUSH_MSG_GROUP
                | CMD_PULL_MSG_GROUP
                | CMD_PULL_MSG_E2EE_GROUP
                | CMD_PUSH_MSG_E2EE_GROUP
        );
        let unwrapped = unwrap_json_value(data);
        collect_messages_from_node(&unwrapped, is_group_cmd, my_uid, send2me_id, &mut actions);
        collect_clear_unreads_from_node(
            &unwrapped,
            is_group_cmd,
            my_uid,
            send2me_id,
            &mut actions,
        );
    } else if matches!(ev.cmd, CMD_CLEAR_UNREAD_1_1 | CMD_CLEAR_UNREAD_GROUP) {
        let is_group_cmd = ev.cmd == CMD_CLEAR_UNREAD_GROUP;
        let unwrapped = unwrap_json_value(data);
        if ev.has_chat_message_payload()
            || unwrapped.get("msgs").is_some()
            || unwrapped.get("groupMsgs").is_some()
        {
            collect_messages_from_node(&unwrapped, is_group_cmd, my_uid, send2me_id, &mut actions);
        }
        collect_clear_unreads_from_node(
            &unwrapped,
            is_group_cmd,
            my_uid,
            send2me_id,
            &mut actions,
        );
        if has_receipt_bucket(&unwrapped) {
            collect_receipts_from_node(
                &unwrapped,
                is_group_cmd,
                true,
                my_uid,
                send2me_id,
                &mut actions,
            );
        }
    } else if matches!(ev.cmd, CMD_REACTION_PUSH | CMD_REACTION_PULL_GROUP) {
        collect_reactions_from_node(
            &ev.payload,
            ev.cmd == CMD_REACTION_PULL_GROUP,
            my_uid,
            send2me_id,
            &mut actions,
        );
    } else if ev.cmd == CMD_CTRL_PULL_610 {
        collect_reactions_from_node(&ev.payload, false, my_uid, send2me_id, &mut actions);
        collect_controls_from_node(&ev.payload, my_uid, send2me_id, &mut actions);
    } else if matches!(
        ev.cmd,
        CMD_CTRL_PUSH_CTRL | CMD_CTRL_ACTION | CMD_CTRL_PULL_CTRL | CMD_CTRL_PULL_VOIP
    ) {
        collect_controls_from_node(&ev.payload, my_uid, send2me_id, &mut actions);
    } else if matches!(
        ev.cmd,
        CMD_DELIVERED_1_1
            | CMD_SEEN_1_1
            | CMD_PULL_ACK_GROUP
            | CMD_DELIVERED_GROUP
            | CMD_SEEN_GROUP
            | CMD_E2EE_DELIVER_SEEN_1_1
            | CMD_E2EE_DELIVER_SEEN_GROUP
    ) {
        let is_group_cmd = matches!(
            ev.cmd,
            CMD_DELIVERED_GROUP
                | CMD_SEEN_GROUP
                | CMD_PULL_ACK_GROUP
                | CMD_E2EE_DELIVER_SEEN_GROUP
        );
        let default_seen = matches!(ev.cmd, CMD_SEEN_1_1 | CMD_SEEN_GROUP) || ev.sub_cmd == 1;
        let receipt_data = unwrap_json_value(data);
        collect_clear_unreads_from_node(
            &receipt_data,
            is_group_cmd,
            my_uid,
            send2me_id,
            &mut actions,
        );
        collect_receipts_from_node(
            &receipt_data,
            is_group_cmd,
            default_seen,
            my_uid,
            send2me_id,
            &mut actions,
        );
    } else if ev.cmd != CMD_AUTHEN && ev.cmd != CMD_PING {
        if ev.has_chat_message_payload() {
            let unwrapped = unwrap_json_value(data);
            collect_messages_from_node(&unwrapped, false, my_uid, send2me_id, &mut actions);
            collect_clear_unreads_from_node(&unwrapped, false, my_uid, send2me_id, &mut actions);
        }
        for voip_ev in extract_voip_controls(&ev.payload) {
            actions.push(WsParsedAction::Voip(voip_ev));
        }
    }
    actions
}

fn pick_str_only(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(Value::String(s)) = v.get(*k) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn unwrap_json_value(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
            {
                serde_json::from_str::<Value>(trimmed).unwrap_or_else(|_| v.clone())
            } else {
                v.clone()
            }
        }
        other => other.clone(),
    }
}

fn has_truthy_self_flag(v: &Value) -> bool {
    [v.get("isSelf"), v.get("is_self")]
        .into_iter()
        .flatten()
        .any(|val| {
            val.as_bool() == Some(true)
                || val.as_i64().is_some_and(|n| n != 0)
                || val
                    .as_str()
                    .is_some_and(|s| matches!(s.trim(), "1" | "true" | "TRUE"))
        })
}

fn is_self_uid(uid: &str, my_uid: &str) -> bool {
    let trimmed = uid.trim();
    if trimmed.is_empty() || trimmed == "0" || my_uid.is_empty() {
        return false;
    }
    let clean = trimmed.split('_').next().unwrap_or(trimmed);
    trimmed == my_uid || clean == my_uid
}

/// Returns `true` when a receipt actor UID refers to the logged-in account (either explicit `"0"`,
/// which Zalo uses for self actions synced from mobile/desktop, or matching `my_uid`).
fn is_self_receipt_uid(uid: &str, my_uid: &str) -> bool {
    let clean = clean_bare_uid(uid);
    if clean.is_empty() {
        return false;
    }
    clean == "0" || (!my_uid.is_empty() && clean == my_uid)
}

fn is_peer_receipt_uid(uid: &str, my_uid: &str) -> bool {
    let clean = clean_bare_uid(uid);
    !clean.is_empty() && clean != "0" && (my_uid.is_empty() || clean != my_uid)
}

fn pick_receipt_last_msg_id(item: &Value) -> Option<String> {
    pick_str(
        item,
        &["lastMsgId", "last_msg_id", "msgId", "globalMsgId", "realMsgId"],
    )
    .filter(|s| !s.is_empty() && s != "0" && !s.starts_with('-'))
}

fn has_receipt_bucket(node: &Value) -> bool {
    [
        "seens",
        "seenMsgs",
        "seen_msgs",
        "groupSeens",
        "group_seens",
        "delivereds",
        "deliverMsgs",
        "deliver_msgs",
        "deliveredMsgs",
        "groupDelivereds",
        "group_delivereds",
    ]
    .iter()
    .any(|k| node.get(*k).is_some())
}

fn parse_clear_unread_item(
    item: &Value,
    default_group: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    // Zalo `clearUnreads` entries use `type == 0` (or omitted) for message unread clears;
    // `type == 1` is `CLEAR_UNREAD_TYPE.REACTION`, and `type == 2` arrives with sentinel `idTo: "-1"`.
    if let Some(raw_type) = item.get("type") {
        let parsed_type = raw_type
            .as_i64()
            .or_else(|| raw_type.as_str().and_then(|s| s.trim().parse::<i64>().ok()));
        if parsed_type.is_some_and(|t| t != 0) {
            return;
        }
    }

    let is_group = default_group
        || is_group_flag(item)
        || item.get("grid").is_some()
        || item.get("groupId").is_some();

    let raw_id = if is_group {
        pick_str(
            item,
            &[
                "idTo",
                "grid",
                "groupId",
                "toid",
                "toId",
                "threadId",
                "conversId",
                "id",
            ],
        )
    } else {
        pick_str(
            item,
            &[
                "idTo",
                "toid",
                "toId",
                "threadId",
                "conversId",
                "userId",
                "uid",
                "id",
            ],
        )
    };
    let Some(raw_id) = raw_id else {
        return;
    };
    let trimmed = raw_id.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return;
    }

    let thread_id = if is_group
        || trimmed.starts_with("group_")
        || trimmed.starts_with('g')
        || trimmed.starts_with('G')
    {
        clean_group_id(trimmed).to_string()
    } else {
        let clean = clean_bare_uid(trimmed);
        if !send2me_id.is_empty() && (clean == "0" || (!my_uid.is_empty() && clean == my_uid)) {
            send2me_id.to_string()
        } else {
            clean
        }
    };
    if thread_id.is_empty()
        || thread_id == "0"
        || thread_id.starts_with('-')
        || (!is_group
            && !my_uid.is_empty()
            && thread_id == my_uid
            && send2me_id.is_empty())
    {
        return;
    }

    out.push(WsParsedAction::Read {
        thread_id,
        is_unread: false,
        last_msg_id: pick_receipt_last_msg_id(item),
    });
}

fn collect_clear_unreads_from_node(
    node: &Value,
    default_group: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let mut visited = false;
    for key in ["clearUnreads", "clear_unreads"] {
        if let Some(raw) = node.get(key) {
            visited = true;
            let unwrapped = unwrap_json_value(raw);
            if let Some(arr) = unwrapped.as_array() {
                for item in arr {
                    parse_clear_unread_item(item, default_group, my_uid, send2me_id, out);
                }
            }
        }
    }
    if !visited {
        if let Some(inner) = node.get("data") {
            let unwrapped = unwrap_json_value(inner);
            if unwrapped.is_object() {
                for key in ["clearUnreads", "clear_unreads"] {
                    if let Some(raw) = unwrapped.get(key) {
                        let arr_val = unwrap_json_value(raw);
                        if let Some(arr) = arr_val.as_array() {
                            for item in arr {
                                parse_clear_unread_item(
                                    item,
                                    default_group,
                                    my_uid,
                                    send2me_id,
                                    out,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

fn extract_seen_uids(item: &Value) -> Option<Vec<String>> {
    let raw = item.get("seenUids").or_else(|| item.get("seen_uids"))?;
    let unwrapped = unwrap_json_value(raw);
    let arr = unwrapped.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = match v {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            Value::Object(_) => pick_str(v, &["uid", "userId", "id"]).unwrap_or_default(),
            _ => String::new(),
        };
        if !s.is_empty() {
            out.push(s);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_receipt_item(
    item: &Value,
    is_group_cmd: bool,
    is_seen: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let is_group = is_group_cmd
        || item.get("grid").is_some()
        || item.get("groupId").is_some()
        || is_group_flag(item);

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

    let is_self_flag = has_truthy_self_flag(item);
    let seen_uids = extract_seen_uids(item);
    let last_msg_id = pick_receipt_last_msg_id(item);

    if is_group {
        let Some(raw_gid) = pick_str(
            item,
            &["grid", "groupId", "idTo", "toid", "toId", "threadId", "conversId"],
        ) else {
            return;
        };
        let thread_id = clean_group_id(&raw_gid).to_string();
        if thread_id.is_empty() || thread_id == "0" || thread_id.starts_with('-') {
            return;
        }

        let (has_self, has_peer) = if let Some(ref uids) = seen_uids {
            let self_in_list = uids.iter().any(|u| is_self_receipt_uid(u, my_uid));
            let peer_in_list = uids.iter().any(|u| is_peer_receipt_uid(u, my_uid));
            (is_self_flag || self_in_list, peer_in_list)
        } else {
            let uid_from = pick_str(item, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
            let self_from = is_self_receipt_uid(&uid_from, my_uid);
            let has_self = is_self_flag || self_from;
            let has_peer =
                !has_self && (uid_from.is_empty() || is_peer_receipt_uid(&uid_from, my_uid));
            (has_self, has_peer)
        };

        if is_seen && has_self {
            out.push(WsParsedAction::Read {
                thread_id: thread_id.clone(),
                is_unread: false,
                last_msg_id: last_msg_id.clone(),
            });
        }
        if has_peer {
            out.push(WsParsedAction::PeerSeen {
                thread_id,
                seen_ts: parsed_ts,
                is_delivered_only: !is_seen,
            });
        }
        return;
    }

    // Direct 1-1 receipt
    if let Some(ref uids) = seen_uids {
        let Some(thread_id) = [
            "idTo",
            "toid",
            "toId",
            "uidFrom",
            "fromUid",
            "fromId",
            "userId",
            "threadId",
            "conversId",
        ]
        .iter()
        .filter_map(|k| pick_str(item, &[k]))
        .map(|s| clean_bare_uid(&s))
        .find(|s| is_peer_receipt_uid(s, my_uid))
        .or_else(|| extract_thread_id(item, false, my_uid, send2me_id).map(|(t, _)| t)) else {
            return;
        };
        let has_self = is_self_flag || uids.iter().any(|u| is_self_receipt_uid(u, my_uid));
        let has_peer = uids.iter().any(|u| is_peer_receipt_uid(u, my_uid));
        if is_seen && has_self {
            out.push(WsParsedAction::Read {
                thread_id: thread_id.clone(),
                is_unread: false,
                last_msg_id: last_msg_id.clone(),
            });
        }
        if has_peer {
            out.push(WsParsedAction::PeerSeen {
                thread_id,
                seen_ts: parsed_ts,
                is_delivered_only: !is_seen,
            });
        }
        return;
    }

    let uid_from = pick_str(item, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
    let reader_is_self = is_self_flag || is_self_receipt_uid(&uid_from, my_uid);

    if reader_is_self {
        if !is_seen {
            return;
        }
        let target_thread = [
            "idTo",
            "toid",
            "toId",
            "threadId",
            "conversId",
            "userId",
            "uidFrom",
            "fromUid",
            "fromId",
        ]
        .iter()
        .filter_map(|k| pick_str(item, &[k]))
        .map(|s| clean_bare_uid(&s))
        .find(|s| is_peer_receipt_uid(s, my_uid))
        .or_else(|| {
            if !send2me_id.is_empty() {
                let id_to = pick_str(item, &["idTo", "toid", "toId"])
                    .map(|s| clean_bare_uid(&s))
                    .unwrap_or_default();
                if id_to == send2me_id
                    || id_to == "0"
                    || (!my_uid.is_empty() && id_to == my_uid)
                {
                    return Some(send2me_id.to_string());
                }
            }
            None
        });
        if let Some(thread_id) = target_thread {
            out.push(WsParsedAction::Read {
                thread_id,
                is_unread: false,
                last_msg_id,
            });
        }
        return;
    }

    let Some(thread_id) = [
        "idTo",
        "toid",
        "toId",
        "uidFrom",
        "fromUid",
        "fromId",
        "userId",
        "threadId",
        "conversId",
    ]
    .iter()
    .filter_map(|k| pick_str(item, &[k]))
    .map(|s| clean_bare_uid(&s))
    .find(|s| is_peer_receipt_uid(s, my_uid))
    .or_else(|| extract_thread_id(item, false, my_uid, send2me_id).map(|(t, _)| t)) else {
        return;
    };

    out.push(WsParsedAction::PeerSeen {
        thread_id,
        seen_ts: parsed_ts,
        is_delivered_only: !is_seen,
    });
}

fn collect_receipts_from_node(
    node: &Value,
    is_group_cmd: bool,
    default_seen: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let buckets: &[(&str, bool, Option<bool>)] = &[
        ("seens", true, None),
        ("seenMsgs", true, None),
        ("seen_msgs", true, None),
        ("groupSeens", true, Some(true)),
        ("group_seens", true, Some(true)),
        ("delivereds", false, None),
        ("deliverMsgs", false, None),
        ("deliver_msgs", false, None),
        ("deliveredMsgs", false, None),
        ("groupDelivereds", false, Some(true)),
        ("group_delivereds", false, Some(true)),
    ];
    let mut matched_bucket = false;
    for (key, is_seen, force_group) in buckets {
        if let Some(raw) = node.get(*key) {
            let unwrapped = unwrap_json_value(raw);
            if let Some(arr) = unwrapped.as_array() {
                matched_bucket = true;
                let bucket_group = force_group.unwrap_or(is_group_cmd);
                for item in arr {
                    parse_receipt_item(item, bucket_group, *is_seen, my_uid, send2me_id, out);
                }
            }
        }
    }
    if matched_bucket {
        return;
    }
    if node.get("clearUnreads").is_some()
        || node.get("clear_unreads").is_some()
        || node.get("msgs").is_some()
        || node.get("groupMsgs").is_some()
    {
        return;
    }

    if let Some(arr) = node.as_array() {
        for item in arr {
            parse_receipt_item(item, is_group_cmd, default_seen, my_uid, send2me_id, out);
        }
    } else if node.is_object() {
        let is_seen =
            default_seen || node.get("seenUids").is_some() || node.get("seen_uids").is_some();
        parse_receipt_item(node, is_group_cmd, is_seen, my_uid, send2me_id, out);
    }
}

fn collect_messages_from_node(
    node: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    collect_messages_from_node_inner(node, in_group_bucket, my_uid, send2me_id, 3, out);
}

fn collect_messages_from_node_inner(
    node: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    depth: u8,
    out: &mut Vec<WsParsedAction>,
) {
    let unwrapped = unwrap_json_value(node);
    let mut found_bucket = false;
    for (key, bucket_group) in [
        ("msgs", in_group_bucket),
        ("strangerMsgs", false),
        ("strangers", false),
        ("otherMsgs", false),
        ("lastMsgs", in_group_bucket),
        ("groupMsgs", true),
    ] {
        if let Some(raw) = unwrapped.get(key) {
            let bucket_val = unwrap_json_value(raw);
            if let Some(arr) = bucket_val.as_array() {
                found_bucket = true;
                for item in arr {
                    push_single_item(item, bucket_group, my_uid, send2me_id, out);
                }
            }
        }
    }
    if depth > 0 {
        if let Some(inner_data) = unwrapped.get("data") {
            let inner_unwrapped = unwrap_json_value(inner_data);
            let is_single_msg = unwrapped.get("msgId").is_some()
                || unwrapped.get("cliMsgId").is_some()
                || unwrapped.get("content").is_some();
            if !is_single_msg && (inner_unwrapped.is_object() || inner_unwrapped.is_array()) {
                let before = out.len();
                collect_messages_from_node_inner(
                    &inner_unwrapped,
                    in_group_bucket,
                    my_uid,
                    send2me_id,
                    depth - 1,
                    out,
                );
                if out.len() > before {
                    found_bucket = true;
                }
            }
        }
    }
    if !found_bucket
        && unwrapped.is_object()
        && (unwrapped.get("msgId").is_some()
            || unwrapped.get("cliMsgId").is_some()
            || unwrapped.get("content").is_some())
    {
        push_single_item(&unwrapped, in_group_bucket, my_uid, send2me_id, out);
    }
    if let Some(arr) = unwrapped.as_array() {
        for item in arr {
            push_single_item(item, in_group_bucket, my_uid, send2me_id, out);
        }
    }
}

fn is_reaction_message_item(item: &Value) -> bool {
    if pick_str(item, &["msgType", "type"])
        .is_some_and(|t| t.eq_ignore_ascii_case("chat.reaction"))
    {
        return true;
    }
    if let Some(content) = item.get("content").and_then(unwrap_json_object) {
        return content.get("rMsg").is_some()
            && (content.get("rIcon").is_some() || content.get("rType").is_some());
    }
    false
}

fn push_single_item(
    item: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let owned_item;
    let effective = if item.is_string() {
        owned_item = unwrap_json_value(item);
        &owned_item
    } else {
        item
    };
    if is_reaction_message_item(effective) {
        parse_reaction_item(effective, in_group_bucket, my_uid, send2me_id, out);
        return;
    }
    if let Some((thread_id, poll_id)) =
        extract_poll_activity_update(effective, in_group_bucket, my_uid, send2me_id)
    {
        out.push(WsParsedAction::PollUpdated { thread_id, poll_id });
        return;
    }
    if let Some(msg) = parse_chat_message(effective, None, my_uid, send2me_id, in_group_bucket) {
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

fn unwrap_reaction_container(node: &Value) -> Value {
    let mut cur = unwrap_json_value(node);
    for _ in 0..3 {
        if cur.get("reacts").is_some()
            || cur.get("reactGroups").is_some()
            || cur.get("rMsg").is_some()
            || cur.get("rIcon").is_some()
            || cur.get("content").is_some()
            || cur.is_array()
        {
            break;
        }
        match cur.get("data") {
            Some(inner) => {
                let unwrapped = unwrap_json_value(inner);
                if unwrapped.is_object() || unwrapped.is_array() {
                    cur = unwrapped;
                } else {
                    break;
                }
            }
            None => break,
        }
    }
    cur
}

fn pick_raw_icon_str(v: &Value) -> Option<String> {
    for k in ["rIcon", "icon", "emoji"] {
        if let Some(val) = v.get(k) {
            if let Some(s) = val.as_str() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

pub(crate) fn parse_reaction_item(
    item: &Value,
    in_group_bucket: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let is_group = in_group_bucket
        || is_group_flag(item)
        || item.get("grid").is_some()
        || item.get("groupId").is_some();

    let content_obj = item.get("content").and_then(unwrap_json_object);
    let src = content_obj.as_ref().unwrap_or(item);

    let uid_from = pick_str(item, &["uidFrom", "fromUid", "fromId", "senderId"])
        .or_else(|| {
            content_obj
                .as_ref()
                .and_then(|c| pick_str(c, &["uidFrom", "fromUid", "fromId"]))
        })
        .map(|s| clean_bare_uid(&s))
        .unwrap_or_default();
    let is_self = has_truthy_self_flag(item) || is_self_reaction_uid(&uid_from, my_uid);
    let actor_id = if is_self {
        if !my_uid.is_empty() {
            Some(my_uid.to_string())
        } else {
            Some("0".to_string())
        }
    } else if !uid_from.is_empty() && uid_from != "0" {
        Some(uid_from.clone())
    } else {
        None
    };

    let id_to = pick_str(item, &["idTo", "toid", "toId"])
        .map(|s| clean_bare_uid(&s))
        .unwrap_or_default();

    let thread_id = if is_group {
        pick_str(
            item,
            &["grid", "groupId", "idTo", "toid", "toId", "threadId", "conversId"],
        )
        .filter(|s| s != "0")
        .map(|s| clean_group_id(&s).to_string())
    } else if !send2me_id.is_empty()
        && (id_to == send2me_id
            || uid_from == send2me_id
            || (is_self && (id_to.is_empty() || id_to == "0" || id_to == my_uid)))
    {
        Some(send2me_id.to_string())
    } else if is_self {
        if !id_to.is_empty() && id_to != "0" && (my_uid.is_empty() || id_to != my_uid) {
            Some(id_to.clone())
        } else {
            extract_thread_id(item, false, my_uid, send2me_id).map(|(t, _)| t)
        }
    } else if !uid_from.is_empty() && uid_from != "0" && (my_uid.is_empty() || uid_from != my_uid) {
        Some(uid_from.clone())
    } else if !id_to.is_empty() && id_to != "0" && (my_uid.is_empty() || id_to != my_uid) {
        Some(id_to.clone())
    } else {
        extract_thread_id(item, false, my_uid, send2me_id).map(|(t, _)| t)
    };

    let Some(tid) = thread_id.filter(|t| !t.is_empty() && t != "0") else {
        return;
    };

    let r_type = src
        .get("rType")
        .or_else(|| item.get("rType"))
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
    let raw_icon = pick_raw_icon_str(src).or_else(|| pick_raw_icon_str(item));
    let is_remove_icon = raw_icon.as_deref().is_some_and(|s| {
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "" | "/-none" | "/-remove" | "none"
        )
    });
    let removed = r_type == Some(-1) || is_remove_icon;
    let emoji = if removed {
        None
    } else {
        reaction_to_emoji(raw_icon.as_deref().unwrap_or(""), r_type)
    };
    if !removed && emoji.is_none() {
        return;
    }

    if let Some(raw_r_msg) = src.get("rMsg").or_else(|| item.get("rMsg")) {
        let unwrapped_r_msg = unwrap_json_value(raw_r_msg);
        let entries: Vec<&Value> = match &unwrapped_r_msg {
            Value::Array(arr) => arr.iter().collect(),
            Value::Object(_) => vec![&unwrapped_r_msg],
            _ => Vec::new(),
        };
        for entry in entries {
            let g_id =
                pick_str(entry, &["gMsgID", "gMsgId", "globalMsgId", "msgId"]).filter(|s| s != "0");
            let c_id = pick_str(entry, &["cMsgID", "cMsgId", "cliMsgId", "clientMsgId"])
                .filter(|s| s != "0");
            let Some(primary_id) = g_id.or_else(|| c_id.clone()) else {
                continue;
            };
            out.push(WsParsedAction::Reaction {
                thread_id: tid.clone(),
                msg_id: primary_id,
                cli_msg_id: c_id,
                actor_id: actor_id.clone(),
                emoji: emoji.clone(),
                me: is_self,
                removed,
            });
        }
        return;
    }

    // Flat legacy control reaction item without `rMsg` array
    let g_id = pick_str(src, &["globalMsgId", "gMsgID", "gMsgId", "msgId"])
        .or_else(|| pick_str(item, &["globalMsgId", "gMsgID", "gMsgId", "msgId"]))
        .filter(|s| s != "0");
    let c_id = pick_str(src, &["cMsgID", "cMsgId", "cliMsgId", "clientMsgId"])
        .or_else(|| pick_str(item, &["cMsgID", "cMsgId", "cliMsgId", "clientMsgId"]))
        .filter(|s| s != "0");
    if let Some(primary_id) = g_id.or_else(|| c_id.clone()) {
        out.push(WsParsedAction::Reaction {
            thread_id: tid,
            msg_id: primary_id,
            cli_msg_id: c_id,
            actor_id,
            emoji,
            me: is_self,
            removed,
        });
    }
}

pub(crate) fn collect_reactions_from_node(
    node: &Value,
    default_group: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let effective = unwrap_reaction_container(node);
    let mut handled_bucket = false;

    if let Some(raw_reacts) = effective.get("reacts") {
        let unwrapped = unwrap_json_value(raw_reacts);
        if let Some(arr) = unwrapped.as_array() {
            handled_bucket = true;
            for item in arr {
                parse_reaction_item(item, false, my_uid, send2me_id, out);
            }
        }
    }
    if let Some(raw_groups) = effective.get("reactGroups") {
        let unwrapped = unwrap_json_value(raw_groups);
        if let Some(arr) = unwrapped.as_array() {
            handled_bucket = true;
            for item in arr {
                parse_reaction_item(item, true, my_uid, send2me_id, out);
            }
        }
    }
    if handled_bucket {
        return;
    }

    if let Some(arr) = effective.as_array() {
        for item in arr {
            parse_reaction_item(item, default_group, my_uid, send2me_id, out);
        }
    } else if effective.is_object()
        && (effective.get("rMsg").is_some()
            || effective.get("rIcon").is_some()
            || is_reaction_message_item(&effective))
    {
        parse_reaction_item(&effective, default_group, my_uid, send2me_id, out);
    }
}

fn has_controls_array(v: &Value) -> bool {
    v.get("controls").and_then(Value::as_array).is_some()
        || v.get("actions").and_then(Value::as_array).is_some()
}

fn value_wraps_controls(v: &Value) -> bool {
    has_controls_array(v)
        || v.is_array()
        || v.get("data")
            .is_some_and(|inner| has_controls_array(inner) || inner.is_array())
}

/// Unwrap outer `{ "data": ... }` transport envelopes around control lists without stripping
/// the `data` field of an actual control item (`act_type` / string `type` / `content` / `controls`).
fn unwrap_control_container(mut node: &Value) -> &Value {
    for _ in 0..3 {
        if has_controls_array(node)
            || node.get("act_type").is_some()
            || node.get("content").is_some()
        {
            break;
        }
        let data_wraps_controls = node.get("data").is_some_and(value_wraps_controls);
        let is_direct_typed_control = node
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
            && !data_wraps_controls;
        if is_direct_typed_control {
            break;
        }
        match node.get("data") {
            Some(inner) if inner.is_object() || inner.is_array() => node = inner,
            _ => break,
        }
    }
    node
}

/// Parse a single control entry into a [`VoipControlEvent`] when `act_type == "voip"`.
///
/// Accepts top-level `{ act_type: "voip", act, data }`, outer `{ act_type: "voip", content: { act, data } }`,
/// and Zalo's wrapped `{ content: { act_type: "voip", act, data } }` (including JSON-stringified `content` or `data`).
pub fn parse_voip_control_item(item: &Value) -> Option<VoipControlEvent> {
    let content_obj = item.get("content").and_then(unwrap_json_object);
    let (src, fallback) = if let Some(ref c) = content_obj {
        if pick_str(c, &["act_type", "type"]).as_deref() == Some("voip") {
            (c, Some(item))
        } else if pick_str(item, &["act_type", "type"]).as_deref() == Some("voip") {
            (item, Some(c))
        } else {
            return None;
        }
    } else if pick_str(item, &["act_type", "type"]).as_deref() == Some("voip") {
        (item, None)
    } else {
        return None;
    };

    let act = pick_str(src, &["act", "action"])
        .or_else(|| fallback.and_then(|f| pick_str(f, &["act", "action"])))
        .unwrap_or_default();
    let raw_data = src
        .get("data")
        .or_else(|| fallback.and_then(|f| f.get("data").or(Some(f))))
        .unwrap_or(&Value::Null);
    let data = match raw_data {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                serde_json::from_str::<Value>(trimmed).unwrap_or_else(|_| raw_data.clone())
            } else {
                raw_data.clone()
            }
        }
        other => other.clone(),
    };
    Some(VoipControlEvent { act, data })
}

/// Extract all `act_type == "voip"` control events from a WebSocket or `lp_control` payload node.
pub fn extract_voip_controls(node: &Value) -> Vec<VoipControlEvent> {
    let mut items: Vec<&Value> = Vec::new();
    let inner = unwrap_control_container(node);
    if let Some(arr) = inner.get("controls").and_then(Value::as_array) {
        items.extend(arr.iter());
    } else if let Some(arr) = inner.as_array() {
        items.extend(arr.iter());
    } else if inner.is_object() {
        items.push(inner);
    }
    items
        .into_iter()
        .filter_map(parse_voip_control_item)
        .collect()
}

fn is_non_empty_json_container(v: &Value) -> bool {
    match v {
        Value::Object(m) => !m.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => false,
    }
}

fn unwrap_param_if_batch(node: Value) -> Value {
    if let Some(raw_param) = node.get("param") {
        let unwrapped = unwrap_json_value(raw_param);
        if unwrapped.is_object()
            && (unwrapped.get("convsUser").is_some()
                || unwrapped.get("convsGroup").is_some()
                || unwrapped.get("convs_user").is_some()
                || unwrapped.get("convs_group").is_some()
                || unwrapped.get("conversations").is_some()
                || unwrapped.get("convs").is_some())
        {
            return unwrapped;
        }
    }
    node
}

fn extract_control_act_and_data(item: &Value) -> (String, String, Value) {
    let content_obj = item.get("content").and_then(unwrap_json_object);
    let act_type = pick_str_only(item, &["act_type"])
        .or_else(|| content_obj.as_ref().and_then(|c| pick_str_only(c, &["act_type"])))
        .or_else(|| pick_str_only(item, &["type"]))
        .or_else(|| content_obj.as_ref().and_then(|c| pick_str_only(c, &["type"])))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let sub_act = content_obj
        .as_ref()
        .and_then(|c| pick_str_only(c, &["act", "action"]))
        .or_else(|| pick_str_only(item, &["act", "action"]))
        .unwrap_or_default()
        .to_ascii_lowercase();

    let raw_data = if let Some(ref c) = content_obj {
        c.get("data")
            .map(unwrap_json_value)
            .filter(is_non_empty_json_container)
            .or_else(|| {
                c.get("param")
                    .map(unwrap_json_value)
                    .filter(is_non_empty_json_container)
            })
            .unwrap_or_else(|| c.clone())
    } else {
        item.get("data")
            .map(unwrap_json_value)
            .filter(is_non_empty_json_container)
            .or_else(|| {
                item.get("param")
                    .map(unwrap_json_value)
                    .filter(is_non_empty_json_container)
            })
            .unwrap_or_else(|| item.clone())
    };
    let data_node = unwrap_param_if_batch(raw_data);
    (act_type, sub_act, data_node)
}

fn extract_id_list_from_field(
    node: &Value,
    keys: &[&str],
    is_group: bool,
    my_uid: &str,
    out: &mut Vec<String>,
) -> bool {
    let mut found_key = false;
    for key in keys {
        let Some(raw) = node.get(*key) else {
            continue;
        };
        found_key = true;
        let unwrapped = unwrap_json_value(raw);
        let Some(arr) = unwrapped.as_array() else {
            continue;
        };
        for entry in arr {
            let raw_id = match entry {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                Value::Object(_) => {
                    if is_group {
                        pick_str(
                            entry,
                            &["id", "grid", "groupId", "threadId", "conversId", "toid", "idTo"],
                        )
                    } else {
                        pick_str(
                            entry,
                            &["id", "uid", "userId", "threadId", "conversId", "toid", "idTo"],
                        )
                    }
                }
                _ => None,
            };
            if let Some(id_str) = raw_id {
                let cleaned = if is_group
                    || id_str.starts_with("group_")
                    || id_str.starts_with('g')
                    || id_str.starts_with('G')
                {
                    clean_group_id(&id_str).to_string()
                } else {
                    id_str.trim().to_string()
                };
                if !cleaned.is_empty()
                    && cleaned != "0"
                    && (is_group || my_uid.is_empty() || !is_self_uid(&cleaned, my_uid))
                    && !out.contains(&cleaned)
                {
                    out.push(cleaned);
                }
            }
        }
    }
    found_key
}

fn extract_batch_thread_ids(node: &Value, my_uid: &str) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    let found_users =
        extract_id_list_from_field(node, &["convsUser", "convs_user"], false, my_uid, &mut ids);
    let found_groups =
        extract_id_list_from_field(node, &["convsGroup", "convs_group"], true, my_uid, &mut ids);
    if found_users || found_groups {
        return Some(ids);
    }
    let found_generic =
        extract_id_list_from_field(node, &["conversations", "convs"], false, my_uid, &mut ids);
    if found_generic {
        Some(ids)
    } else {
        None
    }
}

fn emit_read_control_actions(
    data_node: &Value,
    outer_item: &Value,
    is_unread: bool,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    if !is_unread
        && (data_node.get("clearUnreads").is_some() || data_node.get("clear_unreads").is_some())
    {
        let is_group = is_group_flag(data_node) || is_group_flag(outer_item);
        collect_clear_unreads_from_node(data_node, is_group, my_uid, send2me_id, out);
        return;
    }
    if let Some(batch_ids) = extract_batch_thread_ids(data_node, my_uid) {
        for thread_id in batch_ids {
            out.push(WsParsedAction::Read {
                thread_id,
                is_unread,
                last_msg_id: None,
            });
        }
        return;
    }
    let is_group = is_group_flag(data_node) || is_group_flag(outer_item);
    if let Some((tid, _)) = extract_thread_id(data_node, is_group, my_uid, send2me_id) {
        out.push(WsParsedAction::Read {
            thread_id: tid,
            is_unread,
            last_msg_id: if is_unread {
                None
            } else {
                pick_receipt_last_msg_id(data_node)
            },
        });
    }
}

fn clean_bare_uid(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    while s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        match serde_json::from_str::<String>(&s) {
            Ok(unwrapped) => s = unwrapped.trim().to_string(),
            Err(_) => {
                s = s.trim_matches('"').trim().to_string();
                break;
            }
        }
    }
    s.split('_').next().unwrap_or(&s).trim().to_string()
}

fn extract_social_peer_uid(data_node: &Value, my_uid: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    match data_node {
        Value::String(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                let cleaned = clean_bare_uid(trimmed);
                if !cleaned.is_empty()
                    && cleaned != "0"
                    && !cleaned.starts_with('{')
                    && !cleaned.starts_with('[')
                {
                    candidates.push(cleaned);
                }
            }
        }
        Value::Number(n) => {
            candidates.push(n.to_string());
        }
        Value::Object(_) => {
            for key in [
                "fromUid", "toUid", "fid", "friendId", "userId", "uid", "id", "data",
            ] {
                if let Some(raw) = pick_str(data_node, &[key]) {
                    let trimmed = raw.trim();
                    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                        let cleaned = clean_bare_uid(trimmed);
                        if !cleaned.is_empty()
                            && cleaned != "0"
                            && !cleaned.starts_with('{')
                            && !cleaned.starts_with('[')
                            && !candidates.contains(&cleaned)
                        {
                            candidates.push(cleaned);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    candidates
        .iter()
        .find(|u| !u.is_empty() && *u != "0" && (my_uid.is_empty() || !is_self_uid(u, my_uid)))
        .cloned()
        .or_else(|| candidates.into_iter().find(|u| !u.is_empty() && u != "0"))
}

fn extract_update_member_ids(data_node: &Value) -> Vec<String> {
    let Some(raw) = data_node
        .get("updateMembers")
        .or_else(|| data_node.get("update_members"))
    else {
        return Vec::new();
    };
    let unwrapped = unwrap_json_value(raw);
    let Some(arr) = unwrapped.as_array() else {
        return Vec::new();
    };
    let mut ids = Vec::with_capacity(arr.len());
    for entry in arr {
        let raw_id = match entry {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            Value::Object(_) => pick_str(entry, &["id", "uid", "userId", "memberId"]),
            _ => None,
        };
        if let Some(id_str) = raw_id {
            let cleaned = clean_bare_uid(&id_str);
            if !cleaned.is_empty() && cleaned != "0" && !ids.contains(&cleaned) {
                ids.push(cleaned);
            }
        }
    }
    ids
}

fn parse_poll_control_event(
    act_type: &str,
    sub_act: &str,
    data_node: &Value,
    outer_item: &Value,
) -> Option<WsParsedAction> {
    let resolved_sub = if !sub_act.is_empty() {
        sub_act.to_string()
    } else {
        data_node
            .get("act")
            .or_else(|| data_node.get("action"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default()
    };

    let is_poll_event = (act_type == "group" && resolved_sub.contains("poll"))
        || matches!(
            act_type,
            "poll" | "group_poll" | "new_poll" | "vote_poll" | "end_poll" | "update_poll"
        )
        || (matches!(act_type, "groupboard" | "group_board" | "board")
            && resolved_sub.contains("poll"));
    if !is_poll_event {
        return None;
    }

    let mut nodes = collect_poll_nodes(data_node);
    if let Some(topic) = data_node.get("topic").and_then(unwrap_json_object) {
        let mut topic_nodes = collect_poll_nodes(&topic);
        topic_nodes.extend(nodes);
        nodes = topic_nodes;
    }

    let outer_content = outer_item.get("content").and_then(unwrap_json_object);
    let gid_keys = [
        "groupId",
        "grid",
        "group_id",
        "idTo",
        "toid",
        "toId",
        "threadId",
        "conversId",
    ];
    let raw_gid = nodes
        .iter()
        .find_map(|n| pick_str(n, &gid_keys))
        .or_else(|| outer_content.as_ref().and_then(|c| pick_str(c, &gid_keys)))
        .or_else(|| pick_str(outer_item, &gid_keys))?;
    let thread_id = clean_group_id(&raw_gid).to_string();
    if thread_id.is_empty() || thread_id == "0" {
        return None;
    }

    let poll_id = ["poll_id", "pollId"]
        .iter()
        .find_map(|key| {
            nodes
                .iter()
                .chain(std::iter::once(outer_item))
                .find_map(|n| n.get(*key).and_then(parse_json_i64))
                .filter(|&id| id > 0)
        })
        .or_else(|| {
            ["topicId", "topic_id", "id"].iter().find_map(|key| {
                nodes
                    .iter()
                    .find_map(|n| n.get(*key).and_then(parse_json_i64))
                    .filter(|&id| id > 0 && id.to_string() != thread_id)
            })
        });

    Some(WsParsedAction::PollUpdated { thread_id, poll_id })
}

fn parse_social_control_event(
    act_type: &str,
    sub_act: &str,
    data_node: &Value,
    my_uid: &str,
) -> Option<WsParsedAction> {
    let resolved_sub = if !sub_act.is_empty() {
        sub_act.to_string()
    } else {
        data_node
            .get("act")
            .or_else(|| data_node.get("action"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default()
    };

    if act_type == "fr" {
        // Note: intentionally skip duplicate `"req"` (only emit for `"req_v2"`).
        let kind = match resolved_sub.as_str() {
            "add" => "friend_added",
            "remove" => "friend_removed",
            "block" => "user_blocked",
            "unblock" => "user_unblocked",
            "req_v2" => "friend_request_received",
            "reject" => "friend_request_rejected",
            "undo_req" => "friend_request_undone",
            _ => return None,
        };
        let user_id = extract_social_peer_uid(data_node, my_uid);
        return Some(WsParsedAction::SocialEvent {
            kind: kind.to_string(),
            thread_id: None,
            user_id,
            self_removed: false,
        });
    }

    if act_type == "group" {
        let kind = match resolved_sub.as_str() {
            "leave" => "group_left",
            "remove_member" | "block_member" => "group_member_removed",
            "join" => "group_member_joined",
            "add_admin" | "remove_admin" => "group_admin_changed",
            _ => return None,
        };
        let raw_gid = pick_str(data_node, &["groupId", "grid", "group_id", "id"])?;
        let gid = clean_group_id(&raw_gid).to_string();
        if gid.is_empty() || gid == "0" {
            return None;
        }
        let updated_members = extract_update_member_ids(data_node);
        let is_removal = matches!(
            resolved_sub.as_str(),
            "leave" | "remove_member" | "block_member"
        );
        let self_removed = is_removal
            && !my_uid.is_empty()
            && updated_members.iter().any(|u| is_self_uid(u, my_uid));
        let user_id = updated_members
            .first()
            .cloned()
            .or_else(|| {
                pick_str(data_node, &["sourceId", "actorId", "uid", "userId"])
                    .map(|s| clean_bare_uid(&s))
                    .filter(|s| !s.is_empty() && s != "0")
            });
        return Some(WsParsedAction::SocialEvent {
            kind: kind.to_string(),
            thread_id: Some(gid),
            user_id,
            self_removed,
        });
    }

    None
}

fn collect_controls_from_node(
    node: &Value,
    my_uid: &str,
    send2me_id: &str,
    out: &mut Vec<WsParsedAction>,
) {
    let effective = unwrap_control_container(node);
    let mut items: Vec<&Value> = Vec::new();
    let mut matched_bucket = false;
    if let Some(arr) = effective.get("controls").and_then(Value::as_array) {
        matched_bucket = true;
        items.extend(arr.iter());
    }
    if let Some(arr) = effective.get("actions").and_then(Value::as_array) {
        matched_bucket = true;
        items.extend(arr.iter());
    }
    if !matched_bucket {
        if let Some(arr) = effective.as_array() {
            items.extend(arr.iter());
        } else if effective.is_object() {
            items.push(effective);
        }
    }

    for item in items {
        if let Some(voip_ev) = parse_voip_control_item(item) {
            out.push(WsParsedAction::Voip(voip_ev));
            continue;
        }
        let (act_type, sub_act, data_node) = extract_control_act_and_data(item);
        if let Some(poll_action) =
            parse_poll_control_event(&act_type, &sub_act, &data_node, item)
        {
            out.push(poll_action);
            continue;
        }
        if matches!(act_type.as_str(), "fr" | "group") {
            if let Some(social_action) =
                parse_social_control_event(&act_type, &sub_act, &data_node, my_uid)
            {
                out.push(social_action);
            }
            continue;
        }
        let effective_act = if matches!(act_type.as_str(), "unread_mark" | "convers" | "conv")
            && !sub_act.is_empty()
        {
            match sub_act.as_str() {
                "remove"
                | "remove_unread_mark"
                | "remove-unread-mark"
                | "removeunreadmark"
                | "read"
                | "mark_read"
                | "clear_unread"
                | "clearunread"
                | "clear_unreads" => "remove_unread_mark",
                "add"
                | "add_unread_mark"
                | "add-unread-mark"
                | "addunreadmark"
                | "unread"
                | "mark_unread" => "add_unread_mark",
                other => other,
            }
        } else {
            act_type.as_str()
        };

        if effective_act == "undo_msg" || effective_act == "undo" {
            if let (Some((tid, _)), Some(mid)) = (
                extract_thread_id(&data_node, false, my_uid, send2me_id),
                pick_str(&data_node, &["globalMsgId", "msgId", "cliMsgId"]),
            ) {
                out.push(WsParsedAction::Recalled {
                    thread_id: tid,
                    msg_id: mid,
                });
            }
        } else if effective_act == "reaction" {
            let is_group = is_group_flag(&data_node) || is_group_flag(item);
            parse_reaction_item(&data_node, is_group, my_uid, send2me_id, out);
        } else if matches!(effective_act, "seen_msg" | "read_receipt") {
            let is_group = is_group_flag(&data_node) || is_group_flag(item);
            if data_node.get("clearUnreads").is_some() || data_node.get("clear_unreads").is_some() {
                collect_clear_unreads_from_node(&data_node, is_group, my_uid, send2me_id, out);
            } else if has_receipt_bucket(&data_node) {
                collect_receipts_from_node(&data_node, is_group, true, my_uid, send2me_id, out);
            } else if extract_seen_uids(&data_node).is_some() {
                parse_receipt_item(&data_node, is_group, true, my_uid, send2me_id, out);
            } else {
                let uid_from =
                    pick_str(&data_node, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
                let is_self_flag =
                    has_truthy_self_flag(&data_node) || has_truthy_self_flag(item);
                let is_peer = !is_self_flag
                    && !uid_from.is_empty()
                    && uid_from != "0"
                    && (my_uid.is_empty() || !is_self_uid(&uid_from, my_uid));
                if is_peer {
                    parse_receipt_item(&data_node, is_group, true, my_uid, send2me_id, out);
                } else {
                    emit_read_control_actions(&data_node, item, false, my_uid, send2me_id, out);
                }
            }
        } else if matches!(
            effective_act,
            "seen"
                | "read"
                | "read_msg"
                | "mark_read"
                | "mark_as_read"
                | "clear_unread"
                | "clearunread"
                | "clear_unreads"
                | "remove_unread_mark"
                | "remove-unread-mark"
                | "removeunreadmark"
                | "unread"
                | "add_unread_mark"
                | "add-unread-mark"
                | "addunreadmark"
                | "mark_unread"
                | "mark_as_unread"
        ) {
            let is_unread = matches!(
                effective_act,
                "unread"
                    | "add_unread_mark"
                    | "add-unread-mark"
                    | "addunreadmark"
                    | "mark_unread"
                    | "mark_as_unread"
            );
            let is_group = is_group_flag(&data_node) || is_group_flag(item);
            if effective_act == "seen" && has_receipt_bucket(&data_node) {
                collect_receipts_from_node(&data_node, is_group, true, my_uid, send2me_id, out);
            } else if effective_act == "seen" && extract_seen_uids(&data_node).is_some() {
                parse_receipt_item(&data_node, is_group, true, my_uid, send2me_id, out);
            } else {
                let has_explicit_receipt = effective_act == "seen"
                    && (data_node.get("seenTs").is_some()
                        || data_node.get("msgId").is_some()
                        || data_node.get("globalMsgId").is_some()
                        || data_node.get("realMsgId").is_some());
                let uid_from =
                    pick_str(&data_node, &["uidFrom", "fromUid", "fromId"]).unwrap_or_default();
                let is_self_flag =
                    has_truthy_self_flag(&data_node) || has_truthy_self_flag(item);
                let is_peer = !is_self_flag
                    && !uid_from.is_empty()
                    && uid_from != "0"
                    && (my_uid.is_empty() || !is_self_uid(&uid_from, my_uid));
                if has_explicit_receipt && is_peer {
                    parse_receipt_item(&data_node, is_group, true, my_uid, send2me_id, out);
                } else {
                    emit_read_control_actions(
                        &data_node,
                        item,
                        is_unread,
                        my_uid,
                        send2me_id,
                        out,
                    );
                }
            }
        } else if data_node.get("clearUnreads").is_some() || data_node.get("clear_unreads").is_some()
        {
            let is_group = is_group_flag(&data_node) || is_group_flag(item);
            collect_clear_unreads_from_node(&data_node, is_group, my_uid, send2me_id, out);
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
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "999" && !*is_unread
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888" && *is_unread
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
        assert_eq!(a2.len(), 3);
        assert!(matches!(
            &a2[0],
            WsParsedAction::Read { thread_id, is_unread, .. }
                if thread_id == "888111" && !*is_unread
        ));
        assert!(matches!(
            &a2[1],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "888111" && *seen_ts == 1_700_000_090_000 && !*is_delivered_only
        ));
        assert!(matches!(
            &a2[2],
            WsParsedAction::Read { thread_id, is_unread, .. }
                if thread_id == "888222" && !*is_unread
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

    #[test]
    fn parses_ws_voip_control_events_across_wrapped_and_direct_shapes() {
        let ev_push = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        {
                            "content": {
                                "act_type": "voip",
                                "act": "request",
                                "data": r#"{"callId":464564007,"uidN":"1000000000000000002","params":"{\"sessId\":\"abc\",\"rtpIP\":\"42.113.206.4:10490\"}"}"#
                            }
                        },
                        {
                            "content": "{\"act_type\":\"voip\",\"act\":\"ring_ring\",\"data\":{\"callId\":464564007,\"status\":0}}"
                        }
                    ]
                }
            }),
        };
        let a1 = parse_ws_event(&ev_push, "456", "");
        assert_eq!(a1.len(), 2);
        match &a1[0] {
            WsParsedAction::Voip(v) => {
                assert_eq!(v.act, "request");
                assert_eq!(v.call_id(), 464564007);
                let mp = v.media_params();
                assert_eq!(mp["sessId"], "abc");
                assert_eq!(mp["rtpIP"], "42.113.206.4:10490");
            }
            other => panic!("expected Voip, got {other:?}"),
        }
        match &a1[1] {
            WsParsedAction::Voip(v) => {
                assert_eq!(v.act, "ring_ring");
                assert_eq!(v.call_id(), 464564007);
                assert_eq!(v.status(), Some(0));
            }
            other => panic!("expected Voip, got {other:?}"),
        }

        let ev_pull_voip = WsEvent {
            cmd: CMD_CTRL_PULL_VOIP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        {
                            "act_type": "voip",
                            "act": "answer",
                            "data": { "callId": "464564007", "status": "0" }
                        },
                        {
                            "act_type": "voip",
                            "act": "end_call",
                            "data": { "callId": 464564007 }
                        }
                    ]
                }
            }),
        };
        let a2 = parse_ws_event(&ev_pull_voip, "456", "");
        assert_eq!(a2.len(), 2);
        assert!(matches!(
            &a2[0],
            WsParsedAction::Voip(v) if v.act == "answer" && v.call_id() == 464564007 && v.status() == Some(0)
        ));
        assert!(matches!(
            &a2[1],
            WsParsedAction::Voip(v) if v.act == "end_call" && v.call_id() == 464564007
        ));

        // Mixed batch with non-VoIP reaction + VoIP control in the same frame
        let ev_mixed = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        {
                            "act_type": "reaction",
                            "data": { "uidFrom": "999", "idTo": "456", "msgId": "m99", "rIcon": "/-heart" }
                        },
                        {
                            "content": { "act_type": "voip", "act": "cancel", "data": { "callId": 12345 } }
                        }
                    ]
                }
            }),
        };
        let a3 = parse_ws_event(&ev_mixed, "456", "");
        assert_eq!(a3.len(), 2);
        assert!(matches!(&a3[0], WsParsedAction::Reaction { msg_id, .. } if msg_id == "m99"));
        assert!(matches!(&a3[1], WsParsedAction::Voip(v) if v.act == "cancel" && v.call_id() == 12345));

        // Direct top-level `{ act_type: "voip", act, data: { ... } }` (no outer `data`/`controls` wrapper)
        let direct_payload = json!({
            "act_type": "voip",
            "act": "request",
            "data": { "callId": 888999, "uidN": "1000000000000000009" }
        });
        let direct_extracted = extract_voip_controls(&direct_payload);
        assert_eq!(direct_extracted.len(), 1);
        assert_eq!(direct_extracted[0].act, "request");
        assert_eq!(direct_extracted[0].call_id(), 888999);

        let ev_direct = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: direct_payload,
        };
        let a4 = parse_ws_event(&ev_direct, "456", "");
        assert_eq!(a4.len(), 1);
        assert!(matches!(&a4[0], WsParsedAction::Voip(v) if v.act == "request" && v.call_id() == 888999));

        // Array inside `data`, double-wrapped `data.data`, and outer `act_type` with inner `content`
        let arr_in_data = json!({
            "data": [
                { "act_type": "voip", "content": { "act": "ring_ring", "data": { "callId": 77 } } }
            ]
        });
        let extracted_arr = extract_voip_controls(&arr_in_data);
        assert_eq!(extracted_arr.len(), 1);
        assert_eq!(extracted_arr[0].act, "ring_ring");
        assert_eq!(extracted_arr[0].call_id(), 77);

        let double_wrapped = json!({
            "data": {
                "data": {
                    "controls": [
                        { "content": { "act_type": "voip", "act": "end_call", "data": { "callId": 88 } } }
                    ]
                }
            }
        });
        let extracted_dw = extract_voip_controls(&double_wrapped);
        assert_eq!(extracted_dw.len(), 1);
        assert_eq!(extracted_dw[0].act, "end_call");
        assert_eq!(extracted_dw[0].call_id(), 88);
    }

    #[test]
    fn parses_ws_controls_inside_numeric_type_envelope_without_breaking_direct_typed_controls() {
        // 1. Outer WS control envelope with numeric `"type": 1` + `"data": { "controls": [...] }`
        let ev_num_type = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "type": 1,
                "data": {
                    "controls": [
                        {
                            "act_type": "undo_msg",
                            "data": { "uidFrom": "999", "idTo": "456", "globalMsgId": "gm-num-1" }
                        },
                        {
                            "content": {
                                "act_type": "voip",
                                "act": "request",
                                "data": { "callId": 555_123, "uidN": "999" }
                            }
                        }
                    ]
                }
            }),
        };
        let a_num = parse_ws_event(&ev_num_type, "456", "");
        assert_eq!(a_num.len(), 2);
        assert!(matches!(
            &a_num[0],
            WsParsedAction::Recalled { thread_id, msg_id } if thread_id == "999" && msg_id == "gm-num-1"
        ));
        assert!(matches!(
            &a_num[1],
            WsParsedAction::Voip(v) if v.act == "request" && v.call_id() == 555_123
        ));

        let extracted_num = extract_voip_controls(&ev_num_type.payload);
        assert_eq!(extracted_num.len(), 1);
        assert_eq!(extracted_num[0].call_id(), 555_123);

        // 2. Outer envelope with string `"type"` whose `data` wraps `"controls"` must still unwrap
        let ev_str_envelope = WsEvent {
            cmd: CMD_CTRL_ACTION,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "type": "ctrl_envelope",
                "data": {
                    "controls": [
                        {
                            "act_type": "voip",
                            "act": "ring_ring",
                            "data": { "callId": 555_124 }
                        }
                    ]
                }
            }),
        };
        let a_env = parse_ws_event(&ev_str_envelope, "456", "");
        assert_eq!(a_env.len(), 1);
        assert!(matches!(
            &a_env[0],
            WsParsedAction::Voip(v) if v.act == "ring_ring" && v.call_id() == 555_124
        ));

        // 3. Direct control item using string `"type": "voip"` (whose `data` is the event payload,
        // NOT wrapping `"controls"`) must NOT be unwrapped.
        let ev_direct_type = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "type": "voip",
                "act": "answer",
                "data": { "callId": 555_125, "status": 0 }
            }),
        };
        let a_direct = parse_ws_event(&ev_direct_type, "456", "");
        assert_eq!(a_direct.len(), 1);
        assert!(matches!(
            &a_direct[0],
            WsParsedAction::Voip(v) if v.act == "answer" && v.call_id() == 555_125 && v.status() == Some(0)
        ));

        // 4. Outer numeric `"type"` with `"controls": null` and inner `"data": { "controls": [...] }`
        // and numeric `"type"` wrapping direct `"act_type": "voip"` inside `"data"`.
        let ev_null_controls = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "type": 1,
                "controls": null,
                "data": {
                    "controls": [
                        { "act_type": "voip", "act": "cancel", "data": { "callId": 555_126 } }
                    ]
                }
            }),
        };
        let a_null_ctrl = parse_ws_event(&ev_null_controls, "456", "");
        assert_eq!(a_null_ctrl.len(), 1);
        assert!(matches!(
            &a_null_ctrl[0],
            WsParsedAction::Voip(v) if v.act == "cancel" && v.call_id() == 555_126
        ));

        let ev_num_direct_inner = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "type": 1,
                "data": {
                    "act_type": "voip",
                    "act": "end_call",
                    "data": { "callId": 555_127 }
                }
            }),
        };
        let a_num_inner = parse_ws_event(&ev_num_direct_inner, "456", "");
        assert_eq!(a_num_inner.len(), 1);
        assert!(matches!(
            &a_num_inner[0],
            WsParsedAction::Voip(v) if v.act == "end_call" && v.call_id() == 555_127
        ));

        // 5. Control cmd 610 and fallback non-standard cmd carrying VoIP events are both parsed.
        let ev_610 = WsEvent {
            cmd: CMD_CTRL_PULL_610,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        { "act_type": "voip", "act": "request", "data": { "callId": 555_128 } }
                    ]
                }
            }),
        };
        let a_610 = parse_ws_event(&ev_610, "456", "");
        assert_eq!(a_610.len(), 1);
        assert!(matches!(
            &a_610[0],
            WsParsedAction::Voip(v) if v.act == "request" && v.call_id() == 555_128
        ));

        let ev_615 = WsEvent {
            cmd: 615,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        { "act_type": "voip", "act": "ring_ring", "data": { "callId": 555_129 } }
                    ]
                }
            }),
        };
        let a_615 = parse_ws_event(&ev_615, "456", "");
        assert_eq!(a_615.len(), 1);
        assert!(matches!(
            &a_615[0],
            WsParsedAction::Voip(v) if v.act == "ring_ring" && v.call_id() == 555_129
        ));
    }

    #[test]
    fn parses_call_log_messages_into_structured_teza_call_cards() {
        let voice_ok = json!({
            "msgId": "c-1",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "chat.call",
            "content": {
                "callType": 1,
                "duration": 85,
                "callStatus": "ended"
            },
            "ts": 1_700_000_000
        });
        let m1 = parse_chat_message(&voice_ok, None, "456", "", false).unwrap();
        assert_eq!(m1.msg_type, "chat.call");
        assert_eq!(m1.text, "Cuộc gọi thoại (01:25)");
        assert_eq!(
            m1.attachment_name.as_deref(),
            Some(r#"teza-call:{"callType":"voice","durationSecs":85,"outcome":"completed"}"#)
        );

        let voice_missed = json!({
            "msgId": "c-2",
            "uidFrom": "123",
            "idTo": "456",
            "msgType": "call.voice",
            "content": "{\"callId\":99,\"status\":6,\"duration\":0}",
            "ts": 1_700_000_010
        });
        let m2 = parse_chat_message(&voice_missed, None, "456", "", false).unwrap();
        assert_eq!(m2.msg_type, "chat.call");
        assert_eq!(m2.text, "Cuộc gọi thoại nhỡ");
        assert_eq!(
            m2.attachment_name.as_deref(),
            Some(r#"teza-call:{"callType":"voice","durationSecs":0,"outcome":"missed"}"#)
        );

        let video_declined = json!({
            "msgId": "c-3",
            "uidFrom": "456",
            "idTo": "123",
            "isSelf": true,
            "msgType": "chat.text",
            "content": {
                "callId": 100,
                "callType": 3,
                "reason": "rejected"
            },
            "ts": 1_700_000_020
        });
        let m3 = parse_chat_message(&video_declined, None, "456", "", false).unwrap();
        assert_eq!(m3.msg_type, "chat.call");
        assert_eq!(m3.text, "Cuộc gọi video bị từ chối");
        assert_eq!(
            m3.attachment_name.as_deref(),
            Some(r#"teza-call:{"callType":"video","durationSecs":0,"outcome":"declined"}"#)
        );

        let video_cancelled = json!({
            "msgId": "c-4",
            "uidFrom": "456",
            "idTo": "123",
            "isSelf": true,
            "msgType": "call.video",
            "content": { "params": "{\"duration\":0,\"callStatus\":\"cancelled\"}" },
            "ts": 1_700_000_030
        });
        let m4 = parse_chat_message(&video_cancelled, None, "456", "", false).unwrap();
        assert_eq!(m4.msg_type, "chat.call");
        assert_eq!(m4.text, "Đã hủy cuộc gọi video");
        assert_eq!(
            m4.attachment_name.as_deref(),
            Some(r#"teza-call:{"callType":"video","durationSecs":0,"outcome":"cancelled"}"#)
        );
    }

    #[test]
    fn parses_direct_503_self_seen_vs_peer_seen_and_ignores_self_delivered() {
        let ev_self_seen = WsEvent {
            cmd: CMD_SEEN_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "seenMsgs": [
                        { "uidFrom": "456", "idTo": "999", "seenTs": 1_700_000_100_i64 },
                        { "isSelf": true, "idTo": "888", "seenTs": 1_700_000_101_i64 },
                        { "isSelf": 1, "uidFrom": "777", "idTo": "456", "seenTs": 1_700_000_102_i64 },
                        { "uidFrom": "999", "idTo": "456", "seenTs": 1_700_000_103_i64 },
                        { "uidFrom": "456", "idTo": "456", "seenTs": 1_700_000_104_i64 }
                    ]
                }
            }),
        };
        let actions = parse_ws_event(&ev_self_seen, "456", "");
        assert_eq!(actions.len(), 4);
        assert!(matches!(
            &actions[0],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "999" && !*is_unread
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888" && !*is_unread
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "777" && !*is_unread
        ));
        assert!(matches!(
            &actions[3],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "999" && *seen_ts == 1_700_000_103_000 && !*is_delivered_only
        ));

        let ev_self_deliv = WsEvent {
            cmd: CMD_DELIVERED_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "deliverMsgs": [
                        { "uidFrom": "456", "idTo": "999", "ts": 1_700_000_200_000_i64 }
                    ]
                }
            }),
        };
        assert!(parse_ws_event(&ev_self_deliv, "456", "").is_empty());
    }

    #[test]
    fn parses_group_523_self_peer_mixed_and_normalizes_group_ids() {
        let ev_grp = WsEvent {
            cmd: CMD_SEEN_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "seenMsgs": [
                        { "grid": "g888111", "seenUids": ["456"], "ts": 1_700_000_010_000_i64 },
                        { "grid": "group_888222", "seenUids": ["777"], "ts": 1_700_000_011_000_i64 },
                        { "groupId": "G888333", "seenUids": ["456_0", "777_1"], "ts": 1_700_000_012_000_i64 },
                        { "grid": "888444", "uidFrom": "456", "ts": 1_700_000_013_000_i64 },
                        { "grid": "888555", "isSelf": true, "ts": 1_700_000_014_000_i64 },
                        { "grid": "888666", "uidFrom": "777", "ts": 1_700_000_015_000_i64 }
                    ]
                }
            }),
        };
        let actions = parse_ws_event(&ev_grp, "456", "");
        assert_eq!(actions.len(), 7);
        assert!(matches!(
            &actions[0],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888111" && !*is_unread
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "888222" && *seen_ts == 1_700_000_011_000 && !*is_delivered_only
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888333" && !*is_unread
        ));
        assert!(matches!(
            &actions[3],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "888333" && *seen_ts == 1_700_000_012_000 && !*is_delivered_only
        ));
        assert!(matches!(
            &actions[4],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888444" && !*is_unread
        ));
        assert!(matches!(
            &actions[5],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888555" && !*is_unread
        ));
        assert!(matches!(
            &actions[6],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "888666" && *seen_ts == 1_700_000_015_000 && !*is_delivered_only
        ));

        // Delivered group frame for self must NOT emit Read
        let ev_grp_deliv = WsEvent {
            cmd: CMD_DELIVERED_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "deliverMsgs": [
                        { "grid": "888777", "seenUids": ["456"], "ts": 1_700_000_016_000_i64 }
                    ]
                }
            }),
        };
        assert!(parse_ws_event(&ev_grp_deliv, "456", "").is_empty());
    }

    #[test]
    fn parses_control_seen_msg_read_receipt_stringified_content_and_batch_convs() {
        let ev_ctrl = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        {
                            "act_type": "seen_msg",
                            "data": { "uidFrom": "456", "idTo": "999" }
                        },
                        {
                            "act_type": "read_receipt",
                            "data": { "uidFrom": "0", "idTo": "888" }
                        },
                        {
                            "act_type": "seen_msg",
                            "data": { "uidFrom": "999", "idTo": "456", "seenTs": 1_700_000_300_i64 }
                        },
                        {
                            "content": "{\"act_type\":\"seen_msg\",\"data\":\"{\\\"grid\\\":\\\"g555111\\\",\\\"seenUids\\\":[\\\"456\\\",\\\"777\\\"],\\\"ts\\\":1700000400000}\"}"
                        },
                        {
                            "content": "{\"act_type\":\"remove_unread_mark\",\"data\":\"{\\\"convsUser\\\":[{\\\"id\\\":\\\"101\\\",\\\"ts\\\":\\\"1700\\\"},\\\"102\\\"],\\\"convsGroup\\\":[{\\\"id\\\":\\\"g601\\\"},\\\"group_602\\\"]}\"}"
                        },
                        {
                            "type": 1,
                            "content": {
                                "act_type": "add_unread_mark",
                                "data": {
                                    "param": "{\"convsUser\":[{\"id\":\"201\"}],\"convsGroup\":[{\"id\":\"701\"}]}"
                                }
                            }
                        },
                        {
                            "act_type": "seen",
                            "data": { "grid": "9001", "seenUids": ["777"], "ts": 1_700_000_500_000_i64 }
                        }
                    ]
                }
            }),
        };

        let actions = parse_ws_event(&ev_ctrl, "456", "");
        assert_eq!(actions.len(), 12);
        assert!(matches!(
            &actions[0],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "999" && !*is_unread
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "888" && !*is_unread
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "999" && *seen_ts == 1_700_000_300_000 && !*is_delivered_only
        ));
        assert!(matches!(
            &actions[3],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "555111" && !*is_unread
        ));
        assert!(matches!(
            &actions[4],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "555111" && *seen_ts == 1_700_000_400_000 && !*is_delivered_only
        ));
        assert!(matches!(
            &actions[5],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "101" && !*is_unread
        ));
        assert!(matches!(
            &actions[6],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "102" && !*is_unread
        ));
        assert!(matches!(
            &actions[7],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "601" && !*is_unread
        ));
        assert!(matches!(
            &actions[8],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "602" && !*is_unread
        ));
        assert!(matches!(
            &actions[9],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "201" && *is_unread
        ));
        assert!(matches!(
            &actions[10],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "701" && *is_unread
        ));
        assert!(matches!(
            &actions[11],
            WsParsedAction::PeerSeen { thread_id, seen_ts, is_delivered_only }
                if thread_id == "9001" && *seen_ts == 1_700_000_500_000 && !*is_delivered_only
        ));
    }

    #[test]
    fn parses_ws_601_friend_social_events_and_skips_duplicate_req() {
        let ev = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "controls": [
                        { "content": { "act_type": "fr", "act": "add", "data": "\"1001\"" } },
                        { "content": { "act_type": "fr", "act": "remove", "data": "\"1002_0\"" } },
                        { "content": { "act_type": "fr", "act": "block", "data": "1003" } },
                        { "content": { "act_type": "fr", "act": "unblock", "data": "1003" } },
                        { "content": { "act_type": "fr", "act": "req", "data": "{\"fromUid\":\"1004\",\"toUid\":\"999\"}" } },
                        { "content": { "act_type": "fr", "act": "req_v2", "data": "{\"fromUid\":\"1004\",\"toUid\":\"999\",\"src\":30,\"message\":\"Hi\"}" } },
                        { "content": { "act_type": "fr", "act": "reject", "data": "{\"fromUid\":\"999\",\"toUid\":\"1005\"}" } },
                        { "content": { "act_type": "fr", "act": "undo_req", "data": "{\"fromUid\":\"1006\",\"toUid\":\"999\"}" } }
                    ]
                }
            }),
        };
        let actions = parse_ws_event(&ev, "999", "");
        assert_eq!(actions.len(), 7);
        assert!(matches!(
            &actions[0],
            WsParsedAction::SocialEvent { kind, thread_id, user_id, self_removed }
                if kind == "friend_added" && thread_id.is_none() && user_id.as_deref() == Some("1001") && !*self_removed
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "friend_removed" && user_id.as_deref() == Some("1002")
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "user_blocked" && user_id.as_deref() == Some("1003")
        ));
        assert!(matches!(
            &actions[3],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "user_unblocked" && user_id.as_deref() == Some("1003")
        ));
        // Duplicate "req" was skipped, only "req_v2" emitted
        assert!(matches!(
            &actions[4],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "friend_request_received" && user_id.as_deref() == Some("1004")
        ));
        assert!(matches!(
            &actions[5],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "friend_request_rejected" && user_id.as_deref() == Some("1005")
        ));
        assert!(matches!(
            &actions[6],
            WsParsedAction::SocialEvent { kind, user_id, .. }
                if kind == "friend_request_undone" && user_id.as_deref() == Some("1006")
        ));
    }

    #[test]
    fn parses_ws_601_group_social_events_with_self_removed_detection() {
        let ev = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "controls": [
                    {
                        "act_type": "group",
                        "content": {
                            "act": "leave",
                            "data": "{\"groupId\":\"g888111\",\"creatorId\":\"100\",\"sourceId\":\"999\",\"updateMembers\":[{\"id\":\"999\",\"dName\":\"Me\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "leave",
                            "data": "{\"groupId\":\"g888111\",\"creatorId\":\"100\",\"sourceId\":\"200\",\"updateMembers\":[{\"id\":\"200\",\"dName\":\"Other\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "remove_member",
                            "data": "{\"groupId\":\"group_888222\",\"creatorId\":\"100\",\"sourceId\":\"100\",\"updateMembers\":[{\"id\":\"999_0\",\"dName\":\"Me\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "remove_member",
                            "data": "{\"groupId\":\"888222\",\"creatorId\":\"999\",\"sourceId\":\"999\",\"updateMembers\":[{\"id\":\"300\",\"dName\":\"Kicked\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "block_member",
                            "data": "{\"groupId\":\"888333\",\"sourceId\":\"100\",\"updateMembers\":[{\"id\":\"999\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "join",
                            "data": "{\"groupId\":\"888444\",\"sourceId\":\"100\",\"updateMembers\":[{\"id\":\"999\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "add_admin",
                            "data": "{\"groupId\":\"888555\",\"sourceId\":\"100\",\"updateMembers\":[{\"id\":\"200\"}]}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "remove_admin",
                            "data": "{\"groupId\":\"888555\",\"sourceId\":\"100\",\"updateMembers\":[{\"id\":\"200\"}]}"
                        }
                    }
                ]
            }),
        };

        let actions = parse_ws_event(&ev, "999", "");
        assert_eq!(actions.len(), 8);

        assert!(matches!(
            &actions[0],
            WsParsedAction::SocialEvent { kind, thread_id, user_id, self_removed }
                if kind == "group_left" && thread_id.as_deref() == Some("888111") && user_id.as_deref() == Some("999") && *self_removed
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::SocialEvent { kind, thread_id, user_id, self_removed }
                if kind == "group_left" && thread_id.as_deref() == Some("888111") && user_id.as_deref() == Some("200") && !*self_removed
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::SocialEvent { kind, thread_id, user_id, self_removed }
                if kind == "group_member_removed" && thread_id.as_deref() == Some("888222") && user_id.as_deref() == Some("999") && *self_removed
        ));
        // When self (`999`) kicks member `300`, `sourceId == "999"` must NOT trigger `self_removed`!
        assert!(matches!(
            &actions[3],
            WsParsedAction::SocialEvent { kind, thread_id, user_id, self_removed }
                if kind == "group_member_removed" && thread_id.as_deref() == Some("888222") && user_id.as_deref() == Some("300") && !*self_removed
        ));
        assert!(matches!(
            &actions[4],
            WsParsedAction::SocialEvent { kind, thread_id, self_removed, .. }
                if kind == "group_member_removed" && thread_id.as_deref() == Some("888333") && *self_removed
        ));
        // Joining a group with self in updateMembers is NOT self_removed
        assert!(matches!(
            &actions[5],
            WsParsedAction::SocialEvent { kind, thread_id, self_removed, .. }
                if kind == "group_member_joined" && thread_id.as_deref() == Some("888444") && !*self_removed
        ));
        assert!(matches!(
            &actions[6],
            WsParsedAction::SocialEvent { kind, thread_id, self_removed, .. }
                if kind == "group_admin_changed" && thread_id.as_deref() == Some("888555") && !*self_removed
        ));
        assert!(matches!(
            &actions[7],
            WsParsedAction::SocialEvent { kind, thread_id, self_removed, .. }
                if kind == "group_admin_changed" && thread_id.as_deref() == Some("888555") && !*self_removed
        ));
    }

    #[test]
    fn parses_ws_612_live_reactions_dm_group_self_peer_unreact_and_zero_gmsgid() {
        assert!(is_reaction_ws_cmd(CMD_REACTION_PULL_1_1));
        assert!(is_reaction_ws_cmd(CMD_REACTION_PULL_GROUP));
        assert!(is_reaction_ws_cmd(CMD_REACTION_PUSH));
        assert!(!is_reaction_ws_cmd(CMD_PUSH_MSG_1_1));
        assert!(!is_reaction_ws_cmd(CMD_CTRL_PUSH_CTRL));

        let ev = WsEvent {
            cmd: CMD_REACTION_PUSH,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "reacts": [
                        {
                            "msgId": "9000001",
                            "cliMsgId": "1782286280001",
                            "msgType": "chat.reaction",
                            "uidFrom": "0",
                            "idTo": "999",
                            "content": "{\"rMsg\":[{\"gMsgID\":75001,\"cMsgID\":1782286269001,\"msgType\":1}],\"rIcon\":\"/-heart\",\"rType\":5,\"source\":6}"
                        },
                        {
                            "msgId": "9000002",
                            "cliMsgId": "1782286280002",
                            "msgType": "chat.reaction",
                            "uidFrom": "999",
                            "idTo": "0",
                            "content": "{\"rMsg\":[{\"gMsgID\":\"75002\",\"cMsgID\":\"1782286269002\",\"msgType\":1}],\"rIcon\":\":>\",\"rType\":0,\"source\":0}"
                        },
                        {
                            "msgId": "9000003",
                            "cliMsgId": "1782286280003",
                            "msgType": "chat.reaction",
                            "uidFrom": "0",
                            "idTo": "999",
                            "content": "{\"rMsg\":[{\"gMsgID\":75001,\"cMsgID\":1782286269001,\"msgType\":1}],\"rIcon\":\"\",\"rType\":-1,\"source\":6}"
                        },
                        {
                            "msgId": "9000004",
                            "cliMsgId": "1782286280004",
                            "msgType": "chat.reaction",
                            "uidFrom": "999",
                            "idTo": "456",
                            "content": "{\"rMsg\":[{\"gMsgID\":75002,\"cMsgID\":1782286269002,\"msgType\":1}],\"rIcon\":\"\",\"rType\":-1,\"source\":0}"
                        },
                        {
                            "msgId": "9000005",
                            "cliMsgId": "1782286280005",
                            "msgType": "chat.reaction",
                            "uidFrom": "0",
                            "idTo": "0",
                            "content": "{\"rMsg\":[{\"gMsgID\":75003,\"cMsgID\":1782286269003,\"msgType\":1}],\"rIcon\":\"/-strong\",\"rType\":3,\"source\":0}"
                        }
                    ],
                    "reactGroups": [
                        {
                            "actionId": "1613363535484",
                            "msgId": "7970889894523",
                            "cliMsgId": "1782286279460",
                            "msgType": "chat.reaction",
                            "uidFrom": "8780300047720658810",
                            "idTo": "group_7560225984834507848",
                            "content": "{\"rType\":5,\"rMsg\":[{\"cMsgID\":1782286269667,\"gMsgID\":0,\"msgType\":1}],\"source\":0,\"msgSender\":\"456\",\"rIcon\":\"/-heart\"}"
                        },
                        {
                            "msgId": "7970889894524",
                            "cliMsgId": "1782286279461",
                            "msgType": "chat.reaction",
                            "uidFrom": "0",
                            "idTo": "g7560225984834507848",
                            "content": "{\"rType\":75,\"rMsg\":[{\"cMsgID\":1782286269668,\"gMsgID\":75099,\"msgType\":32}],\"source\":0,\"rIcon\":\"🔥\"}"
                        },
                        {
                            "msgId": "7970889894525",
                            "cliMsgId": "1782286279462",
                            "msgType": "chat.reaction",
                            "uidFrom": "777",
                            "idTo": "7560225984834507848",
                            "content": "{\"rType\":32,\"rMsg\":[{\"cMsgID\":1782286269668,\"gMsgID\":75099,\"msgType\":32}],\"source\":0}"
                        }
                    ]
                }
            }),
        };

        let actions = parse_ws_event(&ev, "456", "send2me_77");
        assert_eq!(actions.len(), 8);

        // 0: Self reacted ❤️ on phone in DM
        assert!(matches!(
            &actions[0],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, actor_id, emoji, me, removed }
                if thread_id == "999"
                    && msg_id == "75001"
                    && cli_msg_id.as_deref() == Some("1782286269001")
                    && actor_id.as_deref() == Some("456")
                    && emoji.as_deref() == Some("❤️")
                    && *me
                    && !*removed
        ));
        // 1: Peer reacted 😆 on phone in DM
        assert!(matches!(
            &actions[1],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, actor_id, emoji, me, removed }
                if thread_id == "999"
                    && msg_id == "75002"
                    && cli_msg_id.as_deref() == Some("1782286269002")
                    && actor_id.as_deref() == Some("999")
                    && emoji.as_deref() == Some("😆")
                    && !*me
                    && !*removed
        ));
        // 2: Self unreact on phone in DM (rIcon="" + rType=-1)
        assert!(matches!(
            &actions[2],
            WsParsedAction::Reaction { thread_id, msg_id, emoji, me, removed, .. }
                if thread_id == "999" && msg_id == "75001" && emoji.is_none() && *me && *removed
        ));
        // 3: Peer unreact on phone in DM (rIcon="" + rType=-1)
        assert!(matches!(
            &actions[3],
            WsParsedAction::Reaction { thread_id, msg_id, actor_id, emoji, me, removed, .. }
                if thread_id == "999"
                    && msg_id == "75002"
                    && actor_id.as_deref() == Some("999")
                    && emoji.is_none()
                    && !*me
                    && *removed
        ));
        // 4: Self reacted 👍 in Cloud (send2me)
        assert!(matches!(
            &actions[4],
            WsParsedAction::Reaction { thread_id, msg_id, emoji, me, removed, .. }
                if thread_id == "send2me_77"
                    && msg_id == "75003"
                    && emoji.as_deref() == Some("👍")
                    && *me
                    && !*removed
        ));
        // 5: Group reaction from mobile with gMsgID=0 -> falls back to cMsgID, strips group_ prefix
        assert!(matches!(
            &actions[5],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, actor_id, emoji, me, removed }
                if thread_id == "7560225984834507848"
                    && msg_id == "1782286269667"
                    && cli_msg_id.as_deref() == Some("1782286269667")
                    && actor_id.as_deref() == Some("8780300047720658810")
                    && emoji.as_deref() == Some("❤️")
                    && !*me
                    && !*removed
        ));
        // 6: Group self custom Unicode emoji 🔥 preserved verbatim
        assert!(matches!(
            &actions[6],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, emoji, me, removed, .. }
                if thread_id == "7560225984834507848"
                    && msg_id == "75099"
                    && cli_msg_id.as_deref() == Some("1782286269668")
                    && emoji.as_deref() == Some("🔥")
                    && *me
                    && !*removed
        ));
        // 7: Missing rIcon falls back to rType=32 -> 😮
        assert!(matches!(
            &actions[7],
            WsParsedAction::Reaction { thread_id, msg_id, emoji, me, removed, .. }
                if thread_id == "7560225984834507848"
                    && msg_id == "75099"
                    && emoji.as_deref() == Some("😮")
                    && !*me
                    && !*removed
        ));
    }

    #[test]
    fn parses_ws_610_611_pulled_reactions_and_preserves_cli_msg_id_on_messages() {
        // Pulled old DM reactions (cmd=610) with object content
        let ev_610 = WsEvent {
            cmd: CMD_REACTION_PULL_1_1,
            sub_cmd: 1,
            version: 1,
            payload: json!({
                "data": {
                    "reacts": [
                        {
                            "uidFrom": "999",
                            "idTo": "456",
                            "content": {
                                "rMsg": [{ "gMsgID": "81001", "cMsgID": "1780000000001", "msgType": 1 }],
                                "rIcon": ":-((",
                                "rType": 2,
                                "source": 6
                            }
                        }
                    ]
                }
            }),
        };
        let a_610 = parse_ws_event(&ev_610, "456", "");
        assert_eq!(a_610.len(), 1);
        assert!(matches!(
            &a_610[0],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, emoji, me, removed, .. }
                if thread_id == "999"
                    && msg_id == "81001"
                    && cli_msg_id.as_deref() == Some("1780000000001")
                    && emoji.as_deref() == Some("😢")
                    && !*me
                    && !*removed
        ));

        // Pulled old group reactions (cmd=611)
        let ev_611 = WsEvent {
            cmd: CMD_REACTION_PULL_GROUP,
            sub_cmd: 1,
            version: 1,
            payload: json!({
                "data": {
                    "reactGroups": [
                        {
                            "uidFrom": "0",
                            "idTo": "555666",
                            "content": {
                                "rMsg": [{ "gMsgID": "82001", "cMsgID": "1780000000002", "msgType": 1 }],
                                "rIcon": ":-h",
                                "rType": 20,
                                "source": 6
                            }
                        }
                    ]
                }
            }),
        };
        let a_611 = parse_ws_event(&ev_611, "456", "");
        assert_eq!(a_611.len(), 1);
        assert!(matches!(
            &a_611[0],
            WsParsedAction::Reaction { thread_id, msg_id, emoji, me, removed, .. }
                if thread_id == "555666"
                    && msg_id == "82001"
                    && emoji.as_deref() == Some("😠")
                    && *me
                    && !*removed
        ));

        // A chat.reaction item inside cmd=501 is parsed as Reaction (never a fake chat bubble)
        let ev_501 = WsEvent {
            cmd: CMD_PUSH_MSG_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "msgs": [
                        {
                            "msgId": "83001",
                            "cliMsgId": "1780000000003",
                            "uidFrom": "999",
                            "idTo": "456",
                            "msgType": "chat.text",
                            "content": "Hi from phone",
                            "ts": "1780000000450"
                        },
                        {
                            "msgId": "83002",
                            "cliMsgId": "1780000000009",
                            "uidFrom": "999",
                            "idTo": "456",
                            "msgType": "chat.reaction",
                            "content": "{\"rMsg\":[{\"gMsgID\":83001,\"cMsgID\":1780000000003,\"msgType\":1}],\"rIcon\":\"/-heart\",\"rType\":5}"
                        }
                    ]
                }
            }),
        };
        let a_501 = parse_ws_event(&ev_501, "456", "");
        assert_eq!(a_501.len(), 2);
        match &a_501[0] {
            WsParsedAction::Message(m) => {
                assert_eq!(m.msg_id, "83001");
                assert_eq!(m.cli_msg_id.as_deref(), Some("1780000000003"));
                assert_eq!(m.ts, 1_780_000_000_450);
            }
            other => panic!("expected Message, got {other:?}"),
        }
        assert!(matches!(
            &a_501[1],
            WsParsedAction::Reaction { thread_id, msg_id, cli_msg_id, emoji, me, removed, .. }
                if thread_id == "999"
                    && msg_id == "83001"
                    && cli_msg_id.as_deref() == Some("1780000000003")
                    && emoji.as_deref() == Some("❤️")
                    && !*me
                    && !*removed
        ));
    }

    #[test]
    fn parses_ws_504_and_524_clear_unreads_from_mobile_and_desktop() {
        let ev_504 = WsEvent {
            cmd: CMD_CLEAR_UNREAD_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "clearUnreads": [
                        { "idTo": "999001", "isGroup": 0, "lastMsgId": "7451001", "ts": 0, "type": 0 },
                        { "idTo": "999002", "isGroup": 0, "lastMsgId": "7451002", "ts": 0, "type": 1 },
                        { "idTo": "-1", "isGroup": 0, "lastMsgId": "0", "ts": 0, "type": 2 },
                        { "idTo": "0", "isGroup": 0, "lastMsgId": "7451003", "ts": 0, "type": 0 }
                    ],
                    "msgs": [],
                    "groupMsgs": [],
                    "more": 0
                }
            }),
        };
        let a_504 = parse_ws_event(&ev_504, "456", "cloud_456");
        assert_eq!(a_504.len(), 2);
        assert!(matches!(
            &a_504[0],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "999001"
                    && !*is_unread
                    && last_msg_id.as_deref() == Some("7451001")
        ));
        assert!(matches!(
            &a_504[1],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "cloud_456"
                    && !*is_unread
                    && last_msg_id.as_deref() == Some("7451003")
        ));

        let ev_524 = WsEvent {
            cmd: CMD_CLEAR_UNREAD_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "clearUnreads": [
                        { "idTo": "g888777", "isGroup": 1, "lastMsgId": "7452001", "ts": 0, "type": 0 },
                        { "idTo": "888778", "isGroup": 1, "lastMsgId": "7452002", "ts": 0, "type": 0 },
                        { "idTo": "888779", "isGroup": 1, "lastMsgId": "7452003", "ts": 0, "type": 1 }
                    ],
                    "msgs": [],
                    "groupMsgs": [],
                    "more": 0
                }
            }),
        };
        let a_524 = parse_ws_event(&ev_524, "456", "");
        assert_eq!(a_524.len(), 2);
        assert!(matches!(
            &a_524[0],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "888777"
                    && !*is_unread
                    && last_msg_id.as_deref() == Some("7452001")
        ));
        assert!(matches!(
            &a_524[1],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "888778"
                    && !*is_unread
                    && last_msg_id.as_deref() == Some("7452002")
        ));
    }

    #[test]
    fn parses_ws_502_seens_522_group_seens_uid_zero_and_602_actions() {
        // Real Zalo cmd 502: `seens` for peer read vs explicit `uidFrom: "0"` self read
        let ev_502 = WsEvent {
            cmd: CMD_DELIVERED_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "delivereds": [],
                    "seens": [
                        { "idTo": "999", "msgId": "5001", "realMsgId": "5001" },
                        { "uidFrom": "0", "idTo": "888", "msgId": "5002" }
                    ]
                }
            }),
        };
        let a_502 = parse_ws_event(&ev_502, "456", "");
        assert_eq!(a_502.len(), 2);
        assert!(matches!(
            &a_502[0],
            WsParsedAction::PeerSeen { thread_id, is_delivered_only, .. }
                if thread_id == "999" && !*is_delivered_only
        ));
        assert!(matches!(
            &a_502[1],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "888" && !*is_unread && last_msg_id.as_deref() == Some("5002")
        ));

        // Real Zalo cmd 522: `groupSeens` containing own uid and `"0"`
        let ev_522 = WsEvent {
            cmd: CMD_DELIVERED_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "delivereds": [],
                    "groupSeens": [
                        { "msgId": "6001", "groupId": "g777001", "seenUids": ["456"] },
                        { "msgId": "6002", "groupId": "777002", "seenUids": ["0", "333"] }
                    ]
                }
            }),
        };
        let a_522 = parse_ws_event(&ev_522, "456", "");
        assert_eq!(a_522.len(), 3);
        assert!(matches!(
            &a_522[0],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "777001" && !*is_unread && last_msg_id.as_deref() == Some("6001")
        ));
        assert!(matches!(
            &a_522[1],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "777002" && !*is_unread && last_msg_id.as_deref() == Some("6002")
        ));
        assert!(matches!(
            &a_522[2],
            WsParsedAction::PeerSeen { thread_id, is_delivered_only, .. }
                if thread_id == "777002" && !*is_delivered_only
        ));

        // Real Zalo cmd 602: `actions` wrapper carrying unread-mark updates
        let ev_602 = WsEvent {
            cmd: CMD_CTRL_ACTION,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "actions": [
                        {
                            "act_type": "conv",
                            "act": "removeUnreadMark",
                            "data": "{\"convsUser\":[\"111\"],\"convsGroup\":[\"g222\"]}"
                        },
                        {
                            "act_type": "conv",
                            "act": "addUnreadMark",
                            "data": "{\"convsUser\":[\"333\"],\"convsGroup\":[]}"
                        }
                    ]
                }
            }),
        };
        let a_602 = parse_ws_event(&ev_602, "456", "");
        assert_eq!(a_602.len(), 3);
        assert!(matches!(
            &a_602[0],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "111" && !*is_unread
        ));
        assert!(matches!(
            &a_602[1],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "222" && !*is_unread
        ));
        assert!(matches!(
            &a_602[2],
            WsParsedAction::Read { thread_id, is_unread, .. } if thread_id == "333" && *is_unread
        ));

        // Offline pull cmd 510 containing both `msgs` and `clearUnreads`: msgs emitted before clear
        let ev_510 = WsEvent {
            cmd: CMD_PULL_MSG_1_1,
            sub_cmd: 1,
            version: 1,
            payload: json!({
                "data": {
                    "msgs": [
                        {
                            "msgId": "7001",
                            "cliMsgId": "1780000007001",
                            "uidFrom": "999",
                            "idTo": "456",
                            "msgType": "chat.text",
                            "content": "Already read on phone",
                            "ts": "1780000007001"
                        }
                    ],
                    "clearUnreads": [
                        { "idTo": "999", "isGroup": 0, "lastMsgId": "7001", "ts": 0, "type": 0 }
                    ]
                }
            }),
        };
        let a_510 = parse_ws_event(&ev_510, "456", "");
        assert_eq!(a_510.len(), 2);
        assert!(matches!(&a_510[0], WsParsedAction::Message(m) if m.msg_id == "7001"));
        assert!(matches!(
            &a_510[1],
            WsParsedAction::Read { thread_id, is_unread, last_msg_id }
                if thread_id == "999" && !*is_unread && last_msg_id.as_deref() == Some("7001")
        ));
    }

    #[test]
    fn extract_poll_meta_parses_nested_params_actionlist_expiry_and_status() {
        // 1. Group message with `msginfo.actionlist` and poll details inside stringified `content.params`
        let nested_msg = json!({
            "msgId": "88001",
            "uidFrom": "100",
            "grid": "g888111",
            "msgType": "group.action",
            "ts": 1_750_000_000_000_i64,
            "content": {
                "title": "An đã tạo bình chọn mới",
                "action": "msginfo.actionlist",
                "params": "{\"poll_id\":654321,\"question\":\"Tối nay đi đâu?\",\"allow_multi_choices\":true,\"allow_add_new_option\":true,\"options\":[{\"option_id\":11,\"content\":\"Café\",\"vote_count\":2,\"voters\":[\"u1\",\"u2\"],\"voted\":true},{\"option_id\":12,\"content\":\"Xem phim\",\"voters\":[\"u3\"]}]}"
            }
        });
        let parsed = parse_chat_message(&nested_msg, None, "999", "", true).unwrap();
        assert_eq!(parsed.msg_type, "chat.poll");
        assert!(parsed.attachment_url.is_none());
        assert_eq!(parsed.text, "Bình chọn: Tối nay đi đâu?");
        let dto: PollDto = serde_json::from_str(
            parsed
                .attachment_name
                .as_deref()
                .unwrap()
                .strip_prefix("teza-poll:")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dto.poll_id, 654321);
        assert_eq!(dto.question, "Tối nay đi đâu?");
        assert!(dto.allow_multi);
        assert!(dto.allow_add_option);
        assert!(!dto.closed);
        assert_eq!(dto.options.len(), 2);
        assert_eq!(dto.options[0].id, 11);
        assert_eq!(dto.options[0].votes, 2);
        assert!(dto.options[0].voted);
        assert_eq!(dto.options[1].votes, 1);

        // 2. Non-poll `msginfo.actionlist` (e.g. group note) must NOT be misclassified as a poll
        let non_poll = json!({
            "msgId": "88002",
            "uidFrom": "100",
            "grid": "888111",
            "msgType": "group.action",
            "ts": 1_750_000_001_000_i64,
            "content": {
                "title": "Ghi chú nhóm",
                "action": "msginfo.actionlist",
                "params": "{\"topicId\":\"note-1\",\"title\":\"Ghi chú nội quy\"}"
            }
        });
        let parsed_note = parse_chat_message(&non_poll, None, "999", "", true).unwrap();
        assert_ne!(parsed_note.msg_type, "chat.poll");

        // 3. `status == 2` and past `expired_time` mark poll as closed; nested `data` unwrapped
        let closed_by_status = json!({
            "data": {
                "poll_id": 1001,
                "question": "Đã khóa",
                "status": 2,
                "closed": false,
                "options": [{ "option_id": 1, "content": "A", "votes": 1 }]
            }
        });
        let p_status = parse_poll_value(&closed_by_status, 0).unwrap();
        assert_eq!(p_status.poll_id, 1001);
        assert!(p_status.closed);

        let expired_past = json!({
            "poll_id": 1002,
            "question": "Đã hết hạn",
            "expired_time": 1_700_000_000_i64,
            "closed": false,
            "options": [{ "option_id": 1, "content": "A", "votes": 0 }]
        });
        let p_expired = parse_poll_value(&expired_past, 0).unwrap();
        assert!(p_expired.closed);

        let future_ms = chrono::Utc::now().timestamp_millis() + 3_600_000;
        let active_future = json!({
            "poll_id": 1003,
            "question": "Đang mở",
            "expired_time": future_ms,
            "status": 1,
            "closed": false,
            "options": [{ "option_id": 1, "content": "A", "votes": 0 }]
        });
        let p_active = parse_poll_value(&active_future, 0).unwrap();
        assert!(!p_active.closed);
    }

    #[test]
    fn parses_ws_group_poll_control_events_into_poll_updated() {
        let ev = WsEvent {
            cmd: CMD_CTRL_PUSH_CTRL,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "controls": [
                    {
                        "act_type": "group",
                        "content": {
                            "act": "new_poll",
                            "data": "{\"groupId\":\"g888111\",\"topic\":{\"id\":50101,\"type\":1}}"
                        }
                    },
                    {
                        "act_type": "group",
                        "content": {
                            "act": "vote_poll",
                            "data": "{\"groupId\":\"group_888222\",\"poll_id\":50102}"
                        }
                    },
                    {
                        "content": {
                            "act_type": "group",
                            "act": "end_poll",
                            "data": "{\"grid\":\"888333\",\"params\":\"{\\\"pollId\\\":\\\"50103\\\"}\"}"
                        }
                    },
                    {
                        "act_type": "group",
                        "act": "update_poll",
                        "data": { "groupId": "888444" }
                    }
                ]
            }),
        };

        let actions = parse_ws_event(&ev, "999", "");
        assert_eq!(actions.len(), 4);
        assert!(matches!(
            &actions[0],
            WsParsedAction::PollUpdated { thread_id, poll_id }
                if thread_id == "888111" && *poll_id == Some(50101)
        ));
        assert!(matches!(
            &actions[1],
            WsParsedAction::PollUpdated { thread_id, poll_id }
                if thread_id == "888222" && *poll_id == Some(50102)
        ));
        assert!(matches!(
            &actions[2],
            WsParsedAction::PollUpdated { thread_id, poll_id }
                if thread_id == "888333" && *poll_id == Some(50103)
        ));
        assert!(matches!(
            &actions[3],
            WsParsedAction::PollUpdated { thread_id, poll_id }
                if thread_id == "888444" && poll_id.is_none()
        ));
    }

    #[test]
    fn ws_poll_vote_update_end_and_add_option_emit_poll_updated_never_duplicate_message() {
        let ev = WsEvent {
            cmd: CMD_PUSH_MSG_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "groupMsgs": [
                        {
                            "msgId": "90001",
                            "uidFrom": "100",
                            "grid": "g888111",
                            "msgType": "group.poll",
                            "ts": 1_760_000_001_000_i64,
                            "content": {
                                "poll_id": 777111_i64,
                                "question": "Cuối tuần đi đâu?",
                                "options": [
                                    { "option_id": 1, "content": "Đà Lạt", "votes": 1 },
                                    { "option_id": 2, "content": "Vũng Tàu", "votes": 0 }
                                ]
                            }
                        },
                        {
                            "msgId": "90002",
                            "uidFrom": "200",
                            "grid": "g888111",
                            "msgType": "group.poll",
                            "action": "vote_poll",
                            "ts": 1_760_000_002_000_i64,
                            "content": {
                                "poll_id": 777111_i64,
                                "title": "Bình đã tham gia bình chọn"
                            }
                        },
                        {
                            "msgId": "90003",
                            "uidFrom": "300",
                            "grid": "g888111",
                            "msgType": "group.action",
                            "ts": 1_760_000_003_000_i64,
                            "content": {
                                "action": "msginfo.actionlist",
                                "title": "Chi đã bình chọn",
                                "params": "{\"topic\":{\"id\":777111,\"type\":1}}"
                            }
                        },
                        {
                            "msgId": "90004",
                            "uidFrom": "300",
                            "grid": "888111",
                            "msgType": "group.action",
                            "subType": "add_option",
                            "ts": 1_760_000_004_000_i64,
                            "content": {
                                "pollInfo": { "poll_id": 777111_i64 }
                            }
                        },
                        {
                            "msgId": "90005",
                            "uidFrom": "100",
                            "grid": "888111",
                            "msgType": "group.action",
                            "ts": 1_760_000_005_000_i64,
                            "content": {
                                "action": "end_poll",
                                "extra": "{\"pollId\":\"777111\"}"
                            }
                        },
                        {
                            "msgId": "90006",
                            "uidFrom": "200",
                            "grid": "888111",
                            "msgType": "group.action",
                            "ts": 1_760_000_006_000_i64,
                            "content": {
                                "action": "msginfo.actionlist",
                                "params": "{\"poll_id\":777111}"
                            }
                        },
                        {
                            "msgId": "90007",
                            "uidFrom": "200",
                            "grid": "888111",
                            "msgType": "chat.text",
                            "ts": 1_760_000_007_000_i64,
                            "content": "Mình đã bình chọn xong rồi nhé!"
                        }
                    ]
                }
            }),
        };

        let actions = parse_ws_event(&ev, "999", "");
        assert_eq!(actions.len(), 7);
        assert!(matches!(
            &actions[0],
            WsParsedAction::Message(m) if m.msg_id == "90001" && m.msg_type == "chat.poll"
        ));
        for (idx, act) in actions.iter().enumerate().take(6).skip(1) {
            assert!(
                matches!(
                    act,
                    WsParsedAction::PollUpdated { thread_id, poll_id }
                        if thread_id == "888111" && *poll_id == Some(777111)
                ),
                "index {idx} should emit PollUpdated, got {act:?}"
            );
        }
        assert!(matches!(
            &actions[6],
            WsParsedAction::Message(m)
                if m.msg_id == "90007"
                    && m.msg_type == "chat.text"
                    && m.text == "Mình đã bình chọn xong rồi nhé!"
        ));
    }

    #[test]
    fn poll_parser_unwraps_topic_pollinfo_extra_avoids_fake_poll_id_and_guards_vote_question() {
        // 1. Nested `topic`, `pollInfo`, and `extra` are unwrapped
        let nested_topic = json!({
            "topic": {
                "id": 445566_i64,
                "pollInfo": {
                    "question": "Chốt giờ họp",
                    "extra": "{\"allow_multi_choices\":true,\"options\":[{\"option_id\":1,\"content\":\"9h\",\"votes\":2}]}"
                }
            }
        });
        let p = parse_poll_value(&nested_topic, 0).unwrap();
        assert_eq!(p.poll_id, 445566);
        assert_eq!(p.question, "Chốt giờ họp");
        assert!(p.allow_multi);
        assert_eq!(p.options.len(), 1);
        assert_eq!(p.options[0].votes, 2);

        // 2. Vote-only notice with NO real `poll_id` and NO options must NOT fabricate `poll_id = msgId`
        let no_id_vote_notice = json!({
            "msgId": "9988776655",
            "uidFrom": "200",
            "grid": "888111",
            "msgType": "group.poll",
            "ts": 1_760_000_010_000_i64,
            "content": {
                "title": "An đã tham gia bình chọn"
            }
        });
        assert!(extract_poll_meta(no_id_vote_notice.get("content"), "group.poll", None).is_none());
        let parsed = parse_chat_message(&no_id_vote_notice, None, "999", "", true).unwrap();
        assert_ne!(parsed.msg_type, "chat.poll");
        assert!(
            !parsed
                .attachment_name
                .as_deref()
                .is_some_and(|s| s.starts_with("teza-poll:"))
        );

        // 3. Real `poll_id` with vote-activity `title` falls back to generic "Bình chọn"
        let vote_title_with_id = json!({
            "poll_id": 888999_i64,
            "title": "Bình chọn đã kết thúc",
            "options": [{ "option_id": 1, "content": "OK", "votes": 5 }]
        });
        let partial = parse_poll_value(&vote_title_with_id, 0).unwrap();
        assert_eq!(partial.poll_id, 888999);
        assert_eq!(partial.question, "Bình chọn");

        // 4. Poll with real question + options but no `poll_id` leaves `poll_id == 0` (never msgId)
        let missing_poll_id_creation = json!({
            "msgId": "1234567890123",
            "uidFrom": "100",
            "grid": "888111",
            "msgType": "group.poll",
            "ts": 1_760_000_020_000_i64,
            "content": {
                "question": "Ăn trưa ở đâu?",
                "options": ["Phở", "Cơm"]
            }
        });
        let created = parse_chat_message(&missing_poll_id_creation, None, "999", "", true).unwrap();
        assert_eq!(created.msg_type, "chat.poll");
        let dto: PollDto = serde_json::from_str(
            created
                .attachment_name
                .as_deref()
                .unwrap()
                .strip_prefix("teza-poll:")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(dto.poll_id, 0);
        assert_eq!(dto.question, "Ăn trưa ở đâu?");
    }

    #[test]
    fn ws_e2ee_551_552_unknown_qcmd_fallback_stranger_buckets_and_uid_suffix_normalization() {
        // 1. `_0` suffix normalization in `extract_thread_id` (incoming & outgoing + send2me intact)
        let incoming_suffix = json!({ "uidFrom": "777888999_0", "idTo": "456_0" });
        assert_eq!(
            extract_thread_id(&incoming_suffix, false, "456", "send2me_77"),
            Some(("777888999".to_string(), false))
        );
        let outgoing_suffix = json!({ "uidFrom": "456_0", "idTo": "777888999_0" });
        assert_eq!(
            extract_thread_id(&outgoing_suffix, false, "456", "send2me_77"),
            Some(("777888999".to_string(), false))
        );
        let cloud_entry = json!({ "uidFrom": "send2me_77", "idTo": "456_0" });
        assert_eq!(
            extract_thread_id(&cloud_entry, false, "456", "send2me_77"),
            Some(("send2me_77".to_string(), false))
        );

        // 2. CMD_PUSH_MSG_E2EE_1_1 (551) & CMD_PUSH_MSG_E2EE_GROUP (552)
        let ev_551 = WsEvent {
            cmd: CMD_PUSH_MSG_E2EE_1_1,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "msgs": [{
                        "msgId": "e2ee-1",
                        "uidFrom": "666555444_0",
                        "idTo": "456_0",
                        "dName": "Người lạ E2EE",
                        "msgType": "chat.text",
                        "content": "Chào bạn (E2EE)",
                        "ts": 1_760_000_100_000_i64
                    }]
                }
            }),
        };
        let a_551 = parse_ws_event(&ev_551, "456", "send2me_77");
        assert_eq!(a_551.len(), 1);
        assert!(matches!(
            &a_551[0],
            WsParsedAction::Message(m)
                if m.thread_id == "666555444" && m.text == "Chào bạn (E2EE)"
        ));

        let ev_552 = WsEvent {
            cmd: CMD_PUSH_MSG_E2EE_GROUP,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "groupMsgs": [{
                        "msgId": "e2ee-g1",
                        "uidFrom": "222_0",
                        "idTo": "g999888",
                        "msgType": "chat.text",
                        "content": "Tin nhóm E2EE",
                        "ts": 1_760_000_101_000_i64
                    }]
                }
            }),
        };
        let a_552 = parse_ws_event(&ev_552, "456", "");
        assert_eq!(a_552.len(), 1);
        assert!(matches!(
            &a_552[0],
            WsParsedAction::Message(m)
                if m.thread_id == "999888" && m.text == "Tin nhóm E2EE"
        ));

        // 3. Unknown qCmd (e.g., 599) with nested `data.data.strangerMsgs` + stringified `otherMsgs`
        let ev_unknown = WsEvent {
            cmd: 599,
            sub_cmd: 0,
            version: 1,
            payload: json!({
                "data": {
                    "data": {
                        "strangerMsgs": [{
                            "msgId": "str-1",
                            "uidFrom": "333222111_0",
                            "idTo": "456_0",
                            "dName": "Khách lạ 1",
                            "msgType": "chat.text",
                            "content": "Mình hỏi mua hàng",
                            "ts": 1_760_000_200_000_i64
                        }],
                        "otherMsgs": "[{\"msgId\":\"str-2\",\"uidFrom\":\"444333222_0\",\"idTo\":\"456\",\"dName\":\"Khách lạ 2\",\"msgType\":\"chat.text\",\"content\":\"Alo shop ơi\",\"ts\":1760000201000}]"
                    }
                }
            }),
        };
        assert!(ev_unknown.has_chat_message_payload());
        let a_unknown = parse_ws_event(&ev_unknown, "456", "");
        assert_eq!(a_unknown.len(), 2);
        assert!(matches!(
            &a_unknown[0],
            WsParsedAction::Message(m)
                if m.thread_id == "333222111" && m.text == "Mình hỏi mua hàng"
        ));
        assert!(matches!(
            &a_unknown[1],
            WsParsedAction::Message(m)
                if m.thread_id == "444333222" && m.text == "Alo shop ơi"
        ));
    }
}
