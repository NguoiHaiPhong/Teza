//! Thread list synchronization (`getfriends`, `getlg/v4`, `getmg-v2`, `get-last-msgs`) and history loading.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::chat::parser::{
    clean_group_id, collect_reactions_from_node, extract_content_text, extract_thread_id,
    format_thread_preview, is_raw_numeric_id, normalize_avatar_url, normalize_media_url,
    normalize_ts_ms, parse_chat_message, parse_friend_thread, parse_poll_value,
    parse_reaction_item, pick_str, WsParsedAction,
};
use crate::chat::{
    AddGroupMembersResultDto, ChatFolderDto, FriendActionResultDto, FriendRequestItemDto,
    FriendRequestsListDto, GroupDetailsResult, KickGroupMembersResultDto, LeaveGroupResultDto,
    MessageDto, MutualGroupDto, PollDto, PollOptionDto, ReactionSummary, ThreadDto, ThreadKind,
    ThreadMemberDto, UserProfileDto,
};
use crate::session::{is_session_expired_code, Session, SessionError};

fn ensure_not_expired(resp: &Value) -> Result<()> {
    if let Some(code) = resp.get("error_code").and_then(Value::as_i64) {
        if is_session_expired_code(code) {
            return Err(SessionError::Expired { code }.into());
        }
    }
    Ok(())
}

fn is_expired_err(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<SessionError>(),
        Some(SessionError::Expired { .. })
    )
}

fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

fn unwrap_data_str(mut resp: Value) -> Value {
    if let Some(d_str) = resp.get("data").and_then(Value::as_str) {
        let trimmed = d_str.trim();
        if (trimmed.starts_with('{') && trimmed.ends_with('}'))
            || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                resp["data"] = parsed;
            }
        }
    }
    resp
}

pub async fn get_friends(session: &Session) -> Result<Vec<ThreadDto>> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[
        ("incInvalid", json!(1)),
        ("page", json!(1)),
        ("count", json!(2000)),
        ("avatar_size", json!(160)),
        ("actiontime", json!(0)),
        ("imei", json!(session.imei)),
    ]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let path_and_query = format!(
        "/api/social/friend/getfriends?{}&{params}",
        session.common_params()
    );
    let raw = session.get_endpoint("friend", &path_and_query).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;
    let data_root = resp.get("data").unwrap_or(&resp);

    let items: &[Value] = match data_root {
        Value::Array(arr) => arr.as_slice(),
        Value::Object(o) if o.get("friends").and_then(Value::as_array).is_some() => {
            o["friends"].as_array().map(Vec::as_slice).unwrap_or(&[])
        }
        Value::Object(o) if o.get("data").and_then(Value::as_array).is_some() => {
            o["data"].as_array().map(Vec::as_slice).unwrap_or(&[])
        }
        _ => &[],
    };

    let mut threads: Vec<ThreadDto> = items.iter().filter_map(parse_friend_thread).collect();

    if !session.profile.send2me_id.is_empty()
        && !threads.iter().any(|t| t.id == session.profile.send2me_id)
    {
        threads.push(ThreadDto {
            id: session.profile.send2me_id.clone(),
            title: "Cloud của tôi (Saved Messages)".into(),
            avatar: String::new(),
            kind: ThreadKind::Cloud,
            last_preview: String::new(),
            last_ts: 0,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        });
    }

    Ok(threads)
}

pub async fn get_groups(session: &Session) -> Result<Vec<ThreadDto>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{}/api/group/getlg/v4?{}",
        session.domains.group.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.get_json(&url).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;
    let data_node = resp
        .pointer("/data/data")
        .or_else(|| resp.get("data"))
        .unwrap_or(&resp);

    let Some(map) = data_node.get("gridVerMap").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let gids: Vec<String> = map.keys().cloned().collect();
    if gids.is_empty() {
        return Ok(Vec::new());
    }

    let (info_map, removed) = match fetch_group_profiles(session, &gids).await {
        Ok(res) => res,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => Default::default(),
    };
    let mut need_mem_uids: Vec<String> = Vec::new();
    let mut groups = Vec::new();
    for gid in gids {
        let clean_gid = clean_group_id(&gid).to_string();
        if removed.contains(&gid) || removed.contains(&clean_gid) {
            continue;
        }
        let info = info_map.get(&gid).or_else(|| info_map.get(&clean_gid));
        let title = info
            .and_then(|v| pick_str(v, &["name", "groupName", "displayName"]))
            .unwrap_or_else(|| format!("Nhóm {gid}"));
        let avatar = info
            .and_then(|v| pick_str(v, &["fullAvt", "avt", "avatar", "groupAvatar", "thumb"]))
            .map(|u| normalize_avatar_url(&u))
            .unwrap_or_default();
        if avatar.is_empty() {
            if let Some(info_val) = info {
                let (mids, _) = extract_group_member_ids(info_val);
                for uid in mids {
                    if !need_mem_uids.iter().any(|x| x == &uid) {
                        need_mem_uids.push(uid);
                    }
                }
            }
        }
        let canonical_id = if clean_gid.is_empty() {
            gid.clone()
        } else {
            clean_gid.clone()
        };
        groups.push(ThreadDto {
            id: canonical_id,
            title,
            avatar,
            kind: ThreadKind::Group,
            last_preview: String::new(),
            last_ts: 0,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        });
    }

    if !need_mem_uids.is_empty() {
        let mut all_profiles = Map::new();
        for chunk in need_mem_uids.chunks(50) {
            if let Ok(mp) = get_mini_profiles(session, chunk).await {
                for (k, v) in mp {
                    all_profiles.insert(k, v);
                }
            }
        }
        for g in &mut groups {
            if g.avatar.is_empty() {
                let clean_gid = clean_group_id(&g.id);
                if let Some(info) = info_map.get(&g.id).or_else(|| info_map.get(clean_gid)) {
                    let (mids, tot) = extract_group_member_ids(info);
                    if let Some(multi) = format_multi_group_avatar(&g.title, &mids, tot, &all_profiles) {
                        g.avatar = multi;
                    }
                }
            }
        }
    }
    Ok(groups)
}

async fn fetch_group_profiles_batch(
    session: &Session,
    chunk: &[String],
    all_info: &mut Map<String, Value>,
    all_removed: &mut HashSet<String>,
) -> Result<bool> {
    let mut gv_zero = Map::new();
    for gid in chunk {
        let clean = clean_group_id(gid);
        if !clean.is_empty() {
            gv_zero.insert(clean.to_string(), json!(0));
        }
    }
    if gv_zero.is_empty() {
        return Ok(true);
    }
    let body = obj(&[
        ("gridVerMap", json!(serde_json::to_string(&gv_zero)?)),
        ("imei", json!(session.imei)),
    ]);
    let params = session.encoded_params(&body)?;
    let hosts = session.domains.candidates("group");

    for host in hosts {
        let url = format!(
            "{}/api/group/getmg-v2?{}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        let raw = match session.post_form(&url, format!("params={params}")).await {
            Ok(v) => v,
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => {
                eprintln!("[litechat][groups] getmg-v2 ({host}) error: {e}");
                continue;
            }
        };
        let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
        ensure_not_expired(&resp)?;
        if let Some(code) = resp.get("error_code").and_then(Value::as_i64) {
            if code != 0 {
                eprintln!("[litechat][groups] getmg-v2 ({host}) error_code={code}");
                return Ok(false);
            }
        }
        let data_node = resp
            .pointer("/data/data")
            .or_else(|| resp.get("data"))
            .unwrap_or(&resp);

        if let Some(info_map) = data_node.get("gridInfoMap").and_then(Value::as_object) {
            for (k, v) in info_map {
                all_info.insert(k.clone(), v.clone());
            }
        }
        if let Some(arr) = data_node.get("removedsGroup").and_then(Value::as_array) {
            for r in arr {
                if let Some(s) = r.as_str() {
                    all_removed.insert(s.to_string());
                }
            }
        }
        return Ok(true);
    }
    Ok(false)
}

async fn fetch_group_profiles(
    session: &Session,
    group_ids: &[String],
) -> Result<(Map<String, Value>, HashSet<String>)> {
    let mut all_info = Map::new();
    let mut all_removed = HashSet::new();

    for chunk in group_ids.chunks(20) {
        if !fetch_group_profiles_batch(session, chunk, &mut all_info, &mut all_removed).await?
            && chunk.len() > 5
        {
            for sub in chunk.chunks(5) {
                let _ = fetch_group_profiles_batch(session, sub, &mut all_info, &mut all_removed)
                    .await?;
            }
        }
    }
    Ok((all_info, all_removed))
}

fn extract_group_member_ids(info: &Value) -> (Vec<String>, usize) {
    let mut raw_ids: Vec<String> = Vec::new();
    if let Some(arr) = info.get("memVerList").and_then(Value::as_array) {
        for item in arr {
            let raw = item
                .as_str()
                .map(str::to_string)
                .or_else(|| item.as_u64().map(|n| n.to_string()));
            if let Some(s) = raw {
                let uid = s.split('_').next().unwrap_or("").trim();
                if !uid.is_empty() && !raw_ids.iter().any(|x| x == uid) {
                    raw_ids.push(uid.to_string());
                }
            }
        }
    }
    let mut sorted: Vec<String> = Vec::new();
    if let Some(creator) = pick_str(info, &["creatorId"]) {
        if raw_ids.iter().any(|x| x == &creator) {
            sorted.push(creator);
        }
    }
    if let Some(admins) = info.get("adminIds").and_then(Value::as_array) {
        for a in admins {
            let aid = a
                .as_str()
                .map(str::to_string)
                .or_else(|| a.as_u64().map(|n| n.to_string()))
                .unwrap_or_default();
            if !aid.is_empty()
                && raw_ids.iter().any(|x| x == &aid)
                && !sorted.iter().any(|x| x == &aid)
            {
                sorted.push(aid);
            }
        }
    }
    for uid in &raw_ids {
        if !sorted.iter().any(|x| x == uid) {
            sorted.push(uid.clone());
        }
    }
    let total = info
        .get("totalMember")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or_else(|| sorted.len())
        .max(sorted.len());
    sorted.truncate(4);
    (sorted, total)
}

fn format_multi_group_avatar(
    group_name: &str,
    mem_ids: &[String],
    total_members: usize,
    profiles: &Map<String, Value>,
) -> Option<String> {
    let fallback_names: Vec<&str> = group_name
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut slots: Vec<String> = Vec::new();
    for (idx, uid) in mem_ids.iter().take(4).enumerate() {
        let mut m_name = fallback_names
            .get(idx)
            .copied()
            .unwrap_or("Thành viên")
            .to_string();
        let mut m_ava = String::new();
        if let Some(p) = profiles.get(uid) {
            if let Some(n) = pick_str(p, &["displayName", "zaloName", "name", "display_name"]) {
                m_name = n;
            }
            if let Some(a) = pick_str(p, &["avatar", "avt", "profile_avatar"]) {
                m_ava = normalize_avatar_url(&a);
            }
        }
        let clean_name = m_name.replace([';', '|'], " ");
        let clean_ava = m_ava.replace([';', '|'], "");
        slots.push(format!("{clean_name}|{clean_ava}"));
    }
    if slots.is_empty() {
        return None;
    }
    let has_meaningful_slot = slots.iter().any(|s| {
        let mut parts = s.splitn(2, '|');
        let name = parts.next().unwrap_or("");
        let url = parts.next().unwrap_or("");
        !url.is_empty() || (!name.is_empty() && name != "Thành viên")
    });
    if !has_meaningful_slot {
        return None;
    }
    let tot = total_members.max(slots.len());
    Some(format!("multi:{tot};{}", slots.join(";")))
}

fn collect_profiles_into(node: &Value, out: &mut Map<String, Value>) {
    match node {
        Value::Object(obj) => {
            for (k, v) in obj {
                let clean_key = k.split('_').next().unwrap_or(k).trim();
                if !clean_key.is_empty() && v.is_object() {
                    out.insert(clean_key.to_string(), v.clone());
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(uid) = pick_str(item, &["userId", "uid", "id", "friendId"]) {
                    out.insert(uid, item.clone());
                }
            }
        }
        _ => {}
    }
}

fn parse_profiles_response(resp: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for candidate in [
        resp.pointer("/data/profiles"),
        resp.get("profiles"),
        resp.pointer("/data/changed_profiles"),
        resp.get("changed_profiles"),
        resp.pointer("/data/data"),
        resp.get("data"),
    ]
    .into_iter()
    .flatten()
    {
        collect_profiles_into(candidate, &mut out);
        if !out.is_empty() {
            break;
        }
    }
    out
}

pub async fn get_mini_profiles(session: &Session, user_ids: &[String]) -> Result<Map<String, Value>> {
    if !session.is_authenticated() || user_ids.is_empty() {
        return Ok(Map::new());
    }
    let versioned_ids: Vec<String> = user_ids
        .iter()
        .map(|uid| {
            let clean = uid.trim();
            if clean.contains('_') {
                clean.to_string()
            } else {
                format!("{clean}_0")
            }
        })
        .collect();

    let mut profile_hosts = session.domains.candidates("profile");
    for extra in ["https://profile-wpa.zaloapp.com", "https://tt-profile-wpa.chat.zalo.me"] {
        if !profile_hosts.iter().any(|h| h == extra) {
            profile_hosts.push(extra.to_string());
        }
    }

    let members_body = obj(&[("friend_pversion_map", json!(versioned_ids))]);
    let members_params = session.encoded_params(&members_body)?;
    for host in &profile_hosts {
        let url = format!(
            "{}/api/social/group/members?{}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session.post_form(&url, format!("params={members_params}")).await {
            Ok(raw) => {
                let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let code = resp.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                if code == 0 {
                    let parsed = parse_profiles_response(&resp);
                    if !parsed.is_empty() {
                        return Ok(parsed);
                    }
                } else {
                    eprintln!("[litechat][profiles] group/members ({host}) error_code={code}");
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => {
                eprintln!("[litechat][profiles] group/members ({host}) error: {e}");
            }
        }
    }

    // Fallback: /api/social/friend/getprofiles/v2
    let v2_body = obj(&[
        ("friend_pversion_map", json!(versioned_ids)),
        ("avatar_size", json!(120)),
        ("language", json!("vi")),
    ]);
    let v2_params = session.encoded_params(&v2_body)?;
    for host in &profile_hosts {
        let url = format!(
            "{}/api/social/friend/getprofiles/v2?{}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session.post_form(&url, format!("params={v2_params}")).await {
            Ok(raw) => {
                let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let code = resp.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                if code == 0 {
                    let parsed = parse_profiles_response(&resp);
                    if !parsed.is_empty() {
                        return Ok(parsed);
                    }
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(_) => {}
        }
    }

    Ok(Map::new())
}

pub async fn get_last_msgs(session: &Session) -> Result<Value> {
    let body = obj(&[
        ("threadIdLocalMsgId", json!("{}")),
        ("imei", json!(session.imei)),
    ]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let url = format!(
        "{}/api/preloadconvers/get-last-msgs?{}&{params}",
        session.domains.conversation.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.get_json(&url).await?;
    Ok(unwrap_data_str(
        session.maybe_decrypt(raw, session.session_key_bytes()),
    ))
}

async fn fetch_pinned_thread_ids(session: &Session) -> Result<HashSet<String>> {
    if !session.is_authenticated() || session.domains.conversation.is_empty() {
        return Ok(HashSet::new());
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let url = format!(
        "{}/api/pinconvers/list?{}&{params}",
        session.domains.conversation.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.get_json(&url).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;

    let mut pinned = HashSet::new();
    let data = resp.get("data").unwrap_or(&resp);
    let arrays = [
        data.as_array(),
        data.get("conversations").and_then(Value::as_array),
        data.get("pinConvers").and_then(Value::as_array),
        data.get("list").and_then(Value::as_array),
    ];
    for arr in arrays.into_iter().flatten() {
        for item in arr {
            let raw_id = match item {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                Value::Object(_) => pick_str(item, &["id", "threadId", "conversId", "grid", "uid"]),
                _ => None,
            };
            if let Some(id) = raw_id {
                let clean = clean_group_id(&id);
                if !clean.is_empty() && clean != "0" {
                    pinned.insert(clean.to_string());
                }
            }
        }
    }
    Ok(pinned)
}

fn unwrap_unread_mark_data_node(resp: &Value) -> Option<Value> {
    let first = unwrap_history_data_node(resp)?;
    if let Some(inner) = first.get("data") {
        match inner {
            Value::String(s) => {
                let trimmed = s.trim();
                if trimmed.starts_with('{') && trimmed.ends_with('}') {
                    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                        if parsed.is_object() {
                            return Some(parsed);
                        }
                    }
                }
            }
            Value::Object(_) => return Some(inner.clone()),
            _ => {}
        }
    }
    if first.is_object() {
        Some(first)
    } else {
        None
    }
}

fn collect_unread_mark_ids(
    node: &Value,
    keys: &[&str],
    is_group: bool,
    out: &mut HashSet<String>,
) -> bool {
    let mut found = false;
    for key in keys {
        let Some(raw) = node.get(*key) else {
            continue;
        };
        found = true;
        let parsed_arr: Option<Vec<Value>> = match raw {
            Value::Array(arr) => Some(arr.clone()),
            Value::String(s) if s.trim().starts_with('[') => {
                serde_json::from_str::<Vec<Value>>(s.trim()).ok()
            }
            _ => None,
        };
        let Some(arr) = parsed_arr else {
            continue;
        };
        for item in arr {
            let raw_id = match &item {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                Value::Object(_) => {
                    if is_group {
                        pick_str(&item, &["id", "grid", "groupId", "threadId", "conversId"])
                    } else {
                        pick_str(&item, &["id", "uid", "userId", "threadId", "conversId"])
                    }
                }
                _ => None,
            };
            if let Some(id_str) = raw_id {
                let clean = if is_group
                    || id_str.starts_with("group_")
                    || id_str.starts_with('g')
                    || id_str.starts_with('G')
                {
                    clean_group_id(&id_str).to_string()
                } else {
                    id_str.trim().to_string()
                };
                if !clean.is_empty() && clean != "0" {
                    out.insert(clean);
                }
            }
        }
    }
    found
}

/// Parse `/api/convers/get-unread-mark` response into the set of unread thread IDs.
/// Returns `None` on non-zero `error_code` or unrecognized response structure so callers
/// safely fall back without clobbering local unread state.
pub fn parse_unread_marks_response(resp: &Value) -> Option<HashSet<String>> {
    if let Some(code) = resp.get("error_code").and_then(value_as_i64) {
        if code != 0 {
            return None;
        }
    }
    let data_node = unwrap_unread_mark_data_node(resp)?;
    let mut unread_ids = HashSet::new();
    let found_users =
        collect_unread_mark_ids(&data_node, &["convsUser", "convs_user"], false, &mut unread_ids);
    let found_groups =
        collect_unread_mark_ids(&data_node, &["convsGroup", "convs_group"], true, &mut unread_ids);
    if !found_users && !found_groups {
        return None;
    }
    Some(unread_ids)
}

/// Best-effort fetch of `/api/convers/get-unread-mark` on the `conversation` service host.
/// Propagates `SessionError::Expired`; falls back to `Ok(None)` on network errors or unknown schema.
pub async fn fetch_unread_marks(session: &Session) -> Result<Option<HashSet<String>>> {
    if !session.is_authenticated() || session.domains.conversation.is_empty() {
        return Ok(None);
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let url = format!(
        "{}/api/convers/get-unread-mark?{}&{params}",
        session.domains.conversation.trim_end_matches('/'),
        session.common_params()
    );
    let raw = match session.get_json(&url).await {
        Ok(v) => v,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => return Ok(None),
    };
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;
    Ok(parse_unread_marks_response(&resp))
}

fn apply_unread_marks_to_threads(
    threads: &mut [ThreadDto],
    unread_ids: &HashSet<String>,
    remote_read_known: &mut HashSet<String>,
) {
    // `/api/convers/get-unread-mark` tracks manual "Mark as Unread" flags (`add-unread-mark`),
    // NOT normal unread incoming messages. Absence from `unread_ids` therefore does NOT prove
    // a thread has 0 unread messages; only explicit read-state evidence from `get-last-msgs`
    // marks a thread authoritative for clearing local unread to 0.
    for t in threads.iter_mut() {
        let is_marked_unread =
            unread_ids.contains(&t.id) || unread_ids.contains(clean_group_id(&t.id));
        if is_marked_unread {
            t.unread_count = t.unread_count.max(1);
            remote_read_known.insert(t.id.clone());
        }
    }
}

async fn fetch_alias_map(session: &Session) -> Result<HashMap<String, String>> {
    if !session.is_authenticated() {
        return Ok(HashMap::new());
    }
    let body = obj(&[("page", json!(1)), ("count", json!(500)), ("imei", json!(session.imei))]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let path_and_query = format!("/api/alias/list?{}&{params}", session.common_params());
    let raw = session.get_endpoint("alias", &path_and_query).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;

    let mut aliases = HashMap::new();
    let data = resp.get("data").unwrap_or(&resp);
    let items = data
        .get("items")
        .or_else(|| data.get("list"))
        .or_else(|| data.as_array().map(|_| data))
        .and_then(Value::as_array);
    if let Some(arr) = items {
        for item in arr {
            if let (Some(uid), Some(alias)) = (
                pick_str(item, &["userId", "uid", "friendId", "id"]),
                pick_str(item, &["alias", "displayName", "name"]),
            ) {
                aliases.insert(uid, alias);
            }
        }
    }
    Ok(aliases)
}

/// Synchronize friends, groups, Cloud thread, pinned state, aliases, and latest conversation previews.
pub async fn sync_threads_full(session: &Session) -> Result<SyncedThreads> {
    let mut threads = match get_friends(session).await {
        Ok(list) => list,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => Vec::new(),
    };
    let mut seen: HashSet<String> = threads.iter().map(|t| t.id.clone()).collect();

    match get_groups(session).await {
        Ok(groups) => {
            for g in groups {
                if seen.insert(g.id.clone()) {
                    threads.push(g);
                }
            }
        }
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => {}
    }

    if let Ok(aliases) = fetch_alias_map(session).await {
        for t in &mut threads {
            if t.kind == ThreadKind::Direct {
                if let Some(alias) = aliases.get(&t.id) {
                    t.title = alias.clone();
                }
            }
        }
    }

    let mut extra_profiles: Map<String, Value> = Map::new();
    let mut name_map: HashMap<String, String> = HashMap::new();
    let last_msgs_opt = get_last_msgs(session).await.ok();

    if let Some(ref last_msgs) = last_msgs_opt {
        let mut need_uids: Vec<String> = Vec::new();
        let known_ids: HashSet<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        let data = last_msgs.get("data").unwrap_or(last_msgs);
        for bucket in ["msgs", "groupMsgs", "lastMsgs"] {
            if let Some(arr) = data.get(bucket).and_then(Value::as_array) {
                let in_group = bucket == "groupMsgs";
                for item in arr {
                    let uid_from =
                        pick_str(item, &["uidFrom", "fromUid", "fromId", "senderId"])
                            .unwrap_or_default();
                    let clean_sender = uid_from.split('_').next().unwrap_or(&uid_from);
                    if !clean_sender.is_empty()
                        && clean_sender != "0"
                        && clean_sender != session.profile.uid
                        && !known_ids.contains(clean_sender)
                        && !need_uids.iter().any(|x| x == clean_sender)
                    {
                        need_uids.push(clean_sender.to_string());
                    }
                    if let Some((tid, is_grp)) =
                        extract_thread_id(item, in_group, &session.profile.uid, &session.profile.send2me_id)
                    {
                        if !is_grp
                            && tid != session.profile.send2me_id
                            && !known_ids.contains(tid.as_str())
                            && !need_uids.iter().any(|x| x == &tid)
                        {
                            need_uids.push(tid);
                        }
                    }
                }
            }
        }
        for chunk in need_uids.chunks(50).take(3) {
            if let Ok(mp) = get_mini_profiles(session, chunk).await {
                for (k, v) in mp {
                    if let Some(name) =
                        pick_str(&v, &["displayName", "zaloName", "name", "display_name"])
                    {
                        name_map.insert(k.clone(), name);
                    }
                    extra_profiles.insert(k, v);
                }
            }
        }
    }

    let mut remote_read_known: HashSet<String> = HashSet::new();
    if let Some(ref last_msgs) = last_msgs_opt {
        remote_read_known = apply_last_msgs_to_threads_with_read_state(
            &mut threads,
            last_msgs,
            &session.profile.uid,
            &session.profile.send2me_id,
            &name_map,
        );
    }

    match fetch_unread_marks(session).await {
        Ok(Some(unread_ids)) => {
            apply_unread_marks_to_threads(&mut threads, &unread_ids, &mut remote_read_known);
        }
        Ok(None) => {}
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => {}
    }

    for t in &mut threads {
        if t.kind == ThreadKind::Direct {
            if let Some(p) = extra_profiles.get(&t.id) {
                if t.title == t.id {
                    if let Some(name) =
                        pick_str(p, &["displayName", "zaloName", "name", "display_name"])
                    {
                        t.title = name;
                    }
                }
                if t.avatar.is_empty() {
                    if let Some(avt) = pick_str(p, &["avatar", "avt", "profile_avatar"]) {
                        t.avatar = normalize_avatar_url(&avt);
                    }
                }
            }
        }
    }

    let mut pinned_synced = false;
    match fetch_pinned_thread_ids(session).await {
        Ok(pinned_ids) => {
            pinned_synced = true;
            for t in &mut threads {
                if pinned_ids.contains(clean_group_id(&t.id)) || pinned_ids.contains(&t.id) {
                    t.is_pinned = true;
                }
            }
        }
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => {}
    }

    sort_threads_by_recent(&mut threads);

    let mut friend_names = name_map;
    for t in &threads {
        if t.kind == ThreadKind::Direct && !t.title.is_empty() && t.title != t.id {
            friend_names
                .entry(t.id.clone())
                .or_insert_with(|| t.title.clone());
        }
    }

    Ok(SyncedThreads {
        threads,
        friend_names,
        pinned_synced,
        remote_read_known,
    })
}

pub async fn sync_threads(session: &Session) -> Result<Vec<ThreadDto>> {
    Ok(sync_threads_full(session).await?.threads)
}

pub fn sort_threads_by_recent(threads: &mut [ThreadDto]) {
    threads.sort_by(|a, b| {
        b.is_pinned
            .cmp(&a.is_pinned)
            .then_with(|| b.last_ts.cmp(&a.last_ts))
            .then_with(|| a.id.cmp(&b.id))
    });
}

#[derive(Debug, Clone)]
pub struct SyncedThreads {
    pub threads: Vec<ThreadDto>,
    pub friend_names: HashMap<String, String>,
    pub pinned_synced: bool,
    pub remote_read_known: HashSet<String>,
}

impl SyncedThreads {
    pub fn is_remote_read_authoritative(&self, thread_id: &str) -> bool {
        self.remote_read_known.contains(thread_id)
            || self.remote_read_known.contains(clean_group_id(thread_id))
    }
}

#[derive(Debug, Clone)]
pub struct RemoteHistoryPage {
    pub messages: Vec<MessageDto>,
    pub has_more: bool,
    pub error_code: i64,
}

fn value_as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

fn value_as_bool(v: &Value) -> Option<bool> {
    v.as_bool()
        .or_else(|| value_as_i64(v).map(|n| n != 0))
        .or_else(|| match v.as_str().map(str::trim) {
            Some("true") | Some("1") => Some(true),
            Some("false") | Some("0") => Some(false),
            _ => None,
        })
}

fn unwrap_history_data_node(resp: &Value) -> Option<Value> {
    match resp.get("data") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(v) => Some(v.clone()),
        None => None,
    }
}

pub fn extract_history_items(resp: &Value) -> Result<Vec<Value>> {
    let err_code = resp
        .get("error_code")
        .and_then(value_as_i64)
        .unwrap_or(0);
    if err_code == -69 {
        return Ok(Vec::new());
    }
    ensure_not_expired(resp)?;
    if err_code != 0 {
        let msg = resp
            .get("error_message")
            .and_then(|v| v.as_str())
            .unwrap_or("Không thể tải lịch sử tin nhắn");
        return Err(anyhow::anyhow!("{msg} (code {err_code})"));
    }
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return Ok(Vec::new());
    };
    if let Some(arr) = data_node.as_array() {
        return Ok(arr.clone());
    }
    for key in ["msgs", "groupMsgs", "list", "items"] {
        if let Some(arr) = data_node.get(key).and_then(|v| v.as_array()) {
            return Ok(arr.clone());
        }
    }
    Ok(Vec::new())
}

pub fn extract_history_has_more(resp: &Value, item_count: usize) -> bool {
    let err_code = resp
        .get("error_code")
        .and_then(value_as_i64)
        .unwrap_or(0);
    if err_code == -69 {
        return false;
    }
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return item_count > 0;
    };
    if let Some(filtered) = data_node
        .get("isFiltered")
        .or_else(|| data_node.get("is_filtered"))
        .and_then(value_as_bool)
    {
        if filtered {
            return true;
        }
    }
    for key in ["hasMore", "has_more", "more"] {
        if let Some(has_more) = data_node.get(key).and_then(value_as_bool) {
            return has_more;
        }
    }
    item_count > 0
}

pub fn normalize_vn_phone(raw: &str) -> Result<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')' | '\t'))
        .collect();
    let stripped = cleaned.strip_prefix('+').unwrap_or(&cleaned);
    if stripped.is_empty() || !stripped.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow::anyhow!("Số điện thoại không hợp lệ"));
    }
    let normalized = if let Some(rest) = stripped.strip_prefix("840") {
        format!("84{rest}")
    } else if let Some(rest) = stripped.strip_prefix('0') {
        format!("84{rest}")
    } else if stripped.starts_with("84") {
        stripped.to_string()
    } else if stripped.len() == 9 {
        format!("84{stripped}")
    } else {
        stripped.to_string()
    };
    if !(10..=15).contains(&normalized.len()) {
        return Err(anyhow::anyhow!("Số điện thoại phải gồm 10-11 chữ số"));
    }
    Ok(normalized)
}

pub fn parse_phone_profile_response(
    resp: &Value,
    fallback_phone: &str,
    self_uid: &str,
) -> Result<UserProfileDto> {
    ensure_not_expired(resp)?;
    let err_code = resp
        .get("error_code")
        .and_then(value_as_i64)
        .unwrap_or(0);
    if err_code == 216 || err_code == 212 {
        return Err(anyhow::anyhow!(
            "Không tìm thấy tài khoản nào với số điện thoại này"
        ));
    }
    if err_code == 221 {
        return Err(anyhow::anyhow!(
            "Bạn đã tìm kiếm quá nhanh, vui lòng thử lại sau ít phút"
        ));
    }
    if err_code != 0 {
        let msg = resp
            .get("error_message")
            .and_then(|v| v.as_str())
            .unwrap_or("Không tìm thấy người dùng");
        return Err(anyhow::anyhow!("{msg}"));
    }
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return Err(anyhow::anyhow!(
            "Không tìm thấy tài khoản nào với số điện thoại này"
        ));
    };
    let uid = pick_str(&data_node, &["uid", "userId", "id"]).ok_or_else(|| {
        anyhow::anyhow!("Không tìm thấy tài khoản nào với số điện thoại này")
    })?;
    let display_name = pick_str(&data_node, &["displayName", "zaloName", "name", "display_name"])
        .unwrap_or_else(|| fallback_phone.to_string());
    let zalo_name = pick_str(&data_node, &["zaloName", "displayName", "name"])
        .unwrap_or_else(|| display_name.clone());
    let avatar = pick_str(&data_node, &["avatar", "avt", "profile_avatar"])
        .map(|a| normalize_avatar_url(&a))
        .unwrap_or_default();
    let cover = pick_str(&data_node, &["cover", "bgavatar", "bgurl"])
        .map(|c| normalize_media_url(&c))
        .filter(|u| !u.is_empty() && !u.trim_end_matches('/').ends_with("/default"))
        .unwrap_or_default();
    let status = pick_str(&data_node, &["status", "bio"]).unwrap_or_default();
    let sdob = pick_str(&data_node, &["sdob", "dobStr", "dob"]).unwrap_or_default();
    let gender = data_node
        .get("gender")
        .and_then(value_as_i64)
        .and_then(|g| u8::try_from(g).ok());
    let phone_number = pick_str(&data_node, &["phoneNumber", "phone"])
        .unwrap_or_else(|| fallback_phone.to_string());
    let is_friend = data_node
        .get("isFr")
        .or_else(|| data_node.get("isFriend"))
        .and_then(value_as_bool);
    let is_blocked = data_node
        .get("isBlocked")
        .or_else(|| data_node.get("is_blocked"))
        .and_then(value_as_bool);
    let is_self = !self_uid.is_empty() && uid == self_uid;
    Ok(UserProfileDto {
        uid,
        display_name,
        zalo_name,
        avatar,
        cover,
        status,
        gender,
        sdob,
        phone_number,
        is_friend,
        is_blocked,
        is_self,
        mutual_groups: Vec::new(),
        local_message_count: 0,
        local_media_count: 0,
        last_interaction_ts: 0,
    })
}

pub fn parse_pinned_board_response(
    resp: &Value,
    thread_id: &str,
    self_uid: &str,
    send2me: &str,
    is_group: bool,
    friend_names: &HashMap<String, String>,
) -> Vec<MessageDto> {
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return Vec::new();
    };
    let mut raw_items: Vec<Value> = Vec::new();
    if let Some(arr) = data_node.as_array() {
        raw_items.extend(arr.iter().cloned());
    } else {
        for key in ["items", "list", "msgs", "boardItems", "topics", "topic"] {
            if let Some(node) = data_node.get(key) {
                if let Some(arr) = node.as_array() {
                    raw_items.extend(arr.iter().cloned());
                } else if node.is_object() {
                    raw_items.push(node.clone());
                }
            }
        }
    }
    let mut out: Vec<MessageDto> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in raw_items {
        let mut parsed_msg: Option<MessageDto> = None;
        if let Some(inner) = item.get("msg").or_else(|| item.get("message")).filter(|v| v.is_object()) {
            parsed_msg = parse_chat_message(inner, Some(thread_id), self_uid, send2me, is_group);
        }
        if parsed_msg.is_none()
            && item.get("content").is_some()
            && (item.get("msgId").is_some() || item.get("cliMsgId").is_some())
        {
            parsed_msg = parse_chat_message(&item, Some(thread_id), self_uid, send2me, is_group);
        }
        let mut dto = if let Some(mut m) = parsed_msg {
            m.is_pinned = true;
            m
        } else {
            let params_obj = match item.get("params") {
                Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
                Some(v) if v.is_object() => v.clone(),
                _ => Value::Null,
            };
            let msg_id = pick_str(
                &params_obj,
                &["msgId", "globalMsgId", "cliMsgId", "clientMsgId", "id"],
            )
            .or_else(|| pick_str(&item, &["msgId", "globalMsgId", "cliMsgId", "id", "topicId"]))
            .unwrap_or_default();
            if msg_id.is_empty() {
                continue;
            }
            let sender_id = pick_str(&params_obj, &["senderUid", "uidFrom", "creatorId"])
                .or_else(|| pick_str(&item, &["creatorId", "uidFrom", "senderId", "senderUid"]))
                .unwrap_or_default();
            let mut sender_name = pick_str(&params_obj, &["senderName", "dName"])
                .or_else(|| pick_str(&item, &["creatorName", "dName", "senderName"]))
                .unwrap_or_default();
            if sender_name.is_empty() && !sender_id.is_empty() {
                if sender_id == self_uid {
                    sender_name = "Bạn".to_string();
                } else if let Some(name) = friend_names.get(&sender_id) {
                    sender_name = name.clone();
                }
            }
            let text = pick_str(
                &params_obj,
                &["title", "message", "msg", "text", "content", "description"],
            )
            .or_else(|| pick_str(&item, &["title", "content", "text", "message", "description"]))
            .unwrap_or_default();
            let msg_type = pick_str(&params_obj, &["msgType", "msg_type"])
                .or_else(|| pick_str(&item, &["msgType", "msg_type"]))
                .unwrap_or_else(|| "chat.text".to_string());
            let is_link_or_card = matches!(
                msg_type.to_ascii_lowercase().as_str(),
                "chat.link"
                    | "link"
                    | "chat.location"
                    | "chat.location.new"
                    | "chat.recommended"
                    | "chat.ecard"
            );
            let attachment_url = if is_link_or_card {
                pick_str(&params_obj, &["thumb", "thumbUrl"])
                    .or_else(|| pick_str(&item, &["thumb", "thumbUrl"]))
                    .and_then(|u| crate::session::normalize_allowed_media_url(&u).ok())
                    .map(|u| u.to_string())
            } else {
                pick_str(&params_obj, &["oriUrl", "hdUrl", "fileUrl", "href", "url", "thumb"])
                    .or_else(|| {
                        pick_str(&item, &["oriUrl", "hdUrl", "fileUrl", "href", "url", "thumb"])
                    })
                    .map(|u| normalize_media_url(&u))
                    .filter(|u| !u.is_empty())
            };
            let attachment_name = pick_str(&params_obj, &["fileName", "title"])
                .filter(|_| attachment_url.is_some());
            if text.is_empty() && attachment_url.is_none() {
                continue;
            }
            let raw_ts = params_obj
                .get("ts")
                .or_else(|| params_obj.get("createTime"))
                .or_else(|| item.get("createTime"))
                .or_else(|| item.get("editTime"))
                .or_else(|| item.get("ts"))
                .or_else(|| item.get("time"))
                .and_then(value_as_i64)
                .unwrap_or(0);
            let ts = normalize_ts_ms(raw_ts);
            let is_outgoing = !self_uid.is_empty() && sender_id == self_uid;
            let cli_msg_id = pick_str(&params_obj, &["cliMsgId", "clientMsgId"])
                .or_else(|| pick_str(&item, &["cliMsgId", "clientMsgId"]))
                .filter(|s| s != "0");
            MessageDto {
                msg_id,
                cli_msg_id,
                thread_id: thread_id.to_string(),
                sender_id,
                sender_name,
                text,
                msg_type,
                ts,
                is_outgoing,
                is_recalled: false,
                is_pinned: true,
                quote: None,
                reactions: Vec::new(),
                attachment_url,
                attachment_name,
            }
        };
        if !self_uid.is_empty() && dto.sender_id == self_uid {
            dto.is_outgoing = true;
            if dto.sender_name.is_empty() {
                dto.sender_name = "Bạn".to_string();
            }
        } else if dto.sender_name.is_empty() {
            if let Some(name) = friend_names.get(&dto.sender_id) {
                dto.sender_name = name.clone();
            }
        }
        if seen.insert(dto.msg_id.clone()) {
            out.push(dto);
        }
    }
    out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));
    out
}

fn extract_last_msg_read_state(item: &Value, is_outgoing: bool, my_uid: &str) -> (u32, bool) {
    if is_outgoing {
        return (0, true);
    }
    for key in ["unread", "unreadCount", "totalUnread", "unread_count"] {
        if let Some(v) = item.get(key) {
            if let Some(n) = value_as_i64(v) {
                return (n.max(0) as u32, true);
            }
            if let Some(b) = value_as_bool(v) {
                return (u32::from(b), true);
            }
        }
    }
    for key in ["isUnread", "is_unread"] {
        if let Some(b) = item.get(key).and_then(value_as_bool) {
            return (u32::from(b), true);
        }
    }
    for key in ["seen", "isSeen", "is_seen", "isRead", "is_read", "read"] {
        if let Some(b) = item.get(key).and_then(value_as_bool) {
            return (if b { 0 } else { 1 }, true);
        }
    }
    if !my_uid.is_empty() {
        if let Some(arr) = item
            .get("seenUids")
            .or_else(|| item.get("seen_uids"))
            .and_then(Value::as_array)
        {
            let self_seen = arr.iter().any(|v| {
                let s = match v {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                };
                let clean = s.split('_').next().unwrap_or(s.as_str());
                !clean.is_empty() && (s == my_uid || clean == my_uid)
            });
            if self_seen {
                return (0, true);
            }
        }
    }
    (0, false)
}

pub fn apply_last_msgs_to_threads(
    threads: &mut Vec<ThreadDto>,
    last_msgs: &Value,
    my_uid: &str,
    send2me_id: &str,
) {
    let _ = apply_last_msgs_to_threads_with_read_state(
        threads,
        last_msgs,
        my_uid,
        send2me_id,
        &HashMap::new(),
    );
}

pub fn apply_last_msgs_to_threads_with_names(
    threads: &mut Vec<ThreadDto>,
    last_msgs: &Value,
    my_uid: &str,
    send2me_id: &str,
    extra_names: &HashMap<String, String>,
) {
    let _ = apply_last_msgs_to_threads_with_read_state(
        threads,
        last_msgs,
        my_uid,
        send2me_id,
        extra_names,
    );
}

pub fn apply_last_msgs_to_threads_with_read_state(
    threads: &mut Vec<ThreadDto>,
    last_msgs: &Value,
    my_uid: &str,
    send2me_id: &str,
    extra_names: &HashMap<String, String>,
) -> HashSet<String> {
    let mut friend_names: HashMap<String, String> = extra_names.clone();
    for t in threads.iter() {
        if t.kind == ThreadKind::Direct && !t.title.is_empty() && t.title != t.id {
            friend_names.entry(t.id.clone()).or_insert_with(|| t.title.clone());
        }
    }

    struct PreviewInfo {
        preview: String,
        ts: i64,
        is_grp: bool,
        is_outgoing: bool,
        unread: u32,
        read_state_known: bool,
        fallback_title: Option<String>,
    }

    let mut previews: HashMap<String, PreviewInfo> = HashMap::new();
    let data = last_msgs.get("data").unwrap_or(last_msgs);
    for bucket in ["msgs", "groupMsgs", "lastMsgs"] {
        if let Some(arr) = data.get(bucket).and_then(Value::as_array) {
            let in_group = bucket == "groupMsgs";
            for item in arr {
                let parsed = parse_chat_message(item, None, my_uid, send2me_id, in_group);
                let (tid, is_grp) = match extract_thread_id(item, in_group, my_uid, send2me_id) {
                    Some(v) => v,
                    None => match &parsed {
                        Some(msg) => (msg.thread_id.clone(), in_group),
                        None => continue,
                    },
                };
                if tid.is_empty() || tid == "0" {
                    continue;
                }
                let raw_ts = item
                    .get("ts")
                    .or_else(|| item.get("cliMsgId"))
                    .or_else(|| item.get("sendTime"))
                    .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                    .map(normalize_ts_ms)
                    .unwrap_or(0);
                let ts = parsed.as_ref().map(|m| m.ts).filter(|&t| t > 0).unwrap_or(raw_ts);
                let raw_text = item
                    .get("content")
                    .and_then(extract_content_text)
                    .or_else(|| pick_str(item, &["msg", "message"]))
                    .or_else(|| parsed.as_ref().map(|m| m.text.clone()))
                    .unwrap_or_default();
                let uid_from = pick_str(item, &["uidFrom", "fromUid", "fromId", "senderId"])
                    .unwrap_or_default();
                let clean_sender = uid_from.split('_').next().unwrap_or(&uid_from);
                let is_self_flag = [item.get("isSelf"), item.get("is_self")]
                    .into_iter()
                    .flatten()
                    .any(|v| {
                        v.as_bool() == Some(true)
                            || v.as_i64() == Some(1)
                            || v.as_str() == Some("1")
                    });
                let is_outgoing = parsed.as_ref().map(|m| m.is_outgoing).unwrap_or_else(|| {
                    is_self_flag
                        || clean_sender == "0"
                        || clean_sender.is_empty()
                        || (!my_uid.is_empty() && clean_sender == my_uid)
                        || (!send2me_id.is_empty() && clean_sender == send2me_id)
                });
                let mut sender_name = parsed
                    .as_ref()
                    .map(|m| m.sender_name.clone())
                    .or_else(|| pick_str(item, &["dName", "displayName", "fromName", "senderName"]))
                    .unwrap_or_default();
                if !is_outgoing && (sender_name.is_empty() || sender_name == uid_from || is_raw_numeric_id(&sender_name)) {
                    if let Some(resolved) = friend_names.get(clean_sender) {
                        sender_name = resolved.clone();
                    }
                }
                let preview = format_thread_preview(&raw_text, is_outgoing, &sender_name, is_grp);
                let (unread, read_state_known) =
                    extract_last_msg_read_state(item, is_outgoing, my_uid);
                let fallback_title = if is_grp {
                    pick_str(item, &["groupName", "name"])
                } else if !is_outgoing {
                    pick_str(item, &["dName", "displayName", "senderName", "name"])
                        .or_else(|| friend_names.get(&tid).cloned())
                } else {
                    friend_names.get(&tid).cloned()
                };

                match previews.get_mut(&tid) {
                    Some(existing) => {
                        if ts > existing.ts
                            || (ts == existing.ts && existing.preview.is_empty() && !preview.is_empty())
                        {
                            existing.ts = ts;
                            if !preview.is_empty() {
                                existing.preview = preview;
                            }
                            existing.is_grp |= is_grp;
                            existing.is_outgoing = is_outgoing;
                            if is_outgoing {
                                existing.unread = 0;
                                existing.read_state_known = true;
                            } else if read_state_known {
                                existing.unread = unread;
                                existing.read_state_known = true;
                            } else {
                                existing.unread = existing.unread.max(unread);
                                existing.read_state_known = false;
                            }
                            if existing.fallback_title.is_none() {
                                existing.fallback_title = fallback_title;
                            }
                        }
                    }
                    None => {
                        previews.insert(
                            tid,
                            PreviewInfo {
                                preview,
                                ts,
                                is_grp,
                                is_outgoing,
                                unread,
                                read_state_known,
                                fallback_title,
                            },
                        );
                    }
                }
            }
        }
    }

    let mut remote_read_known: HashSet<String> = HashSet::new();
    let mut seen_ids: HashSet<String> = HashSet::with_capacity(threads.len());
    for t in threads.iter_mut() {
        seen_ids.insert(t.id.clone());
        if let Some(info) = previews.get(&t.id) {
            let supersedes = info.ts >= t.last_ts;
            if supersedes && !info.preview.is_empty() {
                t.last_preview = info.preview.clone();
            }
            if info.ts > t.last_ts {
                t.last_ts = info.ts;
            }
            if info.is_outgoing && supersedes {
                t.unread_count = 0;
                remote_read_known.insert(t.id.clone());
            } else if info.read_state_known && supersedes {
                t.unread_count = info.unread;
                remote_read_known.insert(t.id.clone());
            } else if info.unread > t.unread_count {
                t.unread_count = info.unread;
            }
        }
    }

    for (tid, info) in previews {
        if seen_ids.contains(&tid) || (info.preview.is_empty() && info.ts <= 0) {
            continue;
        }
        if info.read_state_known {
            remote_read_known.insert(tid.clone());
        }
        let kind = if info.is_grp {
            ThreadKind::Group
        } else if !send2me_id.is_empty() && tid == send2me_id {
            ThreadKind::Cloud
        } else {
            ThreadKind::Direct
        };
        threads.push(ThreadDto {
            id: tid.clone(),
            title: info.fallback_title.unwrap_or(tid),
            avatar: String::new(),
            kind,
            last_preview: info.preview,
            last_ts: info.ts,
            unread_count: info.unread,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        });
    }

    sort_threads_by_recent(threads);
    remote_read_known
}

fn fold_reaction_action_into_message(
    msg: &mut MessageDto,
    actors: &mut HashMap<String, String>,
    actor_id: Option<&str>,
    emoji: Option<&str>,
    me: bool,
    removed: bool,
) {
    let actor_key = if me {
        Some("__me__".to_string())
    } else {
        actor_id
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .map(|s| format!("u:{s}"))
    };

    if removed {
        if me {
            actors.remove("__me__");
            if let Some(pos) = msg
                .reactions
                .iter()
                .position(|r| r.me && emoji.is_none_or(|t| r.emoji == t))
            {
                msg.reactions[pos].me = false;
                msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                if msg.reactions[pos].count == 0 {
                    msg.reactions.remove(pos);
                }
            }
        } else {
            let target = actor_key
                .as_deref()
                .and_then(|k| actors.remove(k))
                .or_else(|| emoji.map(str::to_string));
            if let Some(t) = target {
                if let Some(pos) = msg
                    .reactions
                    .iter()
                    .position(|r| r.emoji == t && r.count > u32::from(r.me))
                {
                    msg.reactions[pos].count = msg.reactions[pos].count.saturating_sub(1);
                    if msg.reactions[pos].count == 0 {
                        msg.reactions.remove(pos);
                    }
                }
            }
        }
        return;
    }

    let Some(target) = emoji.filter(|s| !s.trim().is_empty()) else {
        return;
    };

    if me {
        if msg.reactions.iter().any(|r| r.emoji == target && r.me) {
            actors.insert("__me__".to_string(), target.to_string());
            return;
        }
        if let Some(prev_pos) = msg.reactions.iter().position(|r| r.me) {
            msg.reactions[prev_pos].me = false;
            msg.reactions[prev_pos].count = msg.reactions[prev_pos].count.saturating_sub(1);
            if msg.reactions[prev_pos].count == 0 {
                msg.reactions.remove(prev_pos);
            }
        }
        actors.insert("__me__".to_string(), target.to_string());
    } else if let Some(ref ak) = actor_key {
        if actors.get(ak).map(String::as_str) == Some(target) {
            return;
        }
        if let Some(prev_emoji) = actors.get(ak).cloned() {
            if let Some(prev_pos) = msg
                .reactions
                .iter()
                .position(|r| r.emoji == prev_emoji && r.count > u32::from(r.me))
            {
                msg.reactions[prev_pos].count = msg.reactions[prev_pos].count.saturating_sub(1);
                if msg.reactions[prev_pos].count == 0 {
                    msg.reactions.remove(prev_pos);
                }
            }
        }
        actors.insert(ak.clone(), target.to_string());
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
}

fn parse_remote_history_page(
    resp: &Value,
    thread_id: &str,
    my_uid: &str,
    send2me: &str,
    is_group: bool,
    exclude_msg_id: Option<&str>,
) -> Result<RemoteHistoryPage> {
    let error_code = resp
        .get("error_code")
        .and_then(value_as_i64)
        .unwrap_or(0);
    let items = extract_history_items(resp)?;
    let raw_count = items.len();
    let has_more = extract_history_has_more(resp, raw_count);
    let mut reaction_actions: Vec<WsParsedAction> = Vec::new();
    let mut msgs: Vec<MessageDto> = Vec::new();
    for item in &items {
        if pick_str(item, &["msgType", "type"])
            .is_some_and(|t| t.eq_ignore_ascii_case("chat.reaction"))
        {
            parse_reaction_item(item, is_group, my_uid, send2me, &mut reaction_actions);
            continue;
        }
        if let Some(msg) = parse_chat_message(item, Some(thread_id), my_uid, send2me, is_group) {
            if exclude_msg_id.map(|ex| msg.msg_id != ex).unwrap_or(true) {
                msgs.push(msg);
            }
        }
    }
    if let Some(data_node) = unwrap_history_data_node(resp) {
        if data_node.get("reacts").is_some() || data_node.get("reactGroups").is_some() {
            collect_reactions_from_node(&data_node, is_group, my_uid, send2me, &mut reaction_actions);
        }
    }
    if !reaction_actions.is_empty() && !msgs.is_empty() {
        let mut actor_maps: Vec<HashMap<String, String>> = vec![HashMap::new(); msgs.len()];
        for act in reaction_actions {
            if let WsParsedAction::Reaction {
                msg_id,
                cli_msg_id,
                actor_id,
                emoji,
                me,
                removed,
                ..
            } = act
            {
                let target_idx = msgs.iter().position(|m| {
                    m.msg_id == msg_id
                        || m.cli_msg_id.as_deref() == Some(msg_id.as_str())
                        || cli_msg_id.as_deref().is_some_and(|cid| {
                            m.cli_msg_id.as_deref() == Some(cid) || m.msg_id == cid
                        })
                });
                if let Some(idx) = target_idx {
                    fold_reaction_action_into_message(
                        &mut msgs[idx],
                        &mut actor_maps[idx],
                        actor_id.as_deref(),
                        emoji.as_deref(),
                        me,
                        removed,
                    );
                }
            }
        }
    }
    msgs.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)));
    Ok(RemoteHistoryPage {
        messages: msgs,
        has_more,
        error_code,
    })
}

pub async fn fetch_thread_history_page(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    count: u32,
) -> Result<RemoteHistoryPage> {
    if !session.is_authenticated() {
        return Ok(RemoteHistoryPage {
            messages: Vec::new(),
            has_more: false,
            error_code: 0,
        });
    }
    let clean = clean_group_id(thread_id);
    let my_uid = session.profile.uid.as_str();
    let send2me = session.profile.send2me_id.as_str();

    let mut m = Map::new();
    m.insert("groupId".into(), json!(clean));
    m.insert("globalMsgId".into(), json!("9223372036854775808"));
    m.insert("count".into(), json!(count));
    m.insert("msgIds".into(), json!([]));
    m.insert("imei".into(), json!(session.imei));
    if kind == ThreadKind::Cloud || clean == send2me {
        m.insert("isOA".into(), json!(1));
    }
    m.insert("src".into(), json!(1));
    let params = session.encoded_params(&Value::Object(m))?;
    let host = if session.domains.cloud_msg.is_empty() {
        crate::endpoints::CLOUD_MSG_FALLBACK
    } else {
        session.domains.cloud_msg.trim_end_matches('/')
    };
    let url = format!(
        "{host}/api/cm/getrecentv2?{}&params={params}&nretry=0",
        session.common_params()
    );
    let raw = session.get_json(&url).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    parse_remote_history_page(&resp, thread_id, my_uid, send2me, kind == ThreadKind::Group, None)
}

pub async fn fetch_thread_history(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    count: u32,
) -> Result<Vec<MessageDto>> {
    Ok(fetch_thread_history_page(session, thread_id, kind, count)
        .await?
        .messages)
}

/// Fetch older messages strictly before `before_msg_id` via `/api/cm/getoldv2`.
pub async fn fetch_older_thread_history_page(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    before_msg_id: &str,
    count: u32,
) -> Result<RemoteHistoryPage> {
    if !session.is_authenticated()
        || before_msg_id.is_empty()
        || before_msg_id.starts_with("local-")
        || before_msg_id.starts_with("out_")
    {
        return Ok(RemoteHistoryPage {
            messages: Vec::new(),
            has_more: false,
            error_code: 0,
        });
    }
    let clean = clean_group_id(thread_id);
    let my_uid = session.profile.uid.as_str();
    let send2me = session.profile.send2me_id.as_str();

    let mut m = Map::new();
    m.insert("groupId".into(), json!(clean));
    m.insert("globalMsgId".into(), json!(before_msg_id));
    m.insert("count".into(), json!(count));
    m.insert("msgIds".into(), json!([]));
    m.insert("imei".into(), json!(session.imei));
    if kind == ThreadKind::Cloud || clean == send2me {
        m.insert("isOA".into(), json!(1));
    }
    m.insert("src".into(), json!(3));

    let params = session.encoded_params(&Value::Object(m))?;
    let host = if session.domains.cloud_msg.is_empty() {
        crate::endpoints::CLOUD_MSG_FALLBACK
    } else {
        session.domains.cloud_msg.trim_end_matches('/')
    };
    let url = format!(
        "{host}/api/cm/getoldv2?{}&params={params}&nretry=0",
        session.common_params()
    );
    let raw = session.get_json(&url).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    parse_remote_history_page(
        &resp,
        thread_id,
        my_uid,
        send2me,
        kind == ThreadKind::Group,
        Some(before_msg_id),
    )
}

pub async fn fetch_older_thread_history(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    before_msg_id: &str,
    count: u32,
) -> Result<Vec<MessageDto>> {
    Ok(
        fetch_older_thread_history_page(session, thread_id, kind, before_msg_id, count)
            .await?
            .messages,
    )
}

fn clean_uid(raw: &str) -> String {
    raw.trim()
        .split('_')
        .next()
        .unwrap_or(raw.trim())
        .trim()
        .to_string()
}

fn extract_group_roles_and_member_ids(
    info: &Value,
) -> (String, Vec<String>, Vec<String>, Option<u32>) {
    let creator_id = pick_str(info, &["creatorId"])
        .map(|s| clean_uid(&s))
        .unwrap_or_default();
    let mut admin_ids: Vec<String> = Vec::new();
    if let Some(admins) = info.get("adminIds").and_then(Value::as_array) {
        for a in admins {
            if let Some(raw) = a
                .as_str()
                .map(str::to_string)
                .or_else(|| a.as_u64().map(|n| n.to_string()))
            {
                let aid = clean_uid(&raw);
                if !aid.is_empty() && !admin_ids.iter().any(|x| x == &aid) {
                    admin_ids.push(aid);
                }
            }
        }
    }

    let mut ordered_ids: Vec<String> = Vec::new();
    if !creator_id.is_empty() {
        ordered_ids.push(creator_id.clone());
    }
    for aid in &admin_ids {
        if !ordered_ids.iter().any(|x| x == aid) {
            ordered_ids.push(aid.clone());
        }
    }
    if let Some(arr) = info.get("memVerList").and_then(Value::as_array) {
        for item in arr {
            let raw = item
                .as_str()
                .map(str::to_string)
                .or_else(|| item.as_u64().map(|n| n.to_string()));
            if let Some(s) = raw {
                let uid = clean_uid(&s);
                if !uid.is_empty() && !ordered_ids.iter().any(|x| x == &uid) {
                    ordered_ids.push(uid);
                }
            }
        }
    }

    let total_members = info
        .get("totalMember")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .or_else(|| (!ordered_ids.is_empty()).then_some(ordered_ids.len() as u32));

    (creator_id, admin_ids, ordered_ids, total_members)
}

pub fn build_group_details_from_info(
    info: &Value,
    profiles: &Map<String, Value>,
    self_uid: &str,
    self_display_name: &str,
    self_avatar: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> GroupDetailsResult {
    let (creator_id, admin_ids, ordered_ids, total_members) =
        extract_group_roles_and_member_ids(info);
    let sample_ids: Vec<String> = ordered_ids.into_iter().take(100).collect();

    let members = sample_ids
        .into_iter()
        .map(|uid| {
            let p = profiles.get(&uid);
            let mut name = p
                .and_then(|v| pick_str(v, &["displayName", "zaloName", "name", "display_name"]))
                .unwrap_or_default();
            let mut avatar = p
                .and_then(|v| pick_str(v, &["avatar", "avt", "profile_avatar"]))
                .map(|u| normalize_avatar_url(&u))
                .unwrap_or_default();

            if !self_uid.is_empty() && uid == self_uid {
                if name.is_empty() && !self_display_name.is_empty() {
                    name = format!("{self_display_name} (Bạn)");
                }
                if avatar.is_empty() && !self_avatar.is_empty() {
                    avatar = normalize_avatar_url(self_avatar);
                }
            }
            if let Some((local_name, local_ava)) = known_profiles.get(&uid) {
                if name.is_empty() && !local_name.is_empty() && local_name != &uid {
                    name = local_name.clone();
                }
                if avatar.is_empty() && !local_ava.is_empty() {
                    avatar = local_ava.clone();
                }
            }
            if name.is_empty() {
                name = uid.clone();
            }

            let role = if uid == creator_id {
                "Trưởng nhóm".to_string()
            } else if admin_ids.iter().any(|aid| aid == &uid) {
                "Phó nhóm".to_string()
            } else {
                "Thành viên".to_string()
            };
            ThreadMemberDto {
                id: uid,
                name,
                avatar,
                role,
            }
        })
        .collect();

    GroupDetailsResult {
        total_members,
        members,
        creator_id,
        admin_ids,
    }
}

pub async fn fetch_group_details(
    session: &Session,
    thread_id: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> Result<GroupDetailsResult> {
    if !session.is_authenticated() {
        return Ok(GroupDetailsResult::default());
    }
    let clean = clean_group_id(thread_id).to_string();
    let (info_map, _) = fetch_group_profiles(session, std::slice::from_ref(&clean)).await?;
    let Some(info) = info_map.get(&clean).or_else(|| info_map.get(thread_id)) else {
        return Ok(GroupDetailsResult::default());
    };

    let (_, _, ordered_ids, _) = extract_group_roles_and_member_ids(info);
    let sample_ids: Vec<String> = ordered_ids.into_iter().take(100).collect();
    let mut profiles = Map::new();
    for chunk in sample_ids.chunks(50) {
        if let Ok(mp) = get_mini_profiles(session, chunk).await {
            for (k, v) in mp {
                profiles.insert(k, v);
            }
        }
    }

    Ok(build_group_details_from_info(
        info,
        &profiles,
        &session.profile.uid,
        &session.profile.display_name,
        &session.profile.avatar,
        known_profiles,
    ))
}

pub fn build_user_profile_dto(
    clean_uid_str: &str,
    remote_val: Option<&Value>,
    self_uid: &str,
    self_display_name: &str,
    self_avatar: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> UserProfileDto {
    let is_self = !self_uid.is_empty() && clean_uid_str == self_uid;
    let rv = remote_val;
    let mut display_name = rv
        .and_then(|v| pick_str(v, &["displayName", "alias", "zaloName", "name", "display_name"]))
        .unwrap_or_default();
    let zalo_name = rv
        .and_then(|v| pick_str(v, &["zaloName", "displayName"]))
        .unwrap_or_default();
    let mut avatar = rv
        .and_then(|v| pick_str(v, &["avatar", "avt", "profile_avatar"]))
        .map(|u| normalize_avatar_url(&u))
        .unwrap_or_default();
    let cover = rv
        .and_then(|v| pick_str(v, &["cover", "bgavatar"]))
        .map(|u| normalize_media_url(&u))
        .filter(|u| !u.is_empty() && !u.trim_end_matches('/').ends_with("/default"))
        .unwrap_or_default();
    let status = rv
        .and_then(|v| pick_str(v, &["status", "bio"]))
        .unwrap_or_default();
    let gender = rv
        .and_then(|v| v.get("gender").and_then(Value::as_u64))
        .map(|g| g as u8);
    let sdob = rv
        .and_then(|v| pick_str(v, &["sdob", "dobStr"]))
        .unwrap_or_default();
    let phone_number = rv
        .and_then(|v| pick_str(v, &["phoneNumber", "phone"]))
        .unwrap_or_default();
    let is_friend = rv
        .and_then(|v| v.get("isFr").or_else(|| v.get("isFriend")))
        .and_then(value_as_bool);
    let is_blocked = rv
        .and_then(|v| v.get("isBlocked").or_else(|| v.get("is_blocked")))
        .and_then(value_as_bool);

    if is_self {
        if display_name.is_empty() && !self_display_name.is_empty() {
            display_name = self_display_name.to_string();
        }
        if avatar.is_empty() && !self_avatar.is_empty() {
            avatar = normalize_avatar_url(self_avatar);
        }
    }
    if let Some((local_name, local_ava)) = known_profiles.get(clean_uid_str) {
        if display_name.is_empty() && !local_name.is_empty() && local_name != clean_uid_str {
            display_name = local_name.clone();
        }
        if avatar.is_empty() && !local_ava.is_empty() {
            avatar = local_ava.clone();
        }
    }
    if display_name.is_empty() {
        display_name = clean_uid_str.to_string();
    }

    UserProfileDto {
        uid: clean_uid_str.to_string(),
        display_name,
        zalo_name,
        avatar,
        cover,
        status,
        gender,
        sdob,
        phone_number,
        is_friend,
        is_blocked,
        is_self,
        mutual_groups: Vec::new(),
        local_message_count: 0,
        local_media_count: 0,
        last_interaction_ts: 0,
    }
}

pub async fn fetch_user_profile(
    session: &Session,
    user_id: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> Result<UserProfileDto> {
    let clean = clean_uid(user_id);

    let mut remote_val: Option<Value> = None;
    if session.is_authenticated() && !clean.is_empty() {
        let mut profile_hosts = session.domains.candidates("profile");
        for extra in ["https://profile-wpa.zaloapp.com", "https://tt-profile-wpa.chat.zalo.me"] {
            if !profile_hosts.iter().any(|h| h == extra) {
                profile_hosts.push(extra.to_string());
            }
        }
        let body = obj(&[
            ("friend_pversion_map", json!([format!("{clean}_0")])),
            ("avatar_size", json!(240)),
            ("language", json!("vi")),
        ]);
        if let Ok(params) = session.encoded_params(&body) {
            for host in &profile_hosts {
                let url = format!(
                    "{}/api/social/friend/getprofiles/v2?{}",
                    host.trim_end_matches('/'),
                    session.common_params()
                );
                if let Ok(raw) = session.post_form(&url, format!("params={params}")).await {
                    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                    ensure_not_expired(&resp)?;
                    let map = parse_profiles_response(&resp);
                    if let Some(found) = map.get(&clean) {
                        remote_val = Some(found.clone());
                        break;
                    }
                }
            }
        }
        if remote_val.is_none() {
            if let Ok(mp) = get_mini_profiles(session, std::slice::from_ref(&clean)).await {
                if let Some(found) = mp.get(&clean) {
                    remote_val = Some(found.clone());
                }
            }
        }
    }

    Ok(build_user_profile_dto(
        &clean,
        remote_val.as_ref(),
        &session.profile.uid,
        &session.profile.display_name,
        &session.profile.avatar,
        known_profiles,
    ))
}

pub fn group_info_has_member(info: &Value, target_uid: &str) -> bool {
    let uid = target_uid.trim().split('_').next().unwrap_or("");
    if uid.is_empty() {
        return false;
    }
    if pick_str(info, &["creatorId"]).as_deref() == Some(uid) {
        return true;
    }
    for key in ["adminIds", "memVerList", "memberIds"] {
        if let Some(arr) = info.get(key).and_then(Value::as_array) {
            for item in arr {
                let raw = item
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| item.as_u64().map(|n| n.to_string()))
                    .unwrap_or_default();
                if raw.trim().split('_').next() == Some(uid) {
                    return true;
                }
            }
        }
    }
    false
}

pub async fn fetch_mutual_groups(
    session: &Session,
    target_uid: &str,
    group_threads: &[ThreadDto],
) -> Vec<MutualGroupDto> {
    if !session.is_authenticated() || target_uid.trim().is_empty() || group_threads.is_empty() {
        return Vec::new();
    }
    let slice: Vec<&ThreadDto> = group_threads.iter().take(60).collect();
    let clean_ids: Vec<String> = slice
        .iter()
        .map(|t| clean_group_id(&t.id).to_string())
        .collect();
    let Ok((info_map, _)) = fetch_group_profiles(session, &clean_ids).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for t in slice {
        let clean = clean_group_id(&t.id);
        let Some(info) = info_map.get(clean).or_else(|| info_map.get(&t.id)) else {
            continue;
        };
        if group_info_has_member(info, target_uid) {
            let total_members = info
                .get("totalMember")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
            out.push(MutualGroupDto {
                id: t.id.clone(),
                title: t.title.clone(),
                avatar: t.avatar.clone(),
                total_members,
            });
        }
    }
    out
}

pub async fn find_user_by_phone(session: &Session, raw_phone: &str) -> Result<UserProfileDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let phone = normalize_vn_phone(raw_phone)?;
    let body = obj(&[
        ("phone", json!(phone)),
        ("avatar_size", json!(240)),
        ("language", json!("vi")),
        ("imei", json!(session.imei)),
        ("reqSrc", json!(40)),
    ]);
    let params = session.encoded_params(&body)?;
    let mut hosts = session.domains.candidates("friend");
    for extra in [
        "https://friend-wpa.chat.zalo.me",
        "https://tt-friend-wpa.chat.zalo.me",
    ] {
        if !hosts.iter().any(|h| h == extra) {
            hosts.push(extra.to_string());
        }
    }
    let mut last_err: Option<anyhow::Error> = None;
    for host in &hosts {
        let url = format!(
            "{}/api/friend/profile/get?{}&params={params}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session.get_json(&url).await {
            Ok(raw) => {
                let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                return parse_phone_profile_response(&resp, &phone, &session.profile.uid);
            }
            Err(e) => {
                if is_expired_err(&e) {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Không tìm thấy người dùng với số điện thoại này")))
}

pub async fn fetch_pinned_messages(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    friend_names: &HashMap<String, String>,
) -> Result<Vec<MessageDto>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let my_uid = session.profile.uid.as_str();
    let send2me = session.profile.send2me_id.as_str();
    let clean = clean_group_id(thread_id);

    if kind == ThreadKind::Group {
        let mut hosts = session.domains.candidates("groupboard");
        for extra in [crate::endpoints::GROUP_BOARD, crate::endpoints::BOARD] {
            if !hosts.iter().any(|h| h == extra) {
                hosts.push(extra.to_string());
            }
        }
        let body = obj(&[
            ("group_id", json!(clean)),
            ("board_type", json!(0)),
            ("page", json!(1)),
            ("count", json!(20)),
            ("last_id", json!(0)),
            ("status", json!(0)),
            ("imei", json!(session.imei)),
        ]);
        if let Ok(params) = session.encoded_params(&body) {
            for host in &hosts {
                let url = format!(
                    "{}/api/board/list?{}&params={params}",
                    host.trim_end_matches('/'),
                    session.common_params()
                );
                if let Ok(raw) = session.get_json(&url).await {
                    let resp =
                        unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                    ensure_not_expired(&resp)?;
                    let parsed = parse_pinned_board_response(
                        &resp,
                        thread_id,
                        my_uid,
                        send2me,
                        true,
                        friend_names,
                    );
                    if !parsed.is_empty() {
                        return Ok(parsed);
                    }
                    if resp.get("error_code").and_then(value_as_i64) == Some(0) {
                        break;
                    }
                }
            }
        }
        if let Ok((info_map, _)) = fetch_group_profiles(session, &[clean.to_string()]).await {
            if let Some(info) = info_map.get(clean).or_else(|| info_map.get(thread_id)) {
                let wrapped = json!({ "data": info });
                let parsed = parse_pinned_board_response(
                    &wrapped,
                    thread_id,
                    my_uid,
                    send2me,
                    true,
                    friend_names,
                );
                if !parsed.is_empty() {
                    return Ok(parsed);
                }
            }
        }
        return Ok(Vec::new());
    }

    let conv_id = if kind == ThreadKind::Cloud || thread_id == send2me {
        send2me
    } else {
        thread_id
    };
    if conv_id.is_empty() {
        return Ok(Vec::new());
    }
    let mut hosts = session.domains.candidates("friendboard");
    if !hosts.iter().any(|h| h == crate::endpoints::FRIEND_BOARD) {
        hosts.push(crate::endpoints::FRIEND_BOARD.to_string());
    }
    let body = obj(&[
        ("conversationId", json!(conv_id)),
        ("version", json!(0)),
        ("imei", json!(session.imei)),
    ]);
    if let Ok(params) = session.encoded_params(&body) {
        for host in &hosts {
            let url = format!(
                "{}/api/friendboard/list?{}&params={params}",
                host.trim_end_matches('/'),
                session.common_params()
            );
            if let Ok(raw) = session.get_json(&url).await {
                let resp =
                    unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let parsed = parse_pinned_board_response(
                    &resp,
                    thread_id,
                    my_uid,
                    send2me,
                    false,
                    friend_names,
                );
                if !parsed.is_empty() || resp.get("error_code").and_then(value_as_i64) == Some(0) {
                    return Ok(parsed);
                }
            }
        }
    }
    Ok(Vec::new())
}

pub fn parse_remote_labels_response(resp: &Value) -> Vec<ChatFolderDto> {
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return Vec::new();
    };
    let mut raw_labels: Vec<Value> = Vec::new();
    if let Some(arr) = data_node.as_array() {
        raw_labels.extend(arr.iter().cloned());
    } else {
        if let Some(ld) = data_node.get("labelData") {
            match ld {
                Value::String(s) => {
                    if let Ok(parsed) = serde_json::from_str::<Value>(s.trim()) {
                        if let Some(arr) = parsed.as_array() {
                            raw_labels.extend(arr.iter().cloned());
                        }
                    }
                }
                Value::Array(arr) => raw_labels.extend(arr.iter().cloned()),
                _ => {}
            }
        }
        if raw_labels.is_empty() {
            for key in ["labels", "items", "list"] {
                if let Some(arr) = data_node.get(key).and_then(Value::as_array) {
                    raw_labels.extend(arr.iter().cloned());
                    break;
                }
            }
        }
    }

    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();
    for (idx, item) in raw_labels.into_iter().enumerate() {
        let Some(raw_id) = pick_str(&item, &["id", "labelId"]).or_else(|| {
            item.get("id")
                .or_else(|| item.get("labelId"))
                .and_then(value_as_i64)
                .map(|n| n.to_string())
        }) else {
            continue;
        };
        let Some(name) = pick_str(&item, &["text", "name", "labelName", "title"]) else {
            continue;
        };
        if !seen_ids.insert(raw_id.clone()) {
            continue;
        }
        let color = pick_str(&item, &["color"]).unwrap_or_default();
        let mut thread_ids = Vec::new();
        let mut seen_tids = HashSet::new();
        for key in ["conversations", "conversationIds", "threadIds", "convs"] {
            if let Some(node) = item.get(key) {
                let parsed_arr: Option<Vec<Value>> = match node {
                    Value::Array(arr) => Some(arr.clone()),
                    Value::String(s) if s.trim().starts_with('[') => {
                        serde_json::from_str::<Vec<Value>>(s.trim()).ok()
                    }
                    _ => None,
                };
                if let Some(arr) = parsed_arr {
                    for entry in arr {
                        let tid_raw = entry
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| value_as_i64(&entry).map(|n| n.to_string()));
                        if let Some(raw_tid) = tid_raw {
                            let clean = clean_group_id(raw_tid.trim()).to_string();
                            if !clean.is_empty() && seen_tids.insert(clean.clone()) {
                                thread_ids.push(clean);
                            }
                        }
                    }
                    break;
                }
            }
        }
        out.push(ChatFolderDto {
            id: format!("label-{raw_id}"),
            name,
            icon: "tag".to_string(),
            color,
            thread_ids,
            excluded_thread_ids: Vec::new(),
            include_kinds: Vec::new(),
            only_unread: false,
            exclude_muted: false,
            position: 100 + idx as i64,
            remote_label_id: Some(raw_id),
        });
    }
    out
}

pub async fn fetch_remote_labels(session: &Session) -> Result<Vec<ChatFolderDto>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let params = session.encoded_params(&body)?;
    let path_and_query = format!("/api/label/get?{}&params={params}", session.common_params());
    let raw = session.get_endpoint("label", &path_and_query).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;
    Ok(parse_remote_labels_response(&resp))
}

pub async fn sync_pin_thread(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    pinned: bool,
) -> Result<()> {
    if !session.is_authenticated() || session.domains.conversation.is_empty() {
        return Ok(());
    }
    let conv_id = if kind == ThreadKind::Group {
        format!("g{}", clean_group_id(thread_id))
    } else {
        thread_id.to_string()
    };
    let body = obj(&[
        ("actionType", json!(if pinned { 1 } else { 2 })),
        ("conversations", json!([conv_id])),
        ("imei", json!(session.imei)),
    ]);
    let params = session.encoded_params(&body)?;
    let url = format!(
        "{}/api/pinconvers/updatev2?{}",
        session.domains.conversation.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    ensure_not_expired(&dec)
}

pub async fn sync_unread_mark(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    unread: bool,
) -> Result<()> {
    if !session.is_authenticated() || session.domains.conversation.is_empty() {
        return Ok(());
    }
    let is_group = kind == ThreadKind::Group;
    let id_val = if is_group {
        clean_group_id(thread_id).to_string()
    } else {
        thread_id.to_string()
    };
    let ts_str = chrono::Utc::now().timestamp_millis().to_string();
    let entry = json!({ "id": id_val, "ts": ts_str });
    let inner = json!({
        "convsGroup": if is_group { json!([entry]) } else { json!([]) },
        "convsUser": if is_group { json!([]) } else { json!([entry]) },
        "imei": session.imei,
    });
    let body = obj(&[("param", json!(inner.to_string()))]);
    let params = session.encoded_params(&body)?;
    let endpoint = if unread {
        "add-unread-mark"
    } else {
        "remove-unread-mark"
    };
    let url = format!(
        "{}/api/convers/{endpoint}?{}",
        session.domains.conversation.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    ensure_not_expired(&dec)
}

pub async fn sync_mute_thread(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    muted: bool,
    duration_secs: Option<i64>,
) -> Result<()> {
    if !session.is_authenticated() || session.domains.profile.is_empty() {
        return Ok(());
    }
    let is_group = kind == ThreadKind::Group;
    let toid = if is_group {
        clean_group_id(thread_id).to_string()
    } else {
        thread_id.to_string()
    };
    let now_sec = chrono::Utc::now().timestamp();
    let duration = if muted {
        duration_secs.filter(|d| *d > 0).unwrap_or(-1)
    } else {
        -1
    };
    let body = obj(&[
        ("toid", json!(toid)),
        ("duration", json!(duration)),
        ("action", json!(if muted { 1 } else { 3 })),
        ("startTime", json!(now_sec)),
        ("muteType", json!(if is_group { 2 } else { 1 })),
        ("imei", json!(session.imei)),
    ]);
    let params = session.encoded_params(&body)?;
    let url = format!(
        "{}/api/social/profile/setmute?{}",
        session.domains.profile.trim_end_matches('/'),
        session.common_params()
    );
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    ensure_not_expired(&dec)
}

fn build_seen_event_item(msg_id: &str, sender: &str, id_val: &str, msg_type: &str) -> Value {
    let normalized_msg_type = {
        let trimmed = msg_type.trim();
        if trimmed.is_empty() {
            "chat.text"
        } else {
            trimmed
        }
    };
    json!({
        "cmi": msg_id.trim(),
        "gmi": msg_id.trim(),
        "uidFrom": sender,
        "idTo": id_val,
        "msgType": normalized_msg_type,
    })
}

pub async fn send_seen_event(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg_id: &str,
    sender_id: &str,
) -> Result<()> {
    send_seen_event_with_type(session, thread_id, kind, msg_id, sender_id, "chat.text").await
}

pub async fn send_seen_event_with_type(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg_id: &str,
    sender_id: &str,
    msg_type: &str,
) -> Result<()> {
    if !session.is_authenticated() || kind == ThreadKind::Cloud || msg_id.trim().is_empty() {
        return Ok(());
    }
    let is_group = kind == ThreadKind::Group;
    let id_val = if is_group {
        clean_group_id(thread_id).to_string()
    } else {
        thread_id.to_string()
    };
    let sender = if sender_id.trim().is_empty() {
        id_val.as_str()
    } else {
        sender_id.trim()
    };
    let item = build_seen_event_item(msg_id, sender, &id_val, msg_type);
    let (base_host, path, msg_infos) = if is_group {
        let group_host = if session.domains.group.is_empty() {
            crate::endpoints::service("group").unwrap_or("https://tt-group-wpa.chat.zalo.me")
        } else {
            session.domains.group.trim_end_matches('/')
        };
        (
            group_host,
            "api/group/seenv2",
            json!({ "data": [item], "grid": id_val }),
        )
    } else {
        let chat_host = if session.domains.chat.is_empty()
            || session.domains.chat.contains("-msg-chat.")
            || session.domains.chat.contains("tt-chat2-wpa")
        {
            crate::endpoints::service("chat").unwrap_or("https://tt-chat3-wpa.chat.zalo.me")
        } else {
            session.domains.chat.trim_end_matches('/')
        };
        (chat_host, "api/message/seenv2", json!({ "data": [item] }))
    };
    let body = obj(&[
        ("msgInfos", json!(msg_infos.to_string())),
        ("imei", json!(session.imei)),
    ]);
    let params = session.encoded_params(&body)?;
    let url = format!("{base_host}/{path}?{}", session.common_params());
    let raw = session.post_form(&url, format!("params={params}")).await?;
    let dec = session.maybe_decrypt(raw, session.session_key_bytes());
    ensure_not_expired(&dec)
}

fn push_unique_host(hosts: &mut Vec<String>, host: &str) {
    let trimmed = host.trim().trim_end_matches('/');
    if !trimmed.is_empty() && !hosts.iter().any(|h| h == trimmed) {
        hosts.push(trimmed.to_string());
    }
}

async fn post_form_across_hosts(
    session: &Session,
    hosts: &[String],
    path: &str,
    body: &Value,
) -> Result<Value> {
    let params = session.encoded_params(body)?;
    let clean_path = path.trim_start_matches('/');
    let mut last_err = anyhow::anyhow!("Không có máy chủ khả dụng cho {clean_path}");
    for host in hosts {
        let url = format!(
            "{}/{}?{}",
            host.trim_end_matches('/'),
            clean_path,
            session.common_params()
        );
        match session.post_form(&url, format!("params={params}")).await {
            Ok(raw) => {
                let dec = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&dec)?;
                let code = dec.get("error_code").and_then(value_as_i64).unwrap_or(0);
                if code == 0 {
                    return Ok(dec);
                }
                let msg = pick_str(&dec, &["error_message", "message"])
                    .unwrap_or_else(|| format!("Mã lỗi {code}"));
                last_err = anyhow::anyhow!("{msg}");
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn collect_id_list(v: Option<&Value>) -> Vec<String> {
    let Some(val) = v else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match val {
        Value::Array(arr) => {
            for item in arr {
                let id = item
                    .as_str()
                    .map(str::trim)
                    .map(str::to_string)
                    .or_else(|| item.as_i64().map(|n| n.to_string()))
                    .or_else(|| pick_str(item, &["uid", "userId", "id"]));
                if let Some(s) = id.filter(|x| !x.is_empty()) {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
        Value::Object(map) => {
            for (k, _) in map {
                let s = k.trim().to_string();
                if !s.is_empty() && !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        _ => {}
    }
    out
}

pub fn parse_add_group_members_response(
    resp: &Value,
    requested_ids: &[String],
) -> AddGroupMembersResultDto {
    let data = resp.get("data").unwrap_or(resp);
    let mut failed = collect_id_list(data.get("errorMembers"));
    for extra in collect_id_list(data.get("error_data")) {
        if !failed.contains(&extra) {
            failed.push(extra);
        }
    }
    let pending = collect_id_list(
        data.get("pendingMembers")
            .or_else(|| data.get("pending_members")),
    );
    let added: Vec<String> = requested_ids
        .iter()
        .filter(|id| !failed.contains(id) && !pending.contains(id))
        .cloned()
        .collect();
    let message = pick_str(resp, &["error_message", "message"]).unwrap_or_default();
    AddGroupMembersResultDto {
        added,
        pending,
        failed,
        message,
    }
}

pub async fn add_group_members(
    session: &Session,
    group_id: &str,
    member_ids: &[String],
) -> Result<AddGroupMembersResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let clean_gid = clean_group_id(group_id);
    if clean_gid.is_empty() {
        anyhow::bail!("Mã nhóm không hợp lệ");
    }
    let clean_members: Vec<String> = member_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if clean_members.is_empty() {
        anyhow::bail!("Vui lòng chọn ít nhất 1 thành viên");
    }
    let member_types = vec![-1_i64; clean_members.len()];
    let body = obj(&[
        ("grid", json!(clean_gid)),
        ("members", json!(clean_members)),
        ("membersTypes", json!(member_types)),
        ("imei", json!(session.imei)),
        ("clientLang", json!("vi")),
    ]);
    let mut hosts = session.domains.candidates("group");
    if let Some(svc) = crate::endpoints::service("group") {
        push_unique_host(&mut hosts, svc);
    }
    let resp = post_form_across_hosts(session, &hosts, "api/group/add", &body).await?;
    Ok(parse_add_group_members_response(&resp, &clean_members))
}

fn protocol_service_hosts(session: &Session, svc: &str) -> Vec<String> {
    let profile_host = session.domains.profile.trim().trim_end_matches('/');
    let mut hosts = Vec::new();
    for raw in session.domains.candidates(svc) {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        if svc == "friend" {
            let lower = trimmed.to_ascii_lowercase();
            if (!profile_host.is_empty() && trimmed.eq_ignore_ascii_case(profile_host))
                || lower.contains("profile-wpa")
            {
                continue;
            }
        }
        push_unique_host(&mut hosts, trimmed);
    }
    if let Some(default_host) = crate::endpoints::service(svc) {
        push_unique_host(&mut hosts, default_host);
    }
    hosts
}

async fn post_form_protocol_hosts(
    session: &Session,
    svc: &str,
    path: &str,
    body: &Value,
) -> Result<Value> {
    let hosts = protocol_service_hosts(session, svc);
    let params = session.encoded_params(body)?;
    let clean_path = path.trim_start_matches('/');
    let mut last_err = anyhow::anyhow!("Không có máy chủ khả dụng cho {clean_path}");
    for host in &hosts {
        let url = format!(
            "{}/{}?{}",
            host.trim_end_matches('/'),
            clean_path,
            session.common_params()
        );
        match session.post_form(&url, format!("params={params}")).await {
            Ok(raw) => {
                let dec = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&dec)?;
                if dec.is_object() {
                    return Ok(dec);
                }
                last_err = anyhow::anyhow!("Phản hồi không hợp lệ từ máy chủ cho {clean_path}");
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn get_protocol_hosts(
    session: &Session,
    svc: &str,
    path: &str,
    body: &Value,
) -> Result<Value> {
    let hosts = protocol_service_hosts(session, svc);
    let params = session.encoded_params(body)?;
    let clean_path = path.trim_start_matches('/');
    let mut last_err = anyhow::anyhow!("Không có máy chủ khả dụng cho {clean_path}");
    for host in &hosts {
        let url = format!(
            "{}/{}?{}&params={params}",
            host.trim_end_matches('/'),
            clean_path,
            session.common_params()
        );
        match session.get_json(&url).await {
            Ok(raw) => {
                let dec = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&dec)?;
                if dec.is_object() {
                    return Ok(dec);
                }
                last_err = anyhow::anyhow!("Phản hồi không hợp lệ từ máy chủ cho {clean_path}");
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn unwrap_json_field(val: &Value) -> Option<Value> {
    match val {
        Value::Object(_) | Value::Array(_) => Some(val.clone()),
        Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
            {
                serde_json::from_str::<Value>(trimmed).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn parse_friend_action_response(
    resp: &Value,
    user_id: &str,
    action: &str,
) -> Result<FriendActionResultDto> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    if code != 0 {
        let msg = pick_str(resp, &["error_message", "message"])
            .unwrap_or_else(|| format!("Thao tác thất bại (mã {code})"));
        return Err(anyhow::anyhow!("{msg}"));
    }
    Ok(FriendActionResultDto {
        ok: true,
        user_id: user_id.to_string(),
        action: action.to_string(),
        already_friends: false,
        auto_accepted: false,
        message: pick_str(resp, &["error_message", "message"]).unwrap_or_default(),
    })
}

pub fn parse_send_friend_request_response(
    resp: &Value,
    user_id: &str,
) -> Result<FriendActionResultDto> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    match code {
        0 => Ok(FriendActionResultDto {
            ok: true,
            user_id: user_id.to_string(),
            action: "send_request".to_string(),
            already_friends: false,
            auto_accepted: false,
            message: String::new(),
        }),
        225 => Ok(FriendActionResultDto {
            ok: true,
            user_id: user_id.to_string(),
            action: "send_request".to_string(),
            already_friends: true,
            auto_accepted: false,
            message: "Hai người đã là bạn bè.".to_string(),
        }),
        222 => Ok(FriendActionResultDto {
            ok: true,
            user_id: user_id.to_string(),
            action: "send_request".to_string(),
            already_friends: false,
            auto_accepted: true,
            message: "Người này đã gửi lời mời trước đó — đã tự động trở thành bạn bè.".to_string(),
        }),
        215 => Err(anyhow::anyhow!(
            "Không thể gửi lời mời kết bạn do cài đặt quyền riêng tư hoặc tài khoản đã chặn (mã 215)."
        )),
        _ => {
            let msg = pick_str(resp, &["error_message", "message"])
                .unwrap_or_else(|| format!("Không thể gửi lời mời kết bạn (mã {code})"));
            Err(anyhow::anyhow!("{msg}"))
        }
    }
}

fn parse_sent_friend_request_entry(
    fallback_uid: Option<&str>,
    val: &Value,
) -> Option<FriendRequestItemDto> {
    if !val.is_object() {
        return None;
    }
    let raw_uid = pick_str(val, &["userId", "uid", "id"])
        .or_else(|| fallback_uid.map(str::to_string))?;
    let user_id = clean_uid(&raw_uid);
    if user_id.is_empty() || user_id == "0" {
        return None;
    }
    let display_name = pick_str(val, &["displayName", "zaloName", "name", "display_name"])
        .unwrap_or_else(|| user_id.clone());
    let account_name = pick_str(val, &["zaloName"]).unwrap_or_default();
    let avatar = pick_str(val, &["avatar", "avt", "profile_avatar"])
        .map(|u| normalize_avatar_url(&u))
        .unwrap_or_default();
    let freq_info = val.get("fReqInfo").and_then(unwrap_json_field);
    let message = freq_info
        .as_ref()
        .and_then(|f| pick_str(f, &["message", "msg", "customText"]))
        .or_else(|| pick_str(val, &["message", "msg"]))
        .unwrap_or_default();
    let created_ts = freq_info
        .as_ref()
        .and_then(|f| {
            f.get("time")
                .or_else(|| f.get("createdTime"))
                .or_else(|| f.get("ts"))
                .and_then(value_as_i64)
        })
        .or_else(|| {
            val.get("time")
                .or_else(|| val.get("createdTime"))
                .or_else(|| val.get("ts"))
                .and_then(value_as_i64)
        })
        .map(normalize_ts_ms)
        .unwrap_or(0);
    let is_seen = val
        .get("isSeenFriendReq")
        .or_else(|| freq_info.as_ref().and_then(|f| f.get("isSeen")))
        .and_then(value_as_bool)
        .unwrap_or(false);

    Some(FriendRequestItemDto {
        user_id,
        display_name,
        account_name,
        avatar,
        message,
        created_ts,
        direction: "sent".to_string(),
        is_seen,
    })
}

pub fn parse_sent_friend_requests_response(resp: &Value) -> Result<Vec<FriendRequestItemDto>> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    if code == 112 {
        return Ok(Vec::new());
    }
    if code != 0 {
        let msg = pick_str(resp, &["error_message", "message"])
            .unwrap_or_else(|| format!("Không tải được danh sách lời mời đã gửi (mã {code})"));
        return Err(anyhow::anyhow!("{msg}"));
    }

    let Some(data_node) = resp.get("data").and_then(unwrap_json_field) else {
        return Ok(Vec::new());
    };
    let effective = match &data_node {
        Value::Object(m) if m.len() == 1 && m.get("data").is_some_and(Value::is_object) => {
            m.get("data").unwrap_or(&data_node)
        }
        _ => &data_node,
    };

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    match effective {
        Value::Object(map) => {
            for (k, v) in map {
                if let Some(item) = parse_sent_friend_request_entry(Some(k), v) {
                    if seen.insert(item.user_id.clone()) {
                        out.push(item);
                    }
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(item) = parse_sent_friend_request_entry(None, v) {
                    if seen.insert(item.user_id.clone()) {
                        out.push(item);
                    }
                }
            }
        }
        _ => {}
    }

    out.sort_by(|a, b| {
        b.created_ts
            .cmp(&a.created_ts)
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    Ok(out)
}

pub fn parse_received_friend_requests_response(resp: &Value) -> Result<Vec<FriendRequestItemDto>> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    if code == 112 {
        return Ok(Vec::new());
    }
    if code != 0 {
        let msg = pick_str(resp, &["error_message", "message"])
            .unwrap_or_else(|| format!("Không tải được danh sách lời mời kết bạn (mã {code})"));
        return Err(anyhow::anyhow!("{msg}"));
    }

    let Some(data_node) = resp.get("data").and_then(unwrap_json_field) else {
        return Ok(Vec::new());
    };
    let inner = data_node
        .get("data")
        .and_then(unwrap_json_field)
        .unwrap_or(data_node);
    let Some(items) = inner.get("recommItems").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in items {
        let info = entry
            .get("dataInfo")
            .filter(|v| v.is_object())
            .unwrap_or(entry);
        // Strictly filter to recommType == 2 (ReceivedFriendRequest); skip recommType == 1 (suggestions).
        if info.get("recommType").and_then(value_as_i64) != Some(2) {
            continue;
        }
        let Some(raw_uid) = pick_str(info, &["userId", "uid", "id"]) else {
            continue;
        };
        let user_id = clean_uid(&raw_uid);
        if user_id.is_empty() || user_id == "0" || !seen.insert(user_id.clone()) {
            continue;
        }
        let display_name = pick_str(info, &["displayName", "zaloName", "name", "display_name"])
            .unwrap_or_else(|| user_id.clone());
        let account_name = pick_str(info, &["zaloName"]).unwrap_or_default();
        let avatar = pick_str(info, &["avatar", "avt", "profile_avatar"])
            .map(|u| normalize_avatar_url(&u))
            .unwrap_or_default();
        let recomm_info = info.get("recommInfo").and_then(unwrap_json_field);
        let message = recomm_info
            .as_ref()
            .and_then(|r| pick_str(r, &["message", "customText", "msg"]))
            .or_else(|| pick_str(info, &["message", "msg"]))
            .unwrap_or_default();
        let created_ts = info
            .get("recommTime")
            .or_else(|| {
                recomm_info
                    .as_ref()
                    .and_then(|r| r.get("time").or_else(|| r.get("createdTime")))
            })
            .and_then(value_as_i64)
            .map(normalize_ts_ms)
            .unwrap_or(0);
        let is_seen = info
            .get("isSeenFriendReq")
            .or_else(|| entry.get("isSeenFriendReq"))
            .and_then(value_as_bool)
            .unwrap_or(false);

        out.push(FriendRequestItemDto {
            user_id,
            display_name,
            account_name,
            avatar,
            message,
            created_ts,
            direction: "received".to_string(),
            is_seen,
        });
    }

    out.sort_by(|a, b| {
        b.created_ts
            .cmp(&a.created_ts)
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    Ok(out)
}

pub fn parse_leave_group_response(resp: &Value, clean_gid: &str) -> Result<LeaveGroupResultDto> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    if code == 166 {
        return Err(anyhow::anyhow!(
            "Không thể rời nhóm này. Bạn có thể không còn trong nhóm, hoặc trưởng nhóm cần chuyển quyền trưởng nhóm hoặc giải tán nhóm trước khi rời (mã 166)."
        ));
    }
    if code != 0 {
        let msg = pick_str(resp, &["error_message", "message"])
            .unwrap_or_else(|| format!("Không thể rời nhóm (mã {code})"));
        return Err(anyhow::anyhow!("{msg}"));
    }
    let data_owned = resp.get("data").and_then(unwrap_json_field);
    let data = data_owned.as_ref().unwrap_or(resp);
    let member_error = collect_id_list(
        data.get("memberError")
            .or_else(|| data.get("member_error")),
    );
    let left = member_error.is_empty();
    let message = pick_str(resp, &["error_message", "message"]).unwrap_or_else(|| {
        if left {
            String::new()
        } else {
            "Không thể rời nhóm này.".to_string()
        }
    });
    Ok(LeaveGroupResultDto {
        group_id: clean_gid.to_string(),
        left,
        member_error,
        message,
    })
}

pub fn parse_kick_group_members_response(
    resp: &Value,
    clean_gid: &str,
    requested_ids: &[String],
) -> Result<KickGroupMembersResultDto> {
    ensure_not_expired(resp)?;
    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
    if code == 165 {
        return Err(anyhow::anyhow!("Thành viên không còn trong nhóm (mã 165)."));
    }
    if code == 166 {
        return Err(anyhow::anyhow!(
            "Bạn không có quyền xóa thành viên này hoặc không còn trong nhóm (mã 166)."
        ));
    }
    if code != 0 {
        let msg = pick_str(resp, &["error_message", "message"])
            .unwrap_or_else(|| format!("Không thể xóa thành viên khỏi nhóm (mã {code})"));
        return Err(anyhow::anyhow!("{msg}"));
    }
    let data_owned = resp.get("data").and_then(unwrap_json_field);
    let data = data_owned.as_ref().unwrap_or(resp);
    let mut failed = Vec::new();
    for raw in collect_id_list(
        data.get("errorMembers")
            .or_else(|| data.get("error_members")),
    ) {
        let uid = clean_uid(&raw);
        if !uid.is_empty() && !failed.contains(&uid) {
            failed.push(uid);
        }
    }
    let removed: Vec<String> = requested_ids
        .iter()
        .filter(|id| !failed.contains(id))
        .cloned()
        .collect();
    let message = pick_str(resp, &["error_message", "message"]).unwrap_or_default();
    Ok(KickGroupMembersResultDto {
        group_id: clean_gid.to_string(),
        removed,
        failed,
        message,
    })
}

fn build_send_friend_request_body(uid: &str, message: Option<&str>, imei: &str) -> Value {
    let msg = message
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Xin chào, kết bạn với mình nhé!");
    let src_params = json!({ "uidTo": uid }).to_string();
    obj(&[
        ("toid", json!(uid)),
        ("msg", json!(msg)),
        ("reqsrc", json!(30)),
        ("imei", json!(imei)),
        ("language", json!("vi")),
        ("srcParams", json!(src_params)),
    ])
}

fn build_leave_group_body(clean_gid: &str, imei: &str, silent: bool) -> Value {
    obj(&[
        ("grids", json!([clean_gid])),
        ("imei", json!(imei)),
        ("silent", json!(if silent { 1 } else { 0 })),
        ("language", json!("vi")),
    ])
}

fn build_kick_group_members_body(clean_gid: &str, clean_members: &[String], imei: &str) -> Value {
    obj(&[
        ("grid", json!(clean_gid)),
        ("members", json!(clean_members)),
        ("imei", json!(imei)),
    ])
}

pub async fn remove_friend(session: &Session, user_id: &str) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid)), ("imei", json!(session.imei))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/remove", &body).await?;
    parse_friend_action_response(&resp, &uid, "remove")
}

pub async fn send_friend_request(
    session: &Session,
    user_id: &str,
    message: Option<&str>,
) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = build_send_friend_request_body(&uid, message, &session.imei);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/sendreq", &body).await?;
    parse_send_friend_request_response(&resp, &uid)
}

pub async fn accept_friend_request(
    session: &Session,
    user_id: &str,
) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid)), ("language", json!("vi"))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/accept", &body).await?;
    parse_friend_action_response(&resp, &uid, "accept")
}

pub async fn reject_friend_request(
    session: &Session,
    user_id: &str,
) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/reject", &body).await?;
    parse_friend_action_response(&resp, &uid, "reject")
}

pub async fn undo_friend_request(
    session: &Session,
    user_id: &str,
) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/undo", &body).await?;
    parse_friend_action_response(&resp, &uid, "undo")
}

pub async fn list_sent_friend_requests(session: &Session) -> Result<Vec<FriendRequestItemDto>> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let resp = get_protocol_hosts(session, "friend", "api/friend/requested/list", &body).await?;
    parse_sent_friend_requests_response(&resp)
}

pub async fn list_received_friend_requests(session: &Session) -> Result<Vec<FriendRequestItemDto>> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let resp = get_protocol_hosts(session, "friend", "api/friend/recommendsv2/list", &body).await?;
    parse_received_friend_requests_response(&resp)
}

pub async fn list_friend_requests(session: &Session) -> Result<FriendRequestsListDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let (recv_res, sent_res) = tokio::join!(
        list_received_friend_requests(session),
        list_sent_friend_requests(session),
    );
    match (recv_res, sent_res) {
        (Err(e), _) if is_expired_err(&e) => Err(e),
        (_, Err(e)) if is_expired_err(&e) => Err(e),
        (Ok(received), Ok(sent)) => Ok(FriendRequestsListDto { received, sent }),
        (Ok(received), Err(e)) => {
            eprintln!("[litechat][friends] list_sent_friend_requests error: {e}");
            Ok(FriendRequestsListDto {
                received,
                sent: Vec::new(),
            })
        }
        (Err(e), Ok(sent)) => {
            eprintln!("[litechat][friends] list_received_friend_requests error: {e}");
            Ok(FriendRequestsListDto {
                received: Vec::new(),
                sent,
            })
        }
        (Err(e), Err(_)) => Err(e),
    }
}

pub async fn block_user(session: &Session, user_id: &str) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid)), ("imei", json!(session.imei))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/block", &body).await?;
    parse_friend_action_response(&resp, &uid, "block")
}

pub async fn unblock_user(session: &Session, user_id: &str) -> Result<FriendActionResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let uid = clean_uid(user_id);
    if uid.is_empty() {
        anyhow::bail!("Mã người dùng không hợp lệ");
    }
    let body = obj(&[("fid", json!(uid)), ("imei", json!(session.imei))]);
    let resp = post_form_protocol_hosts(session, "friend", "api/friend/unblock", &body).await?;
    parse_friend_action_response(&resp, &uid, "unblock")
}

pub async fn leave_group(
    session: &Session,
    group_id: &str,
    silent: bool,
) -> Result<LeaveGroupResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let clean_gid = clean_group_id(group_id);
    if clean_gid.is_empty() {
        anyhow::bail!("Mã nhóm không hợp lệ");
    }
    let body = build_leave_group_body(clean_gid, &session.imei, silent);
    let resp = post_form_protocol_hosts(session, "group", "api/group/leave", &body).await?;
    parse_leave_group_response(&resp, clean_gid)
}

pub async fn kick_group_members(
    session: &Session,
    group_id: &str,
    member_ids: &[String],
) -> Result<KickGroupMembersResultDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let clean_gid = clean_group_id(group_id);
    if clean_gid.is_empty() {
        anyhow::bail!("Mã nhóm không hợp lệ");
    }
    let mut clean_members = Vec::new();
    for raw in member_ids {
        let uid = clean_uid(raw);
        if !uid.is_empty() && !clean_members.contains(&uid) {
            clean_members.push(uid);
        }
    }
    if clean_members.is_empty() {
        anyhow::bail!("Vui lòng chọn ít nhất 1 thành viên");
    }
    let body = build_kick_group_members_body(clean_gid, &clean_members, &session.imei);
    let resp = post_form_protocol_hosts(session, "group", "api/group/kickout", &body).await?;
    parse_kick_group_members_response(&resp, clean_gid, &clean_members)
}

pub async fn create_reminder(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    title: &str,
    start_time_ms: i64,
    repeat: i32,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let clean_title = title.trim();
    if clean_title.is_empty() {
        anyhow::bail!("Nội dung nhắc hẹn không được để trống");
    }
    let normalized_start = normalize_ts_ms(start_time_ms);
    let inner_params = json!({ "title": clean_title }).to_string();
    let resp = if kind == ThreadKind::Group {
        let clean_gid = clean_group_id(thread_id);
        let body = obj(&[
            ("grid", json!(clean_gid)),
            ("type", json!(0)),
            ("color", json!(-16_777_216_i64)),
            ("emoji", json!("⏰")),
            ("startTime", json!(normalized_start)),
            ("duration", json!(-1)),
            ("params", json!(inner_params)),
            ("repeat", json!(repeat)),
            ("src", json!(1)),
            ("imei", json!(session.imei)),
        ]);
        let mut hosts = session.domains.candidates("groupboard");
        push_unique_host(&mut hosts, crate::endpoints::GROUP_BOARD);
        push_unique_host(&mut hosts, crate::endpoints::BOARD);
        post_form_across_hosts(session, &hosts, "api/board/topic/createv2", &body).await?
    } else {
        let to_uid = thread_id.trim();
        let object_data = json!({
            "toUid": to_uid,
            "type": 0,
            "color": -16_777_216_i64,
            "emoji": "⏰",
            "startTime": normalized_start,
            "duration": -1,
            "params": { "title": clean_title },
            "needPin": false,
            "reminderType": 0,
            "repeat": repeat,
        })
        .to_string();
        let body = obj(&[("objectData", json!(object_data)), ("imei", json!(session.imei))]);
        let mut hosts = session.domains.candidates("reminder");
        push_unique_host(&mut hosts, crate::endpoints::REMINDER);
        push_unique_host(&mut hosts, crate::endpoints::FRIEND_BOARD);
        post_form_across_hosts(session, &hosts, "api/reminder/create", &body).await?
    };

    let data = resp.get("data").unwrap_or(&resp);
    let reminder_id =
        pick_str(data, &["id", "reminderId", "topicId"]).unwrap_or_default();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let msg_id = if !reminder_id.is_empty() {
        format!("rem-{reminder_id}")
    } else {
        format!("rem-{now_ms}")
    };
    let meta = json!({
        "title": clean_title,
        "startTime": normalized_start,
        "repeat": repeat,
        "reminderId": reminder_id,
    })
    .to_string();
    Ok(MessageDto {
        msg_id,
        cli_msg_id: None,
        thread_id: thread_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".to_string(),
        text: format!("Nhắc hẹn: {clean_title}"),
        msg_type: "chat.reminder".to_string(),
        ts: now_ms,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: None,
        attachment_name: Some(format!("teza-reminder:{meta}")),
    })
}

pub fn parse_poll_dto(resp: &Value, fallback_poll_id: i64) -> Option<PollDto> {
    let data = resp.get("data").unwrap_or(resp);
    parse_poll_value(data, fallback_poll_id)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_poll(
    session: &Session,
    group_id: &str,
    question: &str,
    options: &[String],
    expired_time_ms: i64,
    allow_multi: bool,
    allow_add_option: bool,
    is_anonymous: bool,
) -> Result<MessageDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let clean_gid = clean_group_id(group_id);
    let clean_q = question.trim();
    if clean_q.is_empty() {
        anyhow::bail!("Câu hỏi bình chọn không được để trống");
    }
    let clean_opts: Vec<String> = options
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if clean_opts.len() < 2 {
        anyhow::bail!("Cần ít nhất 2 lựa chọn để tạo bình chọn");
    }

    let body = obj(&[
        ("group_id", json!(clean_gid)),
        ("question", json!(clean_q)),
        ("options", json!(clean_opts)),
        ("expired_time", json!(expired_time_ms.max(0))),
        ("pinAct", json!(false)),
        ("allow_multi_choices", json!(allow_multi)),
        ("allow_add_new_option", json!(allow_add_option)),
        ("is_hide_vote_preview", json!(false)),
        ("is_anonymous", json!(is_anonymous)),
        ("poll_type", json!(0)),
        ("src", json!(1)),
        ("imei", json!(session.imei)),
    ]);
    let mut hosts = session.domains.candidates("groupboard");
    push_unique_host(&mut hosts, crate::endpoints::GROUP_BOARD);
    push_unique_host(&mut hosts, crate::endpoints::BOARD);
    for h in session.domains.candidates("group") {
        push_unique_host(&mut hosts, &h);
    }

    let resp = post_form_across_hosts(session, &hosts, "api/poll/create", &body).await?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let fallback_id = now_ms;
    let poll = parse_poll_dto(&resp, fallback_id).unwrap_or_else(|| PollDto {
        poll_id: fallback_id,
        question: clean_q.to_string(),
        options: clean_opts
            .iter()
            .enumerate()
            .map(|(i, c)| PollOptionDto {
                id: (i + 1) as i64,
                content: c.clone(),
                votes: 0,
                voted: false,
            })
            .collect(),
        allow_multi,
        allow_add_option,
        is_anonymous,
        expired_time: expired_time_ms.max(0),
        closed: false,
    });
    let poll_json = serde_json::to_string(&poll)?;
    Ok(MessageDto {
        msg_id: format!("poll-{}", poll.poll_id),
        cli_msg_id: None,
        thread_id: group_id.to_string(),
        sender_id: session.profile.uid.clone(),
        sender_name: "Bạn".to_string(),
        text: format!("Bình chọn: {}", poll.question),
        msg_type: "chat.poll".to_string(),
        ts: now_ms,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: None,
        attachment_name: Some(format!("teza-poll:{poll_json}")),
    })
}

pub async fn get_poll_detail(session: &Session, poll_id: i64) -> Result<PollDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[("poll_id", json!(poll_id)), ("imei", json!(session.imei))]);
    let mut hosts = session.domains.candidates("groupboard");
    push_unique_host(&mut hosts, crate::endpoints::GROUP_BOARD);
    push_unique_host(&mut hosts, crate::endpoints::BOARD);
    for h in session.domains.candidates("group") {
        push_unique_host(&mut hosts, &h);
    }
    let resp = post_form_across_hosts(session, &hosts, "api/poll/detail", &body).await?;
    parse_poll_dto(&resp, poll_id).ok_or_else(|| anyhow::anyhow!("Không đọc được dữ liệu bình chọn"))
}

pub async fn vote_poll(
    session: &Session,
    poll_id: i64,
    option_ids: &[i64],
) -> Result<PollDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[
        ("poll_id", json!(poll_id)),
        ("option_id", json!(option_ids)),
        ("imei", json!(session.imei)),
    ]);
    let mut hosts = session.domains.candidates("groupboard");
    push_unique_host(&mut hosts, crate::endpoints::GROUP_BOARD);
    push_unique_host(&mut hosts, crate::endpoints::BOARD);
    for h in session.domains.candidates("group") {
        push_unique_host(&mut hosts, &h);
    }
    let resp = post_form_across_hosts(session, &hosts, "api/poll/vote", &body).await?;
    if let Some(dto) = parse_poll_dto(&resp, poll_id).filter(|p| !p.options.is_empty()) {
        return Ok(dto);
    }
    get_poll_detail(session, poll_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn apply_last_msgs_resolves_incoming_partner_normalizes_ts_and_sorts_newest_first() {
        let mut threads = vec![
            ThreadDto {
                id: "cloud-1".into(),
                title: "Cloud".into(),
                avatar: String::new(),
                kind: ThreadKind::Cloud,
                last_preview: String::new(),
                last_ts: 1_700_000_010_000,
                unread_count: 0,
                is_pinned: true,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "friend-a".into(),
                title: "Friend A".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "old".into(),
                last_ts: 1_700_000_000_000,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "888111".into(),
                title: "Group 1".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: String::new(),
                last_ts: 1_700_000_020_000,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
        ];

        let last_msgs = json!({
            "data": {
                "msgs": [
                    {
                        "uidFrom": "friend-a",
                        "idTo": "me-999",
                        "content": "Tin nhan moi nhat tu Friend A",
                        "ts": "1700000050"
                    },
                    {
                        "uidFrom": "stranger-b",
                        "idTo": "me-999",
                        "dName": "Stranger B",
                        "content": "Chao ban",
                        "ts": 1_700_000_030_000_i64
                    }
                ],
                "groupMsgs": [
                    {
                        "grid": "g888111",
                        "uidFrom": "member-1",
                        "dName": "An",
                        "content": "Tin nhom cu hon",
                        "ts": 1_700_000_025_000_i64
                    }
                ]
            }
        });

        apply_last_msgs_to_threads(&mut threads, &last_msgs, "me-999", "cloud-1");

        // Pinned cloud-1 stays above read unpinned threads; unpinned sort newest first
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["cloud-1", "friend-a", "stranger-b", "888111"]);
        assert_eq!(threads[1].last_ts, 1_700_000_050_000);
        assert_eq!(threads[1].last_preview, "Tin nhan moi nhat tu Friend A");
        assert_eq!(threads[3].last_preview, "An: Tin nhom cu hon");
        assert!(!threads.iter().any(|t| t.id == "me-999"));

        // Unread thread keeps timestamp position (does not jump above pinned or newer threads)
        threads[3].unread_count = 2;
        sort_threads_by_recent(&mut threads);
        let ids_after_unread: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        // Zalo sort order: pinned threads first, then newest timestamp regardless of unread badge
        assert_eq!(ids_after_unread, vec!["cloud-1", "friend-a", "stranger-b", "888111"]);
    }

    #[test]
    fn outgoing_never_marks_unread_and_group_sender_uid_resolves_name() {
        let mut threads = vec![
            ThreadDto {
                id: "111222333".into(),
                title: "Minh Anh".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: String::new(),
                last_ts: 0,
                unread_count: 2,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "999888777".into(),
                title: "Nhom Du An".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: String::new(),
                last_ts: 0,
                unread_count: 0,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
        ];

        let last_msgs = json!({
            "data": {
                "msgs": [
                    {
                        "uidFrom": "555666777",
                        "idTo": "111222333",
                        "content": "Tin toi vua gui",
                        "unread": 3,
                        "ts": 1_700_000_090_000_i64
                    }
                ],
                "groupMsgs": [
                    {
                        "grid": "999888777",
                        "uidFrom": "111222333",
                        "content": "Da xong phan API",
                        "ts": 1_700_000_080_000_i64
                    }
                ]
            }
        });

        apply_last_msgs_to_threads(&mut threads, &last_msgs, "555666777", "");

        let direct = threads.iter().find(|t| t.id == "111222333").unwrap();
        assert_eq!(direct.unread_count, 0);
        assert_eq!(direct.last_preview, "Bạn: Tin toi vua gui");

        let grp = threads.iter().find(|t| t.id == "999888777").unwrap();
        assert_eq!(grp.last_preview, "Minh Anh: Da xong phan API");
    }

    #[test]
    fn extract_history_helpers_support_group_msgs_and_filtered_flag() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "groupMsgs": [
                    { "msgId": "g1", "uidFrom": "u1", "idTo": "grp1", "content": "Hello", "ts": 1_700_000_001_000_i64 }
                ],
                "isFiltered": 1,
                "hasMore": false
            }
        });
        let items = extract_history_items(&resp).unwrap();
        assert_eq!(items.len(), 1);
        assert!(extract_history_has_more(&resp, items.len()));

        let end_resp = json!({
            "error_code": -69,
            "error_message": "End of history",
            "data": { "msgs": [] }
        });
        assert!(!extract_history_has_more(&end_resp, 0));
    }

    #[test]
    fn normalize_vn_phone_and_parse_profile_work() {
        assert_eq!(normalize_vn_phone("0912 345 678").unwrap(), "84912345678");
        assert_eq!(normalize_vn_phone("+84-912-345-678").unwrap(), "84912345678");
        assert_eq!(normalize_vn_phone("912345678").unwrap(), "84912345678");
        assert!(normalize_vn_phone("abc123").is_err());
        assert!(normalize_vn_phone("0123").is_err());

        let resp = json!({
            "error_code": 0,
            "data": {
                "userId": "778899",
                "displayName": "Le Thi Hoa",
                "zaloName": "Hoa Le",
                "avatar": "//ava.zdn.vn/hoa.jpg",
                "sdob": "01/05/1995",
                "gender": 1,
                "isFr": 0
            }
        });
    let prof = parse_phone_profile_response(&resp, "84912345678", "999").unwrap();
    assert_eq!(prof.uid, "778899");
    assert_eq!(prof.display_name, "Le Thi Hoa");
    assert_eq!(prof.phone_number, "84912345678");
    assert_eq!(prof.sdob, "01/05/1995");
    assert_eq!(prof.gender, Some(1));
    assert_eq!(prof.is_friend, Some(false));
    assert!(!prof.is_self);

    let not_found = json!({ "error_code": 216, "error_message": "Not found" });
    assert!(parse_phone_profile_response(&not_found, "84912345678", "999").is_err());
    }

    #[test]
    fn parse_pinned_board_response_extracts_embedded_params_and_messages() {
        let mut names = HashMap::new();
        names.insert("1001".to_string(), "Minh".to_string());
        let resp = json!({
            "error_code": 0,
            "data": {
                "items": [
                    {
                        "id": "board-1",
                        "creatorId": "1001",
                        "createTime": 1_700_000_050_000_i64,
                        "params": "{\"clientMsgId\":\"m-pin-1\",\"msgId\":\"m-pin-1\",\"senderUid\":\"1001\",\"title\":\"Hop luc 14h\"}"
                    },
                    {
                        "msgId": "m-pin-2",
                        "uidFrom": "1002",
                        "dName": "Lan",
                        "content": "Ban doc moi",
                        "msgType": "chat.text",
                        "ts": 1_700_000_060_000_i64
                    }
                ]
            }
        });
        let pins = parse_pinned_board_response(&resp, "grp-9", "9999", "", true, &names);
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].msg_id, "m-pin-1");
        assert_eq!(pins[0].sender_name, "Minh");
        assert_eq!(pins[0].text, "Hop luc 14h");
        assert!(pins[0].is_pinned);
        assert_eq!(pins[1].msg_id, "m-pin-2");
        assert_eq!(pins[1].sender_name, "Lan");
    }

    #[test]
    fn parse_remote_labels_response_parses_stringified_label_data() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "labelData": "[{\"id\":1,\"text\":\"Khach hang\",\"color\":\"#CFA13A\",\"conversations\":[\"12345\",\"g998877\",\"12345\"]},{\"labelId\":\"work\",\"labelName\":\"Cong viec\",\"conversations\":\"[\\\"555\\\"]\"}]"
            }
        });
        let folders = parse_remote_labels_response(&resp);
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].id, "label-1");
        assert_eq!(folders[0].name, "Khach hang");
        assert_eq!(folders[0].thread_ids, vec!["12345", "998877"]);
        assert_eq!(folders[0].remote_label_id.as_deref(), Some("1"));
        assert_eq!(folders[1].id, "label-work");
        assert_eq!(folders[1].thread_ids, vec!["555"]);
    }

    #[test]
    fn group_info_has_member_checks_creator_admins_and_mem_ver_list() {
        let info = json!({
            "creatorId": "1001",
            "adminIds": ["2002"],
            "memVerList": ["3003_1", "4004_0"]
        });
        assert!(group_info_has_member(&info, "1001"));
        assert!(group_info_has_member(&info, "2002"));
        assert!(group_info_has_member(&info, "3003"));
        assert!(group_info_has_member(&info, "4004_9"));
        assert!(!group_info_has_member(&info, "9999"));
        assert!(!group_info_has_member(&info, ""));
    }

    #[test]
    fn parse_add_group_members_handles_error_and_pending() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "errorMembers": ["u2"],
                "pendingMembers": [{ "uid": "u3" }]
            }
        });
        let req = vec!["u1".to_string(), "u2".to_string(), "u3".to_string()];
        let out = parse_add_group_members_response(&resp, &req);
        assert_eq!(out.added, vec!["u1"]);
        assert_eq!(out.failed, vec!["u2"]);
        assert_eq!(out.pending, vec!["u3"]);
    }

    #[test]
    fn parse_poll_dto_extracts_options_and_flags() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "poll_id": 5511_i64,
                "question": "Cuối tuần đi đâu?",
                "allow_multi_choices": true,
                "allow_add_new_option": false,
                "is_anonymous": true,
                "expired_time": 1700010000_i64,
                "closed": false,
                "options": [
                    { "option_id": 10, "content": "Đà Lạt", "total_vote": 4, "voted": false },
                    { "option_id": 11, "content": "Vũng Tàu", "total_vote": 2, "voted": true }
                ]
            }
        });
        let dto = parse_poll_dto(&resp, 0).unwrap();
        assert_eq!(dto.poll_id, 5511);
        assert_eq!(dto.question, "Cuối tuần đi đâu?");
        assert!(dto.allow_multi);
        assert!(!dto.allow_add_option);
        assert!(dto.is_anonymous);
        assert_eq!(dto.expired_time, 1_700_010_000_000);
        assert_eq!(dto.options[0].votes, 4);
        assert!(dto.options[1].voted);
    }

    #[test]
    fn parse_pinned_board_response_filters_empty_and_external_link_urls() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "items": [
                    {
                        "id": "b-empty",
                        "creatorId": "1001",
                        "createTime": 1_700_000_050_000_i64,
                        "params": "{\"msgId\":\"pin-bad-url\",\"title\":\"Ghi chú\",\"href\":\"http://evil.example/a.png\"}"
                    },
                    {
                        "id": "b-link",
                        "creatorId": "1001",
                        "createTime": 1_700_000_051_000_i64,
                        "params": "{\"msgId\":\"pin-link\",\"msgType\":\"chat.link\",\"title\":\"Link ngoài\",\"href\":\"https://vnexpress.net/bai-1\"}"
                    }
                ]
            }
        });
        let pins = parse_pinned_board_response(&resp, "grp-9", "9999", "", true, &HashMap::new());
        assert_eq!(pins.len(), 2);
        assert!(pins[0].attachment_url.is_none());
        assert!(pins[1].attachment_url.is_none());
    }

    #[test]
    fn parse_unread_marks_response_handles_nested_string_empty_and_unknown_schema() {
        let nested_resp = json!({
            "error_code": 0,
            "data": {
                "data": "{\"convsUser\":[{\"id\":\"111\",\"ts\":\"1700\"},\"222\"],\"convsGroup\":[{\"id\":\"g888111\"},\"group_888222\"]}",
                "status": 1
            }
        });
        let parsed = parse_unread_marks_response(&nested_resp).unwrap();
        assert!(parsed.contains("111"));
        assert!(parsed.contains("222"));
        assert!(parsed.contains("888111"));
        assert!(parsed.contains("888222"));
        assert_eq!(parsed.len(), 4);

        let empty_valid = json!({
            "error_code": 0,
            "data": { "convsUser": [], "convsGroup": [] }
        });
        let parsed_empty = parse_unread_marks_response(&empty_valid).unwrap();
        assert!(parsed_empty.is_empty());

        let err_resp = json!({
            "error_code": -1,
            "data": { "convsUser": [], "convsGroup": [] }
        });
        assert!(parse_unread_marks_response(&err_resp).is_none());

        let unknown_schema = json!({
            "error_code": 0,
            "data": { "unexpected": true }
        });
        assert!(parse_unread_marks_response(&unknown_schema).is_none());
    }

    #[test]
    fn apply_last_msgs_extracts_explicit_read_state_and_merges_unread_marks() {
        let mut threads = vec![
            ThreadDto {
                id: "u-read-explicit".into(),
                title: "A".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: String::new(),
                last_ts: 0,
                unread_count: 2,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "u-seen-bool".into(),
                title: "B".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: String::new(),
                last_ts: 0,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "u-unknown".into(),
                title: "C".into(),
                avatar: String::new(),
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
            },
            ThreadDto {
                id: "u-no-last-msg".into(),
                title: "D".into(),
                avatar: String::new(),
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
            },
        ];

        let last_msgs = json!({
            "data": {
                "msgs": [
                    {
                        "uidFrom": "u-read-explicit",
                        "idTo": "me-1",
                        "content": "hello",
                        "ts": 1_700_000_010_000_i64,
                        "unreadCount": 0
                    },
                    {
                        "uidFrom": "u-seen-bool",
                        "idTo": "me-1",
                        "content": "seen already",
                        "ts": 1_700_000_020_000_i64,
                        "seen": true
                    },
                    {
                        "uidFrom": "u-unknown",
                        "idTo": "me-1",
                        "content": "no unread field",
                        "ts": 1_700_000_030_000_i64
                    }
                ]
            }
        });

        let mut known = apply_last_msgs_to_threads_with_read_state(
            &mut threads,
            &last_msgs,
            "me-1",
            "",
            &HashMap::new(),
        );
        assert!(known.contains("u-read-explicit"));
        assert!(known.contains("u-seen-bool"));
        assert!(!known.contains("u-unknown"));
        assert!(!known.contains("u-no-last-msg"));
        assert_eq!(
            threads.iter().find(|t| t.id == "u-read-explicit").unwrap().unread_count,
            0
        );
        assert_eq!(
            threads.iter().find(|t| t.id == "u-seen-bool").unwrap().unread_count,
            0
        );

        let mut unread_marks = HashSet::new();
        unread_marks.insert("u-seen-bool".to_string());
        apply_unread_marks_to_threads(&mut threads, &unread_marks, &mut known);
        // Explicit manual unread mark overrides read state
        assert_eq!(
            threads.iter().find(|t| t.id == "u-seen-bool").unwrap().unread_count,
            1
        );
        assert!(known.contains("u-seen-bool"));
        // Explicit read state from get-last-msgs stays authoritative
        assert!(known.contains("u-read-explicit"));
        // Absence from get-unread-mark must NOT mark a thread with no explicit get-last-msgs
        // read/unread field as authoritative (prevents wiping normal incoming unread badges)
        assert!(!known.contains("u-unknown"));
        assert!(!known.contains("u-no-last-msg"));
    }

    #[test]
    fn build_seen_event_item_preserves_msg_type_with_text_fallback() {
        let photo = build_seen_event_item("m-10", "u-1", "u-1", "chat.photo");
        assert_eq!(photo["msgType"], "chat.photo");
        assert_eq!(photo["cmi"], "m-10");
        assert_eq!(photo["gmi"], "m-10");

        let fallback = build_seen_event_item("m-11", "u-1", "u-1", "   ");
        assert_eq!(fallback["msgType"], "chat.text");
    }

    #[test]
    fn protocol_service_hosts_excludes_profile_for_friend_endpoints() {
        let mut session = Session::new().unwrap();
        session.domains.friend = "https://tt-friend-wpa.chat.zalo.me".into();
        session.domains.profile = "https://tt-profile-wpa.chat.zalo.me".into();
        session.domains.group = "https://tt-group-wpa.chat.zalo.me".into();

        // `domains.candidates("friend")` includes `self.profile`, which would overwrite
        // real business errors on `/api/friend/*` if retried.
        let raw_candidates = session.domains.candidates("friend");
        assert!(raw_candidates.iter().any(|h| h == "https://tt-profile-wpa.chat.zalo.me"));

        let friend_hosts = protocol_service_hosts(&session, "friend");
        assert!(!friend_hosts.is_empty());
        assert!(friend_hosts.iter().any(|h| h == "https://tt-friend-wpa.chat.zalo.me"));
        assert!(!friend_hosts.iter().any(|h| h.contains("profile-wpa")));

        let group_hosts = protocol_service_hosts(&session, "group");
        assert!(group_hosts.iter().any(|h| h == "https://tt-group-wpa.chat.zalo.me"));
    }

    #[test]
    fn parse_sent_friend_requests_map_and_error_112_empty() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "111001": {
                    "userId": "111001",
                    "zaloName": "alice_acc",
                    "displayName": "Alice Nguyen",
                    "avatar": "//s160-ava-talk.zadn.vn/alice.jpg",
                    "fReqInfo": {
                        "message": "Chào Alice!",
                        "src": 30,
                        "time": 1_700_000_010_i64
                    }
                },
                "222002": {
                    "userId": "222002",
                    "zaloName": "bob_acc",
                    "displayName": "Bob Tran",
                    "avatar": "https://s160-ava-talk.zadn.vn/default",
                    "fReqInfo": {
                        "message": "Kết bạn nhé",
                        "src": 30,
                        "time": 1_700_000_050_i64
                    }
                }
            }
        });
        let items = parse_sent_friend_requests_response(&resp).unwrap();
        assert_eq!(items.len(), 2);
        // Sorted newest first
        assert_eq!(items[0].user_id, "222002");
        assert_eq!(items[0].display_name, "Bob Tran");
        assert_eq!(items[0].account_name, "bob_acc");
        assert_eq!(items[0].avatar, "");
        assert_eq!(items[0].message, "Kết bạn nhé");
        assert_eq!(items[0].created_ts, 1_700_000_050_000);
        assert_eq!(items[0].direction, "sent");

        assert_eq!(items[1].user_id, "111001");
        assert_eq!(items[1].account_name, "alice_acc");
        assert_eq!(items[1].avatar, "https://s160-ava-talk.zadn.vn/alice.jpg");
        assert_eq!(items[1].created_ts, 1_700_000_010_000);
        assert_eq!(items[1].direction, "sent");

        // error_code == 112 MUST be treated as empty list, NOT error
        let empty_112 = json!({ "error_code": 112, "error_message": "Empty", "data": "" });
        assert!(parse_sent_friend_requests_response(&empty_112).unwrap().is_empty());

        // Real non-112 error must fail cleanly
        let err_resp = json!({ "error_code": 500, "error_message": "Server busy" });
        assert!(parse_sent_friend_requests_response(&err_resp).is_err());
    }

    #[test]
    fn parse_received_friend_requests_filters_recomm_type_2_only() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "expiredDuration": 86400,
                "recommItems": [
                    {
                        "recommItemType": 1,
                        "dataInfo": {
                            "userId": "900001",
                            "zaloName": "suggested_user",
                            "displayName": "Suggested Person",
                            "avatar": "https://ava.zdn.vn/s.jpg",
                            "recommType": 1,
                            "recommTime": 1_700_000_200_i64,
                            "recommInfo": { "message": "Có thể bạn quen" },
                            "isSeenFriendReq": 0
                        }
                    },
                    {
                        "recommItemType": 2,
                        "dataInfo": {
                            "userId": "333003",
                            "zaloName": "carol_acc",
                            "displayName": "Carol Le",
                            "avatar": "https://ava.zdn.vn/carol.jpg",
                            "recommType": 2,
                            "recommTime": 1_700_000_100_i64,
                            "recommInfo": { "message": "Mình là Carol ở dự án X" },
                            "isSeenFriendReq": 1
                        }
                    },
                    {
                        "recommItemType": 2,
                        "dataInfo": {
                            "userId": "444004",
                            "zaloName": "dave_acc",
                            "displayName": "Dave Pham",
                            "avatar": "",
                            "recommType": 2,
                            "recommTime": 1_700_000_300_i64,
                            "recommInfo": { "customText": "Xin chào!" },
                            "isSeenFriendReq": false
                        }
                    }
                ]
            }
        });
        let items = parse_received_friend_requests_response(&resp).unwrap();
        assert_eq!(items.len(), 2);
        // RecommType == 1 MUST be excluded
        assert!(!items.iter().any(|i| i.user_id == "900001"));

        assert_eq!(items[0].user_id, "444004");
        assert_eq!(items[0].account_name, "dave_acc");
        assert_eq!(items[0].message, "Xin chào!");
        assert_eq!(items[0].created_ts, 1_700_000_300_000);
        assert_eq!(items[0].direction, "received");
        assert!(!items[0].is_seen);

        assert_eq!(items[1].user_id, "333003");
        assert_eq!(items[1].account_name, "carol_acc");
        assert_eq!(items[1].message, "Mình là Carol ở dự án X");
        assert_eq!(items[1].created_ts, 1_700_000_100_000);
        assert_eq!(items[1].direction, "received");
        assert!(items[1].is_seen);
    }

    #[test]
    fn send_friend_request_payload_and_special_error_codes_222_225_215() {
        let body = build_send_friend_request_body("555666", Some("Xin chào"), "imei-1");
        assert_eq!(body["toid"], "555666");
        assert_eq!(body["msg"], "Xin chào");
        assert_eq!(body["reqsrc"], 30);
        assert_eq!(body["imei"], "imei-1");
        assert_eq!(body["language"], "vi");
        assert_eq!(body["srcParams"], "{\"uidTo\":\"555666\"}");

        let ok_res = parse_send_friend_request_response(&json!({ "error_code": 0, "data": "" }), "555666").unwrap();
        assert!(ok_res.ok);
        assert!(!ok_res.already_friends);
        assert!(!ok_res.auto_accepted);
        assert_eq!(ok_res.action, "send_request");

        let code_225 = parse_send_friend_request_response(
            &json!({ "error_code": 225, "error_message": "Already friends" }),
            "555666",
        )
        .unwrap();
        assert!(code_225.ok);
        assert!(code_225.already_friends);
        assert!(!code_225.auto_accepted);

        let code_222 = parse_send_friend_request_response(
            &json!({ "error_code": 222, "error_message": "Already requested" }),
            "555666",
        )
        .unwrap();
        assert!(code_222.ok);
        assert!(!code_222.already_friends);
        assert!(code_222.auto_accepted);

        let err_215 = parse_send_friend_request_response(
            &json!({ "error_code": 215, "error_message": "Privacy" }),
            "555666",
        )
        .unwrap_err();
        assert!(err_215.to_string().contains("215"));
    }

    #[test]
    fn leave_group_and_kick_members_payloads_and_response_parsing() {
        let leave_body = build_leave_group_body("888999", "imei-1", true);
        assert_eq!(leave_body["grids"], json!(["888999"]));
        assert_eq!(leave_body["silent"], 1);
        assert_eq!(leave_body["language"], "vi");
        assert_eq!(leave_body["imei"], "imei-1");

        let leave_ok = parse_leave_group_response(
            &json!({ "error_code": 0, "data": { "memberError": [] } }),
            "888999",
        )
        .unwrap();
        assert_eq!(leave_ok.group_id, "888999");
        assert!(leave_ok.left);
        assert!(leave_ok.member_error.is_empty());

        let leave_partial = parse_leave_group_response(
            &json!({ "error_code": 0, "data": { "memberError": ["888999"] } }),
            "888999",
        )
        .unwrap();
        assert!(!leave_partial.left);
        assert_eq!(leave_partial.member_error, vec!["888999"]);

        let leave_166 = parse_leave_group_response(
            &json!({ "error_code": 166, "error_message": "Not allowed" }),
            "888999",
        )
        .unwrap_err();
        assert!(leave_166.to_string().contains("166"));

        let kick_members = vec!["u1".to_string(), "u2".to_string(), "u3".to_string()];
        let kick_body = build_kick_group_members_body("888999", &kick_members, "imei-1");
        assert_eq!(kick_body["grid"], "888999");
        assert_eq!(kick_body["members"], json!(["u1", "u2", "u3"]));
        assert_eq!(kick_body["imei"], "imei-1");

        let kick_ok = parse_kick_group_members_response(
            &json!({ "error_code": 0, "data": { "errorMembers": ["u2"] } }),
            "888999",
            &kick_members,
        )
        .unwrap();
        assert_eq!(kick_ok.group_id, "888999");
        assert_eq!(kick_ok.removed, vec!["u1", "u3"]);
        assert_eq!(kick_ok.failed, vec!["u2"]);

        let kick_165 = parse_kick_group_members_response(
            &json!({ "error_code": 165 }),
            "888999",
            &kick_members,
        )
        .unwrap_err();
        assert!(kick_165.to_string().contains("165"));

        let kick_166 = parse_kick_group_members_response(
            &json!({ "error_code": 166 }),
            "888999",
            &kick_members,
        )
        .unwrap_err();
        assert!(kick_166.to_string().contains("166"));
    }

    #[test]
    fn group_details_populates_creator_id_and_admin_ids_and_user_profile_extracts_is_blocked() {
        let group_info = json!({
            "creatorId": "1001",
            "adminIds": ["2002", 3003_u64, "2002"],
            "memVerList": ["1001_0", "2002_1", "3003_0", "4004_2"],
            "totalMember": 4
        });
        let mut profiles = Map::new();
        profiles.insert("1001".into(), json!({ "displayName": "Leader" }));
        profiles.insert("2002".into(), json!({ "displayName": "Deputy A" }));
        profiles.insert("4004".into(), json!({ "displayName": "Member D" }));

        let details = build_group_details_from_info(
            &group_info,
            &profiles,
            "1001",
            "Me",
            "",
            &HashMap::new(),
        );
        assert_eq!(details.total_members, Some(4));
        assert_eq!(details.creator_id, "1001");
        assert_eq!(details.admin_ids, vec!["2002", "3003"]);
        assert_eq!(details.members.len(), 4);
        assert_eq!(details.members[0].role, "Trưởng nhóm");
        assert_eq!(details.members[1].role, "Phó nhóm");
        assert_eq!(details.members[2].role, "Phó nhóm");
        assert_eq!(details.members[3].role, "Thành viên");

        let blocked_num = json!({
            "displayName": "Spammer",
            "zaloName": "spam_acc",
            "isFr": 0,
            "isBlocked": 1
        });
        let p1 = build_user_profile_dto("7001", Some(&blocked_num), "1001", "Me", "", &HashMap::new());
        assert_eq!(p1.is_friend, Some(false));
        assert_eq!(p1.is_blocked, Some(true));

        let unblocked_bool = json!({
            "displayName": "Friend",
            "isFr": 1,
            "isBlocked": false
        });
        let p2 = build_user_profile_dto("7002", Some(&unblocked_bool), "1001", "Me", "", &HashMap::new());
        assert_eq!(p2.is_friend, Some(true));
        assert_eq!(p2.is_blocked, Some(false));
    }

    #[test]
    fn parse_remote_history_page_preserves_cli_msg_id_and_folds_reactions() {
        let resp = json!({
            "error_code": 0,
            "data": {
                "hasMore": 0,
                "groupMsgs": [
                    {
                        "msgId": "75001",
                        "cliMsgId": "1782286269001",
                        "uidFrom": "200",
                        "idTo": "888111",
                        "msgType": "chat.text",
                        "content": "Tin nhan 1",
                        "ts": 1_782_286_269_500_i64,
                        "reactions": [
                            { "rIcon": "/-strong", "uids": ["300"] },
                            { "rIcon": "", "rType": -1, "uid": "400" }
                        ]
                    },
                    {
                        "msgId": "75002",
                        "cliMsgId": "1782286269002",
                        "uidFrom": "100",
                        "idTo": "888111",
                        "msgType": "chat.text",
                        "content": "Tin nhan 2",
                        "ts": 1_782_286_269_600_i64
                    },
                    {
                        "msgId": "75009",
                        "cliMsgId": "1782286269099",
                        "uidFrom": "0",
                        "idTo": "888111",
                        "msgType": "chat.reaction",
                        "content": "{\"rMsg\":[{\"gMsgID\":0,\"cMsgID\":1782286269001,\"msgType\":1}],\"rIcon\":\"/-heart\",\"rType\":5}"
                    }
                ],
                "reactGroups": [
                    {
                        "uidFrom": "200",
                        "idTo": "888111",
                        "content": "{\"rMsg\":[{\"gMsgID\":75002,\"cMsgID\":1782286269002,\"msgType\":1}],\"rIcon\":\":>\",\"rType\":0}"
                    },
                    {
                        "uidFrom": "200",
                        "idTo": "888111",
                        "content": "{\"rMsg\":[{\"gMsgID\":75002,\"cMsgID\":1782286269002,\"msgType\":1}],\"rIcon\":\":o\",\"rType\":32}"
                    }
                ]
            }
        });

        let page = parse_remote_history_page(&resp, "888111", "100", "", true, None).unwrap();
        assert_eq!(page.messages.len(), 2);

        let m1 = &page.messages[0];
        assert_eq!(m1.msg_id, "75001");
        assert_eq!(m1.cli_msg_id.as_deref(), Some("1782286269001"));
        assert_eq!(m1.reactions.len(), 2);
        let like = m1.reactions.iter().find(|r| r.emoji == "👍").unwrap();
        assert_eq!(like.count, 1);
        assert!(!like.me);
        let heart = m1.reactions.iter().find(|r| r.emoji == "❤️").unwrap();
        assert_eq!(heart.count, 1);
        assert!(heart.me);

        // Peer switched 😆 -> 😮 in reactGroups
        let m2 = &page.messages[1];
        assert_eq!(m2.msg_id, "75002");
        assert_eq!(m2.cli_msg_id.as_deref(), Some("1782286269002"));
        assert_eq!(m2.reactions.len(), 1);
        assert_eq!(m2.reactions[0].emoji, "😮");
        assert_eq!(m2.reactions[0].count, 1);
        assert!(!m2.reactions[0].me);
    }
}
