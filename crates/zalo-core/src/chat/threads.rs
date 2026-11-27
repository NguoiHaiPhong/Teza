//! Thread list synchronization (`getfriends`, `getlg/v4`, `getmg-v2`, `get-last-msgs`) and history loading.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::chat::parser::{
    clean_group_id, collect_reactions_from_node, extract_content_text, extract_poll_activity_dto,
    extract_poll_activity_update, extract_thread_id, format_thread_preview, is_group_flag,
    is_poll_activity_text, is_raw_numeric_id, normalize_avatar_url, normalize_media_url,
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

fn profile_entry_has_identity(v: &Value) -> bool {
    pick_str(v, &["displayName", "zaloName", "name", "display_name"]).is_some()
        || pick_str(v, &["avatar", "avt", "profile_avatar"])
            .is_some_and(|a| !normalize_avatar_url(&a).is_empty())
}

fn compute_missing_profile_uids(requested: &[String], found: &Map<String, Value>) -> Vec<String> {
    let mut missing = Vec::new();
    for raw in requested {
        let clean = clean_uid(raw);
        if clean.is_empty() || clean == "0" || missing.iter().any(|x| x == &clean) {
            continue;
        }
        let resolved = found
            .get(&clean)
            .is_some_and(profile_entry_has_identity);
        if !resolved {
            missing.push(clean);
        }
    }
    missing
}

fn merge_mini_profile_maps(primary: &mut Map<String, Value>, fallback: Map<String, Value>) {
    for (uid, val) in fallback {
        let clean = clean_uid(&uid);
        if clean.is_empty() || clean == "0" {
            continue;
        }
        let should_replace = match primary.get(&clean) {
            None => true,
            Some(existing) => {
                !profile_entry_has_identity(existing) && profile_entry_has_identity(&val)
            }
        };
        if should_replace {
            primary.insert(clean, val);
        }
    }
}

fn to_versioned_profile_ids(uids: &[String]) -> Vec<String> {
    uids.iter()
        .map(|uid| {
            let clean = uid.trim();
            if clean.contains('_') {
                clean.to_string()
            } else {
                format!("{clean}_0")
            }
        })
        .collect()
}

pub async fn get_mini_profiles(session: &Session, user_ids: &[String]) -> Result<Map<String, Value>> {
    if !session.is_authenticated() || user_ids.is_empty() {
        return Ok(Map::new());
    }
    let mut requested_clean: Vec<String> = Vec::new();
    for uid in user_ids {
        let clean = clean_uid(uid);
        if !clean.is_empty() && clean != "0" && !requested_clean.iter().any(|x| x == &clean) {
            requested_clean.push(clean);
        }
    }
    if requested_clean.is_empty() {
        return Ok(Map::new());
    }
    let versioned_ids = to_versioned_profile_ids(&requested_clean);

    let mut profile_hosts = session.domains.candidates("profile");
    for extra in [
        "https://profile-wpa.zaloapp.com",
        "https://tt-profile-wpa.chat.zalo.me",
    ] {
        if !profile_hosts.iter().any(|h| h == extra) {
            profile_hosts.push(extra.to_string());
        }
    }

    let mut merged = Map::new();
    let members_body = obj(&[("friend_pversion_map", json!(versioned_ids))]);
    let members_params = session.encoded_params(&members_body)?;
    for host in &profile_hosts {
        let url = format!(
            "{}/api/social/group/members?{}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session
            .post_form(&url, format!("params={members_params}"))
            .await
        {
            Ok(raw) => {
                let resp =
                    unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let code = resp.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                if code == 0 {
                    let parsed = parse_profiles_response(&resp);
                    if !parsed.is_empty() {
                        merge_mini_profile_maps(&mut merged, parsed);
                        break;
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

    // Always query `/api/social/friend/getprofiles/v2` for any UIDs still missing after
    // `/api/social/group/members` (common for non-friend / stranger direct threads).
    let still_missing = compute_missing_profile_uids(&requested_clean, &merged);
    if !still_missing.is_empty() {
        let missing_versioned = to_versioned_profile_ids(&still_missing);
        let v2_body = obj(&[
            ("friend_pversion_map", json!(missing_versioned)),
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
                    let resp =
                        unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                    ensure_not_expired(&resp)?;
                    let code = resp.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                    if code == 0 {
                        let parsed = parse_profiles_response(&resp);
                        if !parsed.is_empty() {
                            merge_mini_profile_maps(&mut merged, parsed);
                            break;
                        }
                    } else {
                        eprintln!(
                            "[litechat][profiles] friend/getprofiles/v2 ({host}) error_code={code}"
                        );
                    }
                }
                Err(e) if is_expired_err(&e) => return Err(e),
                Err(e) => {
                    eprintln!("[litechat][profiles] friend/getprofiles/v2 ({host}) error: {e}");
                }
            }
        }
    }

    Ok(merged)
}

/// Resolve display name & avatar for raw-UID / unresolved direct threads in 50-UID batches.
/// Returns `uid -> (display_name, avatar_url)`.
pub async fn hydrate_unresolved_direct_threads(
    session: &Session,
    uids: &[String],
) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    if !session.is_authenticated() || uids.is_empty() {
        return out;
    }
    let mut clean_uids: Vec<String> = Vec::new();
    for raw in uids {
        let clean = clean_uid(raw);
        if clean.is_empty()
            || clean == "0"
            || (!session.profile.uid.is_empty() && clean == session.profile.uid)
            || (!session.profile.send2me_id.is_empty() && clean == session.profile.send2me_id)
            || clean_uids.iter().any(|x| x == &clean)
        {
            continue;
        }
        clean_uids.push(clean);
    }
    if clean_uids.is_empty() {
        return out;
    }
    for chunk in clean_uids.chunks(50) {
        match get_mini_profiles(session, chunk).await {
            Ok(mp) => {
                for (k, v) in mp {
                    let uid = clean_uid(&k);
                    if uid.is_empty() || uid == "0" {
                        continue;
                    }
                    let name = pick_str(&v, &["displayName", "zaloName", "name", "display_name"])
                        .unwrap_or_default();
                    let avatar = pick_str(&v, &["avatar", "avt", "profile_avatar"])
                        .map(|u| normalize_avatar_url(&u))
                        .unwrap_or_default();
                    if !name.is_empty() || !avatar.is_empty() {
                        out.insert(uid, (name, avatar));
                    }
                }
            }
            Err(e) if is_expired_err(&e) => {
                eprintln!("[litechat][profiles] hydrate_unresolved_direct_threads expired: {e}");
                break;
            }
            Err(e) => {
                eprintln!("[litechat][profiles] hydrate_unresolved_direct_threads error: {e}");
            }
        }
    }
    out
}

const LAST_MSG_BUCKETS: &[(&str, bool)] = &[
    ("msgs", false),
    ("groupMsgs", true),
    ("lastMsgs", false),
    ("strangerMsgs", false),
    ("strangers", false),
    ("otherMsgs", false),
    ("conversations", false),
    ("convs", false),
];

fn normalize_last_msg_item(item: Value) -> Value {
    let Some(obj_map) = item.as_object() else {
        return item;
    };
    if obj_map.contains_key("content") || obj_map.contains_key("msg") {
        return item;
    }
    let nested = obj_map
        .get("lastMsg")
        .or_else(|| obj_map.get("last_msg"))
        .and_then(unwrap_json_field);
    let Some(Value::Object(inner_map)) = nested else {
        return item;
    };
    let mut merged = obj_map.clone();
    for (k, v) in inner_map {
        merged.entry(k).or_insert(v);
    }
    Value::Object(merged)
}

fn collect_last_msg_entries_inner(node: &Value, depth: u8, out: &mut Vec<(bool, Value)>) {
    let owned = unwrap_json_field(node);
    let current = owned.as_ref().unwrap_or(node);
    let Some(map) = current.as_object() else {
        if let Value::Array(arr) = current {
            for item in arr {
                let item_owned = unwrap_json_field(item);
                let effective = item_owned.as_ref().unwrap_or(item);
                if effective.is_object() {
                    out.push((false, normalize_last_msg_item(effective.clone())));
                }
            }
        }
        return;
    };

    for &(bucket, in_group) in LAST_MSG_BUCKETS {
        if let Some(raw) = map.get(bucket) {
            let bucket_owned = unwrap_json_field(raw);
            let bucket_val = bucket_owned.as_ref().unwrap_or(raw);
            if let Some(arr) = bucket_val.as_array() {
                for item in arr {
                    let item_owned = unwrap_json_field(item);
                    let effective = item_owned.as_ref().unwrap_or(item);
                    if effective.is_object() {
                        out.push((in_group, normalize_last_msg_item(effective.clone())));
                    }
                }
            }
        }
    }

    if depth > 0 {
        if let Some(inner) = map.get("data") {
            collect_last_msg_entries_inner(inner, depth - 1, out);
        }
    }
}

fn collect_last_msg_entries(root: &Value) -> Vec<(bool, Value)> {
    let mut out = Vec::new();
    collect_last_msg_entries_inner(root, 3, &mut out);
    out
}

pub async fn get_last_msgs(session: &Session) -> Result<Value> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let body = obj(&[
        ("threadIdLocalMsgId", json!("{}")),
        ("imei", json!(session.imei)),
    ]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));

    let mut conv_hosts = session.domains.candidates("conversation");
    if conv_hosts.is_empty() && !session.domains.conversation.is_empty() {
        push_unique_host(&mut conv_hosts, &session.domains.conversation);
    }
    let mut fallback_hosts = conv_hosts.clone();
    for h in session.domains.candidates("chat") {
        push_unique_host(&mut fallback_hosts, &h);
    }

    let attempts: [(&str, &[String]); 2] = [
        ("api/preloadconvers/get-last-msgs", &conv_hosts),
        ("api/message/get-last-msgs", &fallback_hosts),
    ];

    let mut empty_ok_resp: Option<Value> = None;
    let mut last_err: Option<anyhow::Error> = None;

    for (path, hosts) in attempts {
        for host in hosts {
            let url = format!(
                "{}/{path}?{}&{params}",
                host.trim_end_matches('/'),
                session.common_params()
            );
            match session.get_json(&url).await {
                Ok(raw) => {
                    let resp =
                        unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                    ensure_not_expired(&resp)?;
                    let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
                    if code != 0 {
                        eprintln!("[litechat][threads] {path} ({host}) error_code={code}");
                        last_err = Some(anyhow::anyhow!("{path} error_code={code}"));
                        continue;
                    }
                    if !collect_last_msg_entries(&resp).is_empty() {
                        return Ok(resp);
                    }
                    if empty_ok_resp.is_none() {
                        empty_ok_resp = Some(resp);
                    }
                    // Try fallback endpoint/host if preloadconvers returned empty buckets.
                    break;
                }
                Err(e) if is_expired_err(&e) => return Err(e),
                Err(e) => {
                    eprintln!("[litechat][threads] {path} ({host}) error: {e}");
                    last_err = Some(e);
                }
            }
        }
    }

    if let Some(resp) = empty_ok_resp {
        return Ok(resp);
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Không thể tải danh sách tin nhắn gần nhất")))
}

fn is_numeric_direct_uid(uid: &str) -> bool {
    !uid.is_empty() && uid != "0" && uid.chars().all(|c| c.is_ascii_digit())
}

fn parse_pinned_thread_ids_split(resp: &Value) -> (HashSet<String>, HashSet<String>) {
    let mut all_pinned = HashSet::new();
    let mut direct_pinned = HashSet::new();
    let data_owned = resp.get("data").and_then(unwrap_json_field);
    let data = data_owned.as_ref().unwrap_or(resp);
    let inner_owned = data.get("data").and_then(unwrap_json_field);
    let effective = inner_owned.as_ref().unwrap_or(data);

    let mut raw_arrays: Vec<Vec<Value>> = Vec::new();
    if let Some(arr) = effective.as_array() {
        raw_arrays.push(arr.clone());
    }
    for key in ["conversations", "pinConvers", "list", "items"] {
        if let Some(raw) = effective.get(key) {
            let unwrapped = unwrap_json_field(raw);
            let val = unwrapped.as_ref().unwrap_or(raw);
            if let Some(arr) = val.as_array() {
                raw_arrays.push(arr.clone());
            }
        }
    }

    for arr in raw_arrays {
        for item in arr {
            let (raw_id, explicit_group) = match &item {
                Value::String(s) => (Some(s.trim().to_string()), false),
                Value::Number(n) => (Some(n.to_string()), false),
                Value::Object(_) => {
                    let is_grp = is_group_flag(&item)
                        || item.get("is_group").and_then(value_as_bool) == Some(true)
                        || item.get("type").and_then(value_as_i64) == Some(1)
                        || item.get("grid").is_some()
                        || item.get("groupId").is_some();
                    (
                        pick_str(&item, &["id", "threadId", "conversId", "grid", "uid"]),
                        is_grp,
                    )
                }
                _ => (None, false),
            };
            if let Some(id) = raw_id {
                let prefixed_group = id.starts_with("group_")
                    || id.starts_with('g')
                    || id.starts_with('G');
                let is_grp = explicit_group || prefixed_group;
                let clean = if is_grp {
                    clean_group_id(&id).to_string()
                } else {
                    clean_uid(&id)
                };
                if !clean.is_empty() && clean != "0" {
                    all_pinned.insert(clean.clone());
                    if !is_grp && is_numeric_direct_uid(&clean) {
                        direct_pinned.insert(clean);
                    }
                }
            }
        }
    }
    (all_pinned, direct_pinned)
}

async fn fetch_pinned_thread_ids_split(
    session: &Session,
) -> Result<(HashSet<String>, HashSet<String>)> {
    if !session.is_authenticated() {
        return Ok((HashSet::new(), HashSet::new()));
    }
    let hosts = session.domains.candidates("conversation");
    if hosts.is_empty() {
        return Ok((HashSet::new(), HashSet::new()));
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let params = session
        .encoded_params(&body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(&body));
    let mut last_err: Option<anyhow::Error> = None;
    for host in &hosts {
        let url = format!(
            "{}/api/pinconvers/list?{}&{params}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session.get_json(&url).await {
            Ok(raw) => {
                let resp =
                    unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
                if code == 0 {
                    return Ok(parse_pinned_thread_ids_split(&resp));
                }
                last_err = Some(anyhow::anyhow!("pinconvers/list error_code={code}"));
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(err) = last_err {
        return Err(err);
    }
    Ok((HashSet::new(), HashSet::new()))
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

fn parse_unread_marks_split(resp: &Value) -> Option<(HashSet<String>, HashSet<String>)> {
    if let Some(code) = resp.get("error_code").and_then(value_as_i64) {
        if code != 0 {
            return None;
        }
    }
    let data_node = unwrap_unread_mark_data_node(resp)?;
    let mut user_ids = HashSet::new();
    let mut group_ids = HashSet::new();
    let found_users =
        collect_unread_mark_ids(&data_node, &["convsUser", "convs_user"], false, &mut user_ids);
    let found_groups = collect_unread_mark_ids(
        &data_node,
        &["convsGroup", "convs_group"],
        true,
        &mut group_ids,
    );
    if !found_users && !found_groups {
        return None;
    }
    Some((user_ids, group_ids))
}

/// Parse `/api/convers/get-unread-mark` response into the set of unread thread IDs.
/// Returns `None` on non-zero `error_code` or unrecognized response structure so callers
/// safely fall back without clobbering local unread state.
pub fn parse_unread_marks_response(resp: &Value) -> Option<HashSet<String>> {
    let (mut users, groups) = parse_unread_marks_split(resp)?;
    users.extend(groups);
    Some(users)
}

async fn fetch_unread_marks_split(
    session: &Session,
) -> Result<Option<(HashSet<String>, HashSet<String>)>> {
    if !session.is_authenticated() {
        return Ok(None);
    }
    let mut conv_hosts = session.domains.candidates("conversation");
    if conv_hosts.is_empty() && !session.domains.conversation.is_empty() {
        push_unique_host(&mut conv_hosts, &session.domains.conversation);
    }
    if conv_hosts.is_empty() {
        return Ok(None);
    }
    let attempts = [
        ("api/conv/getUnreadMark", json!({})),
        (
            "api/convers/get-unread-mark",
            obj(&[("imei", json!(session.imei))]),
        ),
    ];
    for (path, body) in attempts {
        let params = session
            .encoded_params(&body)
            .map(|p| format!("params={p}"))
            .unwrap_or_else(|_| Session::plain_params(&body));
        for base_host in &conv_hosts {
            let url = format!(
                "{}/{path}?{}&{params}",
                base_host.trim_end_matches('/'),
                session.common_params()
            );
            let raw = match session.get_json(&url).await {
                Ok(v) => v,
                Err(e) if is_expired_err(&e) => return Err(e),
                Err(_) => continue,
            };
            let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
            ensure_not_expired(&resp)?;
            if let Some(parsed) = parse_unread_marks_split(&resp) {
                return Ok(Some(parsed));
            }
        }
    }
    Ok(None)
}

/// Best-effort fetch of `/api/conv/getUnreadMark` (with fallback to legacy
/// `/api/convers/get-unread-mark`) on the `conversation` service host.
/// Propagates `SessionError::Expired`; falls back to `Ok(None)` on network errors or unknown schema.
pub async fn fetch_unread_marks(session: &Session) -> Result<Option<HashSet<String>>> {
    Ok(fetch_unread_marks_split(session)
        .await?
        .map(|(mut users, groups)| {
            users.extend(groups);
            users
        }))
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
                let clean = clean_uid(&uid);
                if !clean.is_empty() && clean != "0" {
                    aliases.insert(clean, alias);
                }
            }
        }
    }
    Ok(aliases)
}

fn parse_remote_label_direct_uids(resp: &Value) -> Vec<String> {
    if resp
        .get("error_code")
        .and_then(value_as_i64)
        .is_some_and(|c| c != 0)
    {
        return Vec::new();
    }
    let Some(data_node) = unwrap_history_data_node(resp) else {
        return Vec::new();
    };
    let mut raw_labels: Vec<Value> = Vec::new();
    if let Some(arr) = data_node.as_array() {
        raw_labels.extend(arr.iter().cloned());
    } else {
        if let Some(ld) = data_node.get("labelData").and_then(unwrap_json_field) {
            if let Some(arr) = ld.as_array() {
                raw_labels.extend(arr.iter().cloned());
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
    let mut seen = HashSet::new();
    for item in raw_labels {
        for key in ["conversations", "conversationIds", "threadIds", "convs"] {
            let Some(node) = item.get(key) else {
                continue;
            };
            let unwrapped = unwrap_json_field(node);
            let val = unwrapped.as_ref().unwrap_or(node);
            let Some(arr) = val.as_array() else {
                continue;
            };
            for entry in arr {
                let raw_tid = match entry {
                    Value::String(s) => Some(s.trim().to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Object(_) => {
                        let is_grp = is_group_flag(entry)
                            || entry.get("is_group").and_then(value_as_bool) == Some(true)
                            || entry.get("type").and_then(value_as_i64) == Some(1)
                            || entry.get("grid").is_some()
                            || entry.get("groupId").is_some();
                        if is_grp {
                            None
                        } else {
                            pick_str(entry, &["thread_id", "threadId", "id", "uid", "userId"])
                        }
                    }
                    _ => None,
                };
                if let Some(raw) = raw_tid {
                    if raw.starts_with("group_") || raw.starts_with('g') || raw.starts_with('G') {
                        continue;
                    }
                    let clean = clean_uid(&raw);
                    if is_numeric_direct_uid(&clean) && seen.insert(clean.clone()) {
                        out.push(clean);
                    }
                }
            }
            break;
        }
    }
    out
}

async fn fetch_remote_label_direct_uids(session: &Session) -> Result<Vec<String>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let body = obj(&[("imei", json!(session.imei))]);
    let params = session.encoded_params(&body)?;
    let path_and_query = format!("/api/label/get?{}&params={params}", session.common_params());
    let raw = session.get_endpoint("label", &path_and_query).await?;
    let resp = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
    ensure_not_expired(&resp)?;
    Ok(parse_remote_label_direct_uids(&resp))
}

fn parse_direct_thread_ids_from_conv_list(resp: &Value) -> Vec<String> {
    if resp
        .get("error_code")
        .and_then(value_as_i64)
        .is_some_and(|c| c != 0)
    {
        return Vec::new();
    }
    let data_owned = resp.get("data").and_then(unwrap_json_field);
    let data = data_owned.as_ref().unwrap_or(resp);
    let inner_owned = data.get("data").and_then(unwrap_json_field);
    let effective = inner_owned.as_ref().unwrap_or(data);

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push_entry = |entry: &Value, force_direct: bool| {
        let (raw_id, explicit_group) = match entry {
            Value::String(s) => (Some(s.trim().to_string()), false),
            Value::Number(n) => (Some(n.to_string()), false),
            Value::Object(_) => {
                let is_grp = !force_direct
                    && (is_group_flag(entry)
                        || entry.get("is_group").and_then(value_as_bool) == Some(true)
                        || entry.get("type").and_then(value_as_i64) == Some(1)
                        || entry.get("grid").is_some()
                        || entry.get("groupId").is_some());
                let id = pick_str(
                    entry,
                    &["thread_id", "threadId", "id", "uid", "userId", "conversId"],
                );
                (id, is_grp)
            }
            _ => (None, false),
        };
        let Some(raw) = raw_id else {
            return;
        };
        if explicit_group
            || (!force_direct
                && (raw.starts_with("group_") || raw.starts_with('g') || raw.starts_with('G')))
        {
            return;
        }
        let clean = clean_uid(&raw);
        if is_numeric_direct_uid(&clean) && seen.insert(clean.clone()) {
            out.push(clean);
        }
    };

    if let Some(arr) = effective.as_array() {
        for entry in arr {
            push_entry(entry, false);
        }
    }
    for key in ["convsUser", "convs_user"] {
        if let Some(raw) = effective.get(key) {
            let unwrapped = unwrap_json_field(raw);
            let val = unwrapped.as_ref().unwrap_or(raw);
            if let Some(arr) = val.as_array() {
                for entry in arr {
                    push_entry(entry, true);
                }
            }
        }
    }
    for key in [
        "threads",
        "items",
        "conversations",
        "convs",
        "archivedChats",
        "archived_chats",
        "hiddenConvers",
        "hidden_convers",
        "list",
    ] {
        if let Some(raw) = effective.get(key) {
            let unwrapped = unwrap_json_field(raw);
            let val = unwrapped.as_ref().unwrap_or(raw);
            if let Some(arr) = val.as_array() {
                for entry in arr {
                    push_entry(entry, false);
                }
            }
        }
    }
    out
}

fn conversation_and_chat_hosts(session: &Session) -> Vec<String> {
    let mut hosts = session.domains.candidates("conversation");
    if hosts.is_empty() && !session.domains.conversation.is_empty() {
        push_unique_host(&mut hosts, &session.domains.conversation);
    }
    for h in session.domains.candidates("chat") {
        push_unique_host(&mut hosts, &h);
    }
    hosts
}

async fn fetch_direct_uids_from_conv_endpoint(
    session: &Session,
    path: &str,
    body: &Value,
) -> Result<Vec<String>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
    }
    let hosts = conversation_and_chat_hosts(session);
    if hosts.is_empty() {
        return Ok(Vec::new());
    }
    let params = session
        .encoded_params(body)
        .map(|p| format!("params={p}"))
        .unwrap_or_else(|_| Session::plain_params(body));
    let clean_path = path.trim_start_matches('/');
    let mut last_err: Option<anyhow::Error> = None;
    for host in &hosts {
        let url = format!(
            "{}/{clean_path}?{}&{params}",
            host.trim_end_matches('/'),
            session.common_params()
        );
        match session.get_json(&url).await {
            Ok(raw) => {
                let resp =
                    unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
                ensure_not_expired(&resp)?;
                let code = resp.get("error_code").and_then(value_as_i64).unwrap_or(0);
                if code == 0 {
                    return Ok(parse_direct_thread_ids_from_conv_list(&resp));
                }
                last_err = Some(anyhow::anyhow!("{clean_path} error_code={code}"));
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(err) = last_err {
        return Err(err);
    }
    Ok(Vec::new())
}

async fn fetch_archived_direct_uids(session: &Session) -> Result<Vec<String>> {
    let body = obj(&[("version", json!(0)), ("imei", json!(session.imei))]);
    fetch_direct_uids_from_conv_endpoint(session, "api/archivedchat/list", &body).await
}

async fn fetch_hidden_direct_uids(session: &Session) -> Result<Vec<String>> {
    let body = obj(&[("imei", json!(session.imei))]);
    fetch_direct_uids_from_conv_endpoint(session, "api/hiddenconvers/get-all", &body).await
}

#[allow(clippy::too_many_arguments)]
fn materialize_discovered_direct_threads(
    threads: &mut Vec<ThreadDto>,
    my_uid: &str,
    send2me_id: &str,
    unread_user_ids: &HashSet<String>,
    pinned_direct_ids: &HashSet<String>,
    alias_map: &HashMap<String, String>,
    label_direct_ids: &[String],
    archived_direct_ids: &[String],
    hidden_direct_ids: &[String],
) -> Vec<String> {
    let mut seen: HashSet<String> = threads.iter().map(|t| t.id.clone()).collect();
    let clean_my_uid = clean_uid(my_uid);
    let mut created = Vec::new();

    let mut candidate_uids: Vec<String> = Vec::new();
    let mut push_candidate = |raw: &str| {
        let uid = clean_uid(raw);
        if is_numeric_direct_uid(&uid)
            && (clean_my_uid.is_empty() || uid != clean_my_uid)
            && (send2me_id.is_empty() || uid != send2me_id)
            && !seen.contains(&uid)
            && !candidate_uids.iter().any(|x| x == &uid)
        {
            candidate_uids.push(uid);
        }
    };

    // Prioritize unread & pinned non-friend conversations first.
    let mut unread_sorted: Vec<&String> = unread_user_ids.iter().collect();
    unread_sorted.sort();
    for uid in unread_sorted {
        push_candidate(uid);
    }
    let mut pinned_sorted: Vec<&String> = pinned_direct_ids.iter().collect();
    pinned_sorted.sort();
    for uid in pinned_sorted {
        push_candidate(uid);
    }
    for uid in archived_direct_ids {
        push_candidate(uid);
    }
    for uid in hidden_direct_ids {
        push_candidate(uid);
    }
    for uid in label_direct_ids {
        push_candidate(uid);
    }
    let mut alias_uids: Vec<&String> = alias_map.keys().collect();
    alias_uids.sort();
    for uid in alias_uids {
        push_candidate(uid);
    }

    for uid in candidate_uids {
        if !seen.insert(uid.clone()) {
            continue;
        }
        let title = alias_map
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.clone());
        let is_pinned = pinned_direct_ids.contains(&uid);
        let unread_count = u32::from(unread_user_ids.contains(&uid));
        threads.push(ThreadDto {
            id: uid.clone(),
            title,
            avatar: String::new(),
            kind: ThreadKind::Direct,
            last_preview: String::new(),
            last_ts: 0,
            unread_count,
            is_pinned,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
        });
        created.push(uid);
    }
    created
}

/// Synchronize friends, groups, Cloud thread, non-friend conversations, pinned state, aliases,
/// and latest conversation previews.
pub async fn sync_threads_full(session: &Session) -> Result<SyncedThreads> {
    let mut threads = match get_friends(session).await {
        Ok(list) => list,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] get_friends failed: {e}");
            Vec::new()
        }
    };
    let friend_ids: HashSet<String> = threads
        .iter()
        .filter(|t| t.kind == ThreadKind::Direct)
        .map(|t| t.id.clone())
        .collect();
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
        Err(e) => {
            eprintln!("[litechat][threads] get_groups failed: {e}");
        }
    }

    let (last_msgs_res, aliases_res, unread_res, pinned_res, labels_res, archived_res, hidden_res) = tokio::join!(
        get_last_msgs(session),
        fetch_alias_map(session),
        fetch_unread_marks_split(session),
        fetch_pinned_thread_ids_split(session),
        fetch_remote_label_direct_uids(session),
        fetch_archived_direct_uids(session),
        fetch_hidden_direct_uids(session),
    );

    let last_msgs_opt = match last_msgs_res {
        Ok(v) => Some(v),
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] get_last_msgs failed: {e}");
            None
        }
    };
    let aliases = match aliases_res {
        Ok(map) => map,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_alias_map failed: {e}");
            HashMap::new()
        }
    };
    let (unread_user_ids, unread_all_ids) = match unread_res {
        Ok(Some((users, groups))) => {
            let mut all = users.clone();
            all.extend(groups);
            (users, Some(all))
        }
        Ok(None) => (HashSet::new(), None),
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_unread_marks failed: {e}");
            (HashSet::new(), None)
        }
    };
    let (pinned_synced, all_pinned_ids, direct_pinned_ids) = match pinned_res {
        Ok((all_pinned, direct_pinned)) => (true, all_pinned, direct_pinned),
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_pinned_thread_ids failed: {e}");
            (false, HashSet::new(), HashSet::new())
        }
    };
    let label_direct_ids = match labels_res {
        Ok(ids) => ids,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_remote_label_direct_uids failed: {e}");
            Vec::new()
        }
    };
    let archived_direct_ids = match archived_res {
        Ok(ids) => ids,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_archived_direct_uids failed: {e}");
            Vec::new()
        }
    };
    let hidden_direct_ids = match hidden_res {
        Ok(ids) => ids,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => {
            eprintln!("[litechat][threads] fetch_hidden_direct_uids failed: {e}");
            Vec::new()
        }
    };

    let newly_discovered_direct_ids = materialize_discovered_direct_threads(
        &mut threads,
        &session.profile.uid,
        &session.profile.send2me_id,
        &unread_user_ids,
        &direct_pinned_ids,
        &aliases,
        &label_direct_ids,
        &archived_direct_ids,
        &hidden_direct_ids,
    );

    // Separate direct non-friend / unresolved thread UIDs from group sender UIDs so direct
    // stranger profiles are ALWAYS resolved first and never truncated by group-sender caps.
    let mut direct_thread_need_uids: Vec<String> = Vec::new();
    let mut group_sender_need_uids: Vec<String> = Vec::new();
    let known_ids: HashSet<String> = threads.iter().map(|t| t.id.clone()).collect();
    let clean_my_uid = clean_uid(&session.profile.uid);

    for t in &threads {
        if t.kind == ThreadKind::Direct
            && t.id != clean_my_uid
            && t.id != session.profile.send2me_id
        {
            let is_non_friend = !friend_ids.contains(&t.id);
            let needs_title =
                t.title.trim().is_empty() || t.title == t.id || is_raw_numeric_id(&t.title);
            if (needs_title || (is_non_friend && t.avatar.is_empty()))
                && !direct_thread_need_uids.iter().any(|x| x == &t.id)
            {
                direct_thread_need_uids.push(t.id.clone());
            }
        }
    }

    if let Some(ref last_msgs) = last_msgs_opt {
        let entries = collect_last_msg_entries(last_msgs);
        for (in_group, item) in &entries {
            if let Some((tid, is_grp)) = extract_thread_id(
                item,
                *in_group,
                &session.profile.uid,
                &session.profile.send2me_id,
            ) {
                if !is_grp
                    && !tid.is_empty()
                    && tid != "0"
                    && tid != clean_my_uid
                    && tid != session.profile.send2me_id
                    && !friend_ids.contains(&tid)
                    && !direct_thread_need_uids.iter().any(|x| x == &tid)
                {
                    direct_thread_need_uids.push(tid);
                }
            }
            let uid_from =
                pick_str(item, &["uidFrom", "fromUid", "fromId", "senderId"]).unwrap_or_default();
            let clean_sender = clean_uid(&uid_from);
            if !clean_sender.is_empty()
                && clean_sender != "0"
                && clean_sender != clean_my_uid
                && clean_sender != session.profile.send2me_id
                && !known_ids.contains(&clean_sender)
                && !direct_thread_need_uids.iter().any(|x| x == &clean_sender)
                && !group_sender_need_uids.iter().any(|x| x == &clean_sender)
            {
                group_sender_need_uids.push(clean_sender);
            }
        }
    }

    let mut extra_profiles: Map<String, Value> = Map::new();
    let mut name_map: HashMap<String, String> = HashMap::new();

    // 1. Resolve ALL direct stranger / unresolved threads first (no `.take(3)` truncation).
    for chunk in direct_thread_need_uids.chunks(50) {
        match get_mini_profiles(session, chunk).await {
            Ok(mp) => {
                for (k, v) in mp {
                    if let Some(name) =
                        pick_str(&v, &["displayName", "zaloName", "name", "display_name"])
                    {
                        name_map.insert(k.clone(), name);
                    }
                    extra_profiles.insert(k, v);
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => {
                eprintln!("[litechat][threads] direct get_mini_profiles failed: {e}");
            }
        }
    }

    // 2. Resolve group message sender names (bounded to avoid excessive startup calls).
    let remaining_group_senders: Vec<String> = group_sender_need_uids
        .into_iter()
        .filter(|uid| !extra_profiles.contains_key(uid))
        .collect();
    for chunk in remaining_group_senders.chunks(50).take(3) {
        match get_mini_profiles(session, chunk).await {
            Ok(mp) => {
                for (k, v) in mp {
                    if let Some(name) =
                        pick_str(&v, &["displayName", "zaloName", "name", "display_name"])
                    {
                        name_map.insert(k.clone(), name);
                    }
                    extra_profiles.insert(k, v);
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => {
                eprintln!("[litechat][threads] group sender get_mini_profiles failed: {e}");
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

    if let Some(ref unread_ids) = unread_all_ids {
        apply_unread_marks_to_threads(&mut threads, unread_ids, &mut remote_read_known);
    }

    if pinned_synced {
        for t in &mut threads {
            if all_pinned_ids.contains(clean_group_id(&t.id)) || all_pinned_ids.contains(&t.id) {
                t.is_pinned = true;
            }
        }
    }

    for t in &mut threads {
        if t.kind == ThreadKind::Direct {
            if let Some(p) = extra_profiles.get(&t.id) {
                if t.title.trim().is_empty() || t.title == t.id || is_raw_numeric_id(&t.title) {
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

    // Apply user-defined aliases AFTER all threads (including strangers) are created & populated.
    if !aliases.is_empty() {
        for t in &mut threads {
            if t.kind == ThreadKind::Direct {
                if let Some(alias) = aliases.get(&t.id) {
                    t.title = alias.clone();
                }
            }
        }
    }

    // Hydrate up to 12 newly discovered non-friend direct threads that still have `last_ts == 0`
    // (e.g., from pinned/unread/archived/hidden/label lists) so they show real previews & times.
    let newly_discovered_set: HashSet<&str> =
        newly_discovered_direct_ids.iter().map(String::as_str).collect();
    let mut hydrate_indices: Vec<usize> = threads
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.kind == ThreadKind::Direct
                && t.last_ts == 0
                && !friend_ids.contains(&t.id)
                && (newly_discovered_set.contains(t.id.as_str())
                    || t.is_pinned
                    || t.unread_count > 0)
        })
        .map(|(idx, _)| idx)
        .collect();
    hydrate_indices.sort_by(|&a, &b| {
        let ta = &threads[a];
        let tb = &threads[b];
        tb.is_pinned
            .cmp(&ta.is_pinned)
            .then_with(|| (tb.unread_count > 0).cmp(&(ta.unread_count > 0)))
            .then_with(|| ta.id.cmp(&tb.id))
    });
    hydrate_indices.truncate(12);
    for idx in hydrate_indices {
        let tid = threads[idx].id.clone();
        match fetch_thread_history_page(session, &tid, ThreadKind::Direct, 1).await {
            Ok(page) => {
                if let Some(last_msg) = page.messages.last() {
                    if last_msg.ts > threads[idx].last_ts {
                        threads[idx].last_ts = last_msg.ts;
                    }
                    let preview = format_thread_preview(
                        &last_msg.text,
                        last_msg.is_outgoing,
                        &last_msg.sender_name,
                        false,
                    );
                    if !preview.is_empty() {
                        threads[idx].last_preview = preview;
                    }
                    if !last_msg.is_outgoing
                        && (threads[idx].title.trim().is_empty()
                            || threads[idx].title == threads[idx].id
                            || is_raw_numeric_id(&threads[idx].title))
                        && !last_msg.sender_name.trim().is_empty()
                        && !is_raw_numeric_id(&last_msg.sender_name)
                    {
                        threads[idx].title = last_msg.sender_name.clone();
                    }
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => {
                eprintln!("[litechat][threads] hydrate history for stranger {tid} failed: {e}");
            }
        }
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

fn strip_redundant_trunk_zero(digits: &str, country_code: &str) -> Option<String> {
    const TRUNK_ZERO_CODES: &[&str] = &[
        "886", "84", "82", "81", "44", "61", "66", "60", "62", "63", "49", "33",
    ];
    if !TRUNK_ZERO_CODES.contains(&country_code) {
        return None;
    }
    let rest = digits.strip_prefix(country_code)?.strip_prefix('0')?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("{country_code}{rest}"))
}

pub fn normalize_phone_number(raw: &str, default_country: &str) -> Result<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')' | '\t' | '\u{00A0}'))
        .collect();
    let (is_international, digits) = if let Some(rest) = cleaned.strip_prefix('+') {
        (true, rest)
    } else if let Some(rest) = cleaned.strip_prefix("00") {
        (true, rest)
    } else {
        (false, cleaned.as_str())
    };
    if digits.is_empty()
        || !digits.chars().all(|c| c.is_ascii_digit())
        || (is_international && digits.starts_with('0'))
    {
        return Err(anyhow::anyhow!("Số điện thoại không hợp lệ"));
    }

    let cc_clean: String = default_country
        .trim()
        .trim_start_matches('+')
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    let cc = if cc_clean.is_empty() {
        "84"
    } else {
        cc_clean.as_str()
    };

    let normalized = if is_international {
        const TRUNK_ZERO_CODES: &[&str] = &[
            "886", "84", "82", "81", "44", "61", "66", "60", "62", "63", "49", "33",
        ];
        TRUNK_ZERO_CODES
            .iter()
            .find_map(|code| strip_redundant_trunk_zero(digits, code))
            .unwrap_or_else(|| digits.to_string())
    } else if let Some(fixed) = strip_redundant_trunk_zero(digits, cc) {
        fixed
    } else if cc == "84" {
        if let Some(fixed_kr) = strip_redundant_trunk_zero(digits, "82")
            .filter(|s| (11..=12).contains(&s.len()))
        {
            fixed_kr
        } else if let Some(rest) = digits.strip_prefix('0') {
            format!("84{rest}")
        } else if (digits.starts_with("84") && digits.len() >= 10)
            || (digits.starts_with("82") && (11..=12).contains(&digits.len()))
        {
            digits.to_string()
        } else if digits.len() == 9 {
            format!("84{digits}")
        } else {
            digits.to_string()
        }
    } else if let Some(rest) = digits.strip_prefix('0') {
        format!("{cc}{rest}")
    } else if digits.starts_with(cc) && digits.len() >= 10 {
        digits.to_string()
    } else if digits.len() <= 10 {
        format!("{cc}{digits}")
    } else {
        digits.to_string()
    };

    let min_len = if is_international { 8 } else { 10 };
    if !(min_len..=15).contains(&normalized.len()) {
        return Err(anyhow::anyhow!(
            "Số điện thoại không hợp lệ (cần 10-15 chữ số kèm mã quốc gia)"
        ));
    }
    Ok(normalized)
}

pub fn normalize_vn_phone(raw: &str) -> Result<String> {
    normalize_phone_number(raw, "84")
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
    if let Some(raw) = item.get("seenUids").or_else(|| item.get("seen_uids")) {
        let parsed_arr: Option<Vec<Value>> = match raw {
            Value::Array(arr) => Some(arr.clone()),
            Value::String(s) if s.trim().starts_with('[') => {
                serde_json::from_str::<Vec<Value>>(s.trim()).ok()
            }
            _ => None,
        };
        if let Some(arr) = parsed_arr {
            let self_seen = arr.iter().any(|v| {
                let s = match v {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                };
                let clean = s.split('_').next().unwrap_or(s.as_str());
                clean == "0" || (!my_uid.is_empty() && !clean.is_empty() && (s == my_uid || clean == my_uid))
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
    let clean_my_uid = clean_uid(my_uid);
    let entries = collect_last_msg_entries(last_msgs);
    for (in_group, item) in &entries {
        let parsed = parse_chat_message(item, None, my_uid, send2me_id, *in_group);
        let (tid, is_grp) = match extract_thread_id(item, *in_group, my_uid, send2me_id) {
            Some(v) => v,
            None => match &parsed {
                Some(msg) => (msg.thread_id.clone(), *in_group),
                None => {
                    let Some(raw_id) = pick_str(item, &["id", "uid"]) else {
                        continue;
                    };
                    let grp = *in_group
                        || is_group_flag(item)
                        || raw_id.starts_with("group_")
                        || raw_id.starts_with('g')
                        || raw_id.starts_with('G');
                    let clean = if grp {
                        clean_group_id(&raw_id).to_string()
                    } else if !send2me_id.is_empty() && raw_id == send2me_id {
                        raw_id
                    } else {
                        clean_uid(&raw_id)
                    };
                    (clean, grp)
                }
            },
        };
        if tid.is_empty()
            || tid == "0"
            || (!is_grp
                && !clean_my_uid.is_empty()
                && tid == clean_my_uid
                && tid != send2me_id)
        {
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
                || (!my_uid.is_empty() && (clean_sender == my_uid || clean_sender == clean_my_uid))
                || (!send2me_id.is_empty() && clean_sender == send2me_id)
        });
        let mut sender_name = parsed
            .as_ref()
            .map(|m| m.sender_name.clone())
            .or_else(|| pick_str(item, &["dName", "displayName", "fromName", "senderName"]))
            .unwrap_or_default();
        if !is_outgoing
            && (sender_name.is_empty() || sender_name == uid_from || is_raw_numeric_id(&sender_name))
        {
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

fn extract_message_poll_dto(msg: &MessageDto) -> Option<PollDto> {
    let raw = msg.attachment_name.as_deref()?.strip_prefix("teza-poll:")?;
    serde_json::from_str::<PollDto>(raw).ok()
}

fn apply_merged_poll_to_message(msg: &mut MessageDto, incoming_poll: &PollDto) {
    let merged = match extract_message_poll_dto(msg) {
        Some(existing) => merge_poll_dto(&existing, incoming_poll),
        None => incoming_poll.clone(),
    };
    if let Ok(poll_json) = serde_json::to_string(&merged) {
        msg.msg_type = "chat.poll".to_string();
        msg.attachment_url = None;
        msg.attachment_name = Some(format!("teza-poll:{poll_json}"));
        let has_real_q = !merged.question.trim().is_empty()
            && merged.question.trim() != "Bình chọn"
            && !is_poll_activity_text(&merged.question);
        if has_real_q
            && (msg.text.trim().is_empty()
                || msg.text.trim() == "[Bình chọn]"
                || msg.text.trim() == "Bình chọn"
                || is_poll_activity_text(&msg.text))
        {
            msg.text = format!("Bình chọn: {}", merged.question);
        }
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
    let mut poll_activity_updates: Vec<(i64, PollDto)> = Vec::new();
    let mut msgs: Vec<MessageDto> = Vec::new();
    for item in &items {
        if pick_str(item, &["msgType", "type"])
            .is_some_and(|t| t.eq_ignore_ascii_case("chat.reaction"))
        {
            parse_reaction_item(item, is_group, my_uid, send2me, &mut reaction_actions);
            continue;
        }
        if let Some((_, activity_poll_id)) =
            extract_poll_activity_update(item, is_group, my_uid, send2me)
        {
            if let Some(dto) = extract_poll_activity_dto(item, activity_poll_id) {
                let ts = item
                    .get("ts")
                    .or_else(|| item.get("cliMsgId"))
                    .or_else(|| item.get("sendTime"))
                    .and_then(value_as_i64)
                    .map(normalize_ts_ms)
                    .unwrap_or(0);
                poll_activity_updates.push((ts, dto));
            }
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

    // Fold any duplicate poll messages sharing `(thread_id, poll_id)` (where `poll_id > 0`) into
    // a single canonical message keeping the earliest `msg_id`/`ts` and merging `PollDto` state.
    let mut folded: Vec<MessageDto> = Vec::with_capacity(msgs.len());
    let mut poll_idx_by_key: HashMap<(String, i64), usize> = HashMap::new();
    for msg in msgs {
        if let Some(incoming_poll) = extract_message_poll_dto(&msg) {
            if incoming_poll.poll_id > 0 {
                let key = (msg.thread_id.clone(), incoming_poll.poll_id);
                if let Some(&idx) = poll_idx_by_key.get(&key) {
                    apply_merged_poll_to_message(&mut folded[idx], &incoming_poll);
                    folded[idx].is_pinned |= msg.is_pinned;
                    continue;
                }
                poll_idx_by_key.insert(key, folded.len());
            }
        }
        folded.push(msg);
    }

    // Apply any vote/update/end activity payloads in chronological order to matching poll cards.
    if !poll_activity_updates.is_empty() && !poll_idx_by_key.is_empty() {
        poll_activity_updates.sort_by_key(|(ts, _)| *ts);
        for (_, dto) in poll_activity_updates {
            if dto.poll_id > 0 {
                let key = (thread_id.to_string(), dto.poll_id);
                if let Some(&idx) = poll_idx_by_key.get(&key) {
                    apply_merged_poll_to_message(&mut folded[idx], &dto);
                }
            }
        }
    }

    Ok(RemoteHistoryPage {
        messages: folded,
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
    find_user_by_phone_with_country(session, raw_phone, "84").await
}

pub async fn find_user_by_phone_with_country(
    session: &Session,
    raw_phone: &str,
    default_country: &str,
) -> Result<UserProfileDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    let phone = normalize_phone_number(raw_phone, default_country)?;
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

fn build_conv_unread_mark_body(
    id_val: &str,
    is_group: bool,
    unread: bool,
    ts_ms: i64,
    imei: &str,
) -> Value {
    let inner = if unread {
        let entry = json!({
            "id": id_val,
            "cliMsgId": ts_ms.to_string(),
            "fromUid": "0",
            "ts": ts_ms,
        });
        json!({
            "convsGroup": if is_group { json!([entry]) } else { json!([]) },
            "convsUser": if is_group { json!([]) } else { json!([entry]) },
            "imei": imei,
        })
    } else {
        let data_entry = json!({ "id": id_val, "ts": ts_ms });
        json!({
            "convsGroup": if is_group { json!([id_val]) } else { json!([]) },
            "convsUser": if is_group { json!([]) } else { json!([id_val]) },
            "convsGroupData": if is_group { json!([data_entry]) } else { json!([]) },
            "convsUserData": if is_group { json!([]) } else { json!([data_entry]) },
        })
    };
    obj(&[("param", json!(inner.to_string()))])
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
    let ts_ms = chrono::Utc::now().timestamp_millis();
    let base_host = session.domains.conversation.trim_end_matches('/');

    // Canonical Zalo endpoint (`/api/conv/addUnreadMark` / `/api/conv/removeUnreadMark`).
    let conv_endpoint = if unread {
        "api/conv/addUnreadMark"
    } else {
        "api/conv/removeUnreadMark"
    };
    let conv_body = build_conv_unread_mark_body(&id_val, is_group, unread, ts_ms, &session.imei);
    let conv_params = session.encoded_params(&conv_body)?;
    let conv_url = format!("{base_host}/{conv_endpoint}?{}", session.common_params());
    let primary_err: Option<anyhow::Error> = match session
        .post_form(&conv_url, format!("params={conv_params}"))
        .await
    {
        Ok(raw) => {
            let dec = unwrap_data_str(session.maybe_decrypt(raw, session.session_key_bytes()));
            ensure_not_expired(&dec)?;
            let code = dec.get("error_code").and_then(value_as_i64).unwrap_or(0);
            if code == 0 {
                return Ok(());
            }
            Some(anyhow::anyhow!("conv unread mark error_code={code}"))
        }
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(e) => Some(e),
    };

    // Legacy fallback (`/api/convers/add-unread-mark` / `/api/convers/remove-unread-mark`).
    let entry = json!({ "id": id_val, "ts": ts_ms.to_string() });
    let legacy_inner = json!({
        "convsGroup": if is_group { json!([entry]) } else { json!([]) },
        "convsUser": if is_group { json!([]) } else { json!([entry]) },
        "imei": session.imei,
    });
    let legacy_body = obj(&[("param", json!(legacy_inner.to_string()))]);
    let legacy_params = session.encoded_params(&legacy_body)?;
    let legacy_endpoint = if unread {
        "add-unread-mark"
    } else {
        "remove-unread-mark"
    };
    let legacy_url = format!(
        "{base_host}/api/convers/{legacy_endpoint}?{}",
        session.common_params()
    );
    match session
        .post_form(&legacy_url, format!("params={legacy_params}"))
        .await
    {
        Ok(raw) => {
            let dec = session.maybe_decrypt(raw, session.session_key_bytes());
            ensure_not_expired(&dec)
        }
        Err(e) if is_expired_err(&e) => Err(e),
        Err(e) => Err(primary_err.unwrap_or(e)),
    }
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

fn build_seen_event_item(
    msg_id: &str,
    cli_msg_id: Option<&str>,
    sender: &str,
    id_val: &str,
    is_group: bool,
    msg_type: &str,
    ts: i64,
) -> Value {
    let normalized_msg_type = {
        let trimmed = msg_type.trim();
        if trimmed.is_empty() {
            "chat.text"
        } else {
            trimmed
        }
    };
    let gmi = msg_id.trim();
    let cmi = cli_msg_id
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "0")
        .unwrap_or(gmi);
    let di = if is_group { id_val } else { "0" };
    let normalized_ts = if ts > 0 { normalize_ts_ms(ts) } else { -1 };
    json!({
        "cmi": cmi,
        "gmi": gmi,
        "si": sender,
        "di": di,
        "mt": normalized_msg_type,
        "st": 0,
        "at": 0,
        "cmd": 0,
        "ts": normalized_ts,
    })
}

fn build_seen_event_msg_infos(item: Value, id_val: &str, is_group: bool) -> Value {
    if is_group {
        json!({ "data": [item], "grid": id_val })
    } else {
        json!({ "data": [item], "senderId": id_val })
    }
}

pub async fn send_seen_event(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg_id: &str,
    sender_id: &str,
) -> Result<()> {
    send_seen_event_with_meta(
        session,
        thread_id,
        kind,
        msg_id,
        None,
        sender_id,
        "chat.text",
        0,
    )
    .await
}

pub async fn send_seen_event_with_type(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg_id: &str,
    sender_id: &str,
    msg_type: &str,
) -> Result<()> {
    send_seen_event_with_meta(session, thread_id, kind, msg_id, None, sender_id, msg_type, 0).await
}

#[allow(clippy::too_many_arguments)]
pub async fn send_seen_event_with_meta(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    msg_id: &str,
    cli_msg_id: Option<&str>,
    sender_id: &str,
    msg_type: &str,
    ts: i64,
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
    let item = build_seen_event_item(msg_id, cli_msg_id, sender, &id_val, is_group, msg_type, ts);
    let msg_infos = build_seen_event_msg_infos(item, &id_val, is_group);
    let (base_host, path) = if is_group {
        let group_host = if session.domains.group.is_empty() {
            crate::endpoints::service("group").unwrap_or("https://tt-group-wpa.chat.zalo.me")
        } else {
            session.domains.group.trim_end_matches('/')
        };
        (group_host, "api/group/seenv2")
    } else {
        let chat_host = if session.domains.chat.is_empty()
            || session.domains.chat.contains("-msg-chat.")
            || session.domains.chat.contains("tt-chat2-wpa")
        {
            crate::endpoints::service("chat").unwrap_or("https://tt-chat3-wpa.chat.zalo.me")
        } else {
            session.domains.chat.trim_end_matches('/')
        };
        (chat_host, "api/message/seenv2")
    };
    let body = if is_group {
        obj(&[
            ("msgInfos", json!(msg_infos.to_string())),
            ("imei", json!(session.imei)),
        ])
    } else {
        obj(&[("msgInfos", json!(msg_infos.to_string()))])
    };
    let params = session.encoded_params(&body)?;
    let url = format!("{base_host}/{path}?{}&nretry=0", session.common_params());
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
    let mut first_proto_err: Option<anyhow::Error> = None;
    let mut last_err: Option<anyhow::Error> = None;
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
                if dec.is_object() {
                    let code = dec.get("error_code").and_then(value_as_i64).unwrap_or(0);
                    if code == 0 {
                        return Ok(dec);
                    }
                    let msg = pick_str(&dec, &["error_message", "message"])
                        .unwrap_or_else(|| format!("Mã lỗi {code}"));
                    if first_proto_err.is_none() {
                        first_proto_err = Some(anyhow::anyhow!("{msg}"));
                    }
                    last_err = Some(anyhow::anyhow!("{msg}"));
                } else {
                    last_err = Some(anyhow::anyhow!(
                        "Phản hồi không hợp lệ từ máy chủ cho {clean_path}"
                    ));
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    Err(first_proto_err
        .or(last_err)
        .unwrap_or_else(|| anyhow::anyhow!("Không có máy chủ khả dụng cho {clean_path}")))
}

async fn get_across_hosts(
    session: &Session,
    hosts: &[String],
    path: &str,
    body: &Value,
) -> Result<Value> {
    let params = session.encoded_params(body)?;
    let clean_path = path.trim_start_matches('/');
    let mut first_proto_err: Option<anyhow::Error> = None;
    let mut last_err: Option<anyhow::Error> = None;
    for host in hosts {
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
                    let code = dec.get("error_code").and_then(value_as_i64).unwrap_or(0);
                    if code == 0 {
                        return Ok(dec);
                    }
                    let msg = pick_str(&dec, &["error_message", "message"])
                        .unwrap_or_else(|| format!("Mã lỗi {code}"));
                    if first_proto_err.is_none() {
                        first_proto_err = Some(anyhow::anyhow!("{msg}"));
                    }
                    last_err = Some(anyhow::anyhow!("{msg}"));
                } else {
                    last_err = Some(anyhow::anyhow!(
                        "Phản hồi không hợp lệ từ máy chủ cho {clean_path}"
                    ));
                }
            }
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    Err(first_proto_err
        .or(last_err)
        .unwrap_or_else(|| anyhow::anyhow!("Không có máy chủ khả dụng cho {clean_path}")))
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

/// Minimum recommended lead time (ms) enforced at the Tauri IPC boundary for new reminders.
pub const REMINDER_MIN_LEAD_MS: i64 = 30_000;

/// Normalizes and validates a reminder start timestamp, ensuring it is positive and strictly
/// after `now_ms + min_lead_ms`.
pub fn validate_reminder_start_time(
    start_time_ms: i64,
    now_ms: i64,
    min_lead_ms: i64,
) -> Result<i64> {
    let normalized_start = normalize_ts_ms(start_time_ms);
    if normalized_start <= 0 {
        anyhow::bail!("Thời gian nhắc hẹn không hợp lệ");
    }
    let normalized_now = normalize_ts_ms(now_ms);
    let lead = min_lead_ms.max(0);
    let min_allowed = normalized_now.saturating_add(lead);
    if normalized_start <= min_allowed {
        if lead >= 1_000 {
            let secs = lead / 1_000;
            anyhow::bail!("Thời gian nhắc hẹn phải cách hiện tại ít nhất {secs} giây");
        }
        anyhow::bail!("Thời gian nhắc hẹn phải nằm trong tương lai");
    }
    Ok(normalized_start)
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
    let req_now_ms = chrono::Utc::now().timestamp_millis();
    let normalized_start = validate_reminder_start_time(start_time_ms, req_now_ms, 0)?;
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

/// All 5 poll endpoints live on `zpwServiceMap.group[0]` (`tt-group-wpa.chat.zalo.me`),
/// keeping `groupboard` / `board` only as secondary fallbacks.
fn poll_service_hosts(session: &Session) -> Vec<String> {
    let mut hosts = Vec::new();
    for h in session.domains.candidates("group") {
        push_unique_host(&mut hosts, &h);
    }
    if let Some(svc) = crate::endpoints::service("group") {
        push_unique_host(&mut hosts, svc);
    }
    for h in session.domains.candidates("groupboard") {
        push_unique_host(&mut hosts, &h);
    }
    push_unique_host(&mut hosts, crate::endpoints::GROUP_BOARD);
    push_unique_host(&mut hosts, crate::endpoints::BOARD);
    hosts
}

#[allow(clippy::too_many_arguments)]
fn build_create_poll_body(
    clean_gid: &str,
    clean_q: &str,
    clean_opts: &[String],
    expired_time_ms: i64,
    allow_multi: bool,
    allow_add_option: bool,
    is_anonymous: bool,
    imei: &str,
) -> Value {
    let normalized_expiry = if expired_time_ms > 0 {
        normalize_ts_ms(expired_time_ms)
    } else {
        0
    };
    obj(&[
        ("group_id", json!(clean_gid)),
        ("question", json!(clean_q)),
        ("options", json!(clean_opts)),
        ("expired_time", json!(normalized_expiry)),
        ("pinAct", json!(false)),
        ("allow_multi_choices", json!(allow_multi)),
        ("allow_add_new_option", json!(allow_add_option)),
        ("is_hide_vote_preview", json!(false)),
        ("is_anonymous", json!(is_anonymous)),
        ("poll_type", json!(0)),
        ("src", json!(1)),
        ("imei", json!(imei)),
    ])
}

fn build_poll_detail_body(poll_id: i64, imei: &str) -> Value {
    obj(&[("poll_id", json!(poll_id)), ("imei", json!(imei))])
}

fn build_vote_poll_body(poll_id: i64, option_ids: &[i64], imei: &str) -> Value {
    obj(&[
        ("poll_id", json!(poll_id)),
        ("option_ids", json!(option_ids)),
        ("imei", json!(imei)),
    ])
}

fn build_add_poll_options_body(
    poll_id: i64,
    new_options: &[String],
    voted_option_ids: &[i64],
) -> Result<Value> {
    let clean_opts: Vec<String> = new_options
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if clean_opts.is_empty() {
        anyhow::bail!("Vui lòng nhập ít nhất 1 lựa chọn mới");
    }
    let opts_json: Vec<Value> = clean_opts
        .iter()
        .map(|opt| json!({ "content": opt, "voted": true }))
        .collect();
    let encoded_opts = serde_json::to_string(&opts_json)?;
    Ok(obj(&[
        ("poll_id", json!(poll_id)),
        ("new_options", json!(encoded_opts)),
        ("voted_option_ids", json!(voted_option_ids)),
    ]))
}

fn build_lock_poll_body(poll_id: i64, imei: &str) -> Value {
    obj(&[("poll_id", json!(poll_id)), ("imei", json!(imei))])
}

/// Merge an incoming (possibly partial, e.g. `/api/poll/vote` options-only or sparse
/// message payload with empty options) `PollDto` into an existing `PollDto` without
/// wiping question/flags metadata or re-opening a locked poll.
pub fn merge_poll_dto(existing: &PollDto, incoming: &PollDto) -> PollDto {
    let incoming_has_real_question = !incoming.question.trim().is_empty()
        && incoming.question.trim() != "Bình chọn"
        && !is_poll_activity_text(&incoming.question);
    let existing_has_real_question = !existing.question.trim().is_empty()
        && existing.question.trim() != "Bình chọn"
        && !is_poll_activity_text(&existing.question);
    let is_partial = (!incoming_has_real_question && existing_has_real_question)
        || (incoming.options.is_empty() && !existing.options.is_empty());

    let poll_id = if incoming.poll_id > 0 {
        incoming.poll_id
    } else {
        existing.poll_id
    };
    let question = if incoming_has_real_question {
        incoming.question.clone()
    } else if existing_has_real_question {
        existing.question.clone()
    } else if !incoming.question.trim().is_empty() && !is_poll_activity_text(&incoming.question) {
        incoming.question.clone()
    } else {
        "Bình chọn".to_string()
    };
    let options = if !incoming.options.is_empty() {
        incoming.options.clone()
    } else {
        existing.options.clone()
    };
    let expired_time = if incoming.expired_time > 0 {
        incoming.expired_time
    } else {
        existing.expired_time
    };
    // Closing/locking a poll is irreversible on Zalo; history re-sync of an older creation
    // payload with `closed: false` must never re-open an already locked poll.
    let closed = existing.closed || incoming.closed;

    if is_partial {
        PollDto {
            poll_id,
            question,
            options,
            allow_multi: existing.allow_multi || incoming.allow_multi,
            allow_add_option: existing.allow_add_option || incoming.allow_add_option,
            is_anonymous: existing.is_anonymous || incoming.is_anonymous,
            expired_time,
            closed,
        }
    } else {
        PollDto {
            poll_id,
            question,
            options,
            allow_multi: incoming.allow_multi,
            allow_add_option: incoming.allow_add_option,
            is_anonymous: incoming.is_anonymous,
            expired_time,
            closed,
        }
    }
}

pub fn parse_poll_dto(resp: &Value, fallback_poll_id: i64) -> Option<PollDto> {
    if let Some(data_owned) = resp.get("data").and_then(unwrap_json_field) {
        if let Some(inner_data) = data_owned.get("data").and_then(unwrap_json_field) {
            if let Some(dto) = parse_poll_value(&inner_data, fallback_poll_id) {
                return Some(dto);
            }
        }
        if let Some(dto) = parse_poll_value(&data_owned, fallback_poll_id) {
            return Some(dto);
        }
    }
    parse_poll_value(resp, fallback_poll_id)
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
    if clean_gid.is_empty() {
        anyhow::bail!("Mã nhóm không hợp lệ");
    }
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

    let body = build_create_poll_body(
        clean_gid,
        clean_q,
        &clean_opts,
        expired_time_ms,
        allow_multi,
        allow_add_option,
        is_anonymous,
        &session.imei,
    );
    let hosts = poll_service_hosts(session);

    let resp = post_form_across_hosts(session, &hosts, "api/poll/create", &body).await?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let normalized_expiry = if expired_time_ms > 0 {
        normalize_ts_ms(expired_time_ms)
    } else {
        0
    };
    let build_local_fallback = |poll_id: i64| PollDto {
        poll_id,
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
        expired_time: normalized_expiry,
        closed: false,
    };

    let poll = match parse_poll_dto(&resp, 0) {
        Some(mut parsed) if parsed.poll_id > 0 => {
            if parsed.options.is_empty() {
                match get_poll_detail(session, parsed.poll_id).await {
                    Ok(detail) if !detail.options.is_empty() => detail,
                    Err(e) if is_expired_err(&e) => return Err(e),
                    _ => {
                        let base = build_local_fallback(parsed.poll_id);
                        merge_poll_dto(&base, &parsed)
                    }
                }
            } else {
                if parsed.question == "Bình chọn" && clean_q != "Bình chọn" {
                    parsed.question = clean_q.to_string();
                }
                parsed
            }
        }
        _ => build_local_fallback(now_ms),
    };

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
    if poll_id <= 0 {
        anyhow::bail!("Mã bình chọn không hợp lệ");
    }
    let body = build_poll_detail_body(poll_id, &session.imei);
    let hosts = poll_service_hosts(session);
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
    if poll_id <= 0 {
        anyhow::bail!("Mã bình chọn không hợp lệ");
    }
    let body = build_vote_poll_body(poll_id, option_ids, &session.imei);
    let hosts = poll_service_hosts(session);
    let resp = match get_across_hosts(session, &hosts, "api/poll/vote", &body).await {
        Ok(v) => v,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(get_err) => match post_form_across_hosts(session, &hosts, "api/poll/vote", &body).await
        {
            Ok(v) => v,
            Err(e) if is_expired_err(&e) => return Err(e),
            Err(_) => return Err(get_err),
        },
    };
    let voted_partial = parse_poll_dto(&resp, poll_id);
    // `/api/poll/vote` only returns `{ options: [...] }` without question/flags metadata.
    // Always fetch fresh detail so metadata is never wiped.
    match get_poll_detail(session, poll_id).await {
        Ok(detail) => {
            if detail.options.is_empty() {
                if let Some(partial) = voted_partial.as_ref().filter(|p| !p.options.is_empty()) {
                    return Ok(merge_poll_dto(&detail, partial));
                }
            }
            Ok(detail)
        }
        Err(e) if is_expired_err(&e) => Err(e),
        Err(detail_err) => voted_partial
            .filter(|p| !p.options.is_empty())
            .ok_or(detail_err),
    }
}

pub async fn add_poll_options(
    session: &Session,
    poll_id: i64,
    new_options: &[String],
    voted_option_ids: &[i64],
) -> Result<PollDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    if poll_id <= 0 {
        anyhow::bail!("Mã bình chọn không hợp lệ");
    }
    let body = build_add_poll_options_body(poll_id, new_options, voted_option_ids)?;
    let hosts = poll_service_hosts(session);
    let resp = match get_across_hosts(session, &hosts, "api/poll/option/add", &body).await {
        Ok(v) => v,
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(get_err) => {
            match post_form_across_hosts(session, &hosts, "api/poll/option/add", &body).await {
                Ok(v) => v,
                Err(e) if is_expired_err(&e) => return Err(e),
                Err(_) => return Err(get_err),
            }
        }
    };
    let added_partial = parse_poll_dto(&resp, poll_id);
    match get_poll_detail(session, poll_id).await {
        Ok(detail) => {
            if detail.options.is_empty() {
                if let Some(partial) = added_partial.as_ref().filter(|p| !p.options.is_empty()) {
                    return Ok(merge_poll_dto(&detail, partial));
                }
            }
            Ok(detail)
        }
        Err(e) if is_expired_err(&e) => Err(e),
        Err(detail_err) => added_partial
            .filter(|p| !p.options.is_empty())
            .ok_or(detail_err),
    }
}

pub async fn lock_poll(session: &Session, poll_id: i64) -> Result<PollDto> {
    if !session.is_authenticated() {
        anyhow::bail!("Chưa đăng nhập");
    }
    if poll_id <= 0 {
        anyhow::bail!("Mã bình chọn không hợp lệ");
    }
    let body = build_lock_poll_body(poll_id, &session.imei);
    let hosts = poll_service_hosts(session);
    let resp = post_form_across_hosts(session, &hosts, "api/poll/end", &body).await?;
    match get_poll_detail(session, poll_id).await {
        Ok(mut detail) => {
            detail.closed = true;
            Ok(detail)
        }
        Err(e) if is_expired_err(&e) => Err(e),
        Err(detail_err) => {
            if let Some(mut partial) = parse_poll_dto(&resp, poll_id) {
                partial.closed = true;
                Ok(partial)
            } else {
                Err(detail_err)
            }
        }
    }
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
        let photo = build_seen_event_item(
            "m-10",
            Some("c-10"),
            "u-1",
            "u-1",
            false,
            "chat.photo",
            1_700_000_010_000,
        );
        assert_eq!(photo["mt"], "chat.photo");
        assert_eq!(photo["cmi"], "c-10");
        assert_eq!(photo["gmi"], "m-10");
        assert_eq!(photo["si"], "u-1");
        assert_eq!(photo["di"], "0");
        assert_eq!(photo["ts"], 1_700_000_010_000_i64);

        let dm_infos = build_seen_event_msg_infos(photo, "u-1", false);
        assert_eq!(dm_infos["senderId"], "u-1");
        assert!(dm_infos.get("grid").is_none());

        let grp = build_seen_event_item("m-11", None, "u-2", "9001", true, "   ", 0);
        assert_eq!(grp["mt"], "chat.text");
        assert_eq!(grp["cmi"], "m-11");
        assert_eq!(grp["gmi"], "m-11");
        assert_eq!(grp["si"], "u-2");
        assert_eq!(grp["di"], "9001");
        assert_eq!(grp["ts"], -1);

        let grp_infos = build_seen_event_msg_infos(grp, "9001", true);
        assert_eq!(grp_infos["grid"], "9001");
        assert!(grp_infos.get("senderId").is_none());
    }

    #[test]
    fn build_conv_unread_mark_body_matches_zalo_add_and_remove_schemas() {
        let add_dm = build_conv_unread_mark_body("111", false, true, 1_700_000_000_000, "imei-1");
        let add_inner: Value = serde_json::from_str(add_dm["param"].as_str().unwrap()).unwrap();
        assert_eq!(add_inner["convsUser"][0]["id"], "111");
        assert_eq!(add_inner["convsUser"][0]["fromUid"], "0");
        assert_eq!(add_inner["convsUser"][0]["cliMsgId"], "1700000000000");
        assert_eq!(add_inner["convsGroup"].as_array().unwrap().len(), 0);
        assert_eq!(add_inner["imei"], "imei-1");

        let rm_grp = build_conv_unread_mark_body("888", true, false, 1_700_000_000_123, "imei-1");
        let rm_inner: Value = serde_json::from_str(rm_grp["param"].as_str().unwrap()).unwrap();
        assert_eq!(rm_inner["convsGroup"], json!(["888"]));
        assert_eq!(rm_inner["convsGroupData"][0]["id"], "888");
        assert_eq!(rm_inner["convsGroupData"][0]["ts"], 1_700_000_000_123_i64);
        assert_eq!(rm_inner["convsUser"].as_array().unwrap().len(), 0);
        assert_eq!(rm_inner["convsUserData"].as_array().unwrap().len(), 0);

        let seen_zero = json!({ "seenUids": "[\"0\"]" });
        assert_eq!(extract_last_msg_read_state(&seen_zero, false, "456"), (0, true));
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

    #[test]
    fn poll_service_hosts_prioritizes_group_before_groupboard_fallbacks() {
        let mut session = Session::new().unwrap();
        session.domains.group = "https://tt-group-wpa.chat.zalo.me".into();
        session.domains.service_map_candidates.insert(
            "groupboard".into(),
            vec!["https://groupboard-wpa.chat.zalo.me".into()],
        );
        let hosts = poll_service_hosts(&session);
        assert!(!hosts.is_empty());
        assert_eq!(hosts[0], "https://tt-group-wpa.chat.zalo.me");
        let grp_idx = hosts
            .iter()
            .position(|h| h == "https://tt-group-wpa.chat.zalo.me")
            .unwrap();
        let gb_idx = hosts
            .iter()
            .position(|h| h == crate::endpoints::GROUP_BOARD)
            .unwrap();
        let board_idx = hosts
            .iter()
            .position(|h| h == crate::endpoints::BOARD)
            .unwrap();
        assert!(grp_idx < gb_idx);
        assert!(gb_idx < board_idx);
    }

    #[test]
    fn poll_payload_builders_match_zalo_wire_protocol() {
        let create_body = build_create_poll_body(
            "888111",
            "Trưa nay ăn gì?",
            &["Phở".into(), "Bún".into()],
            0,
            true,
            true,
            false,
            "imei-poll",
        );
        assert_eq!(create_body["group_id"], "888111");
        assert_eq!(create_body["question"], "Trưa nay ăn gì?");
        assert_eq!(create_body["options"], json!(["Phở", "Bún"]));
        assert_eq!(create_body["allow_multi_choices"], true);
        assert_eq!(create_body["allow_add_new_option"], true);
        assert_eq!(create_body["is_anonymous"], false);
        assert_eq!(create_body["pinAct"], false);
        assert_eq!(create_body["is_hide_vote_preview"], false);
        assert_eq!(create_body["poll_type"], 0);
        assert_eq!(create_body["src"], 1);
        assert_eq!(create_body["imei"], "imei-poll");

        // vote_poll MUST send plural "option_ids" (never singular "option_id")
        let vote_body = build_vote_poll_body(98765, &[101, 102], "imei-poll");
        assert_eq!(vote_body["poll_id"], 98765);
        assert_eq!(vote_body["option_ids"], json!([101, 102]));
        assert!(vote_body.get("option_id").is_none());
        assert_eq!(vote_body["imei"], "imei-poll");

        // add_poll_options encodes new_options as JSON string of { content, voted: true }
        let add_body = build_add_poll_options_body(
            98765,
            &["  Cơm tấm  ".into(), "".into(), "Bánh mì".into()],
            &[101],
        )
        .unwrap();
        assert_eq!(add_body["poll_id"], 98765);
        assert_eq!(add_body["voted_option_ids"], json!([101]));
        let parsed_new_opts: Value =
            serde_json::from_str(add_body["new_options"].as_str().unwrap()).unwrap();
        assert_eq!(
            parsed_new_opts,
            json!([
                { "content": "Cơm tấm", "voted": true },
                { "content": "Bánh mì", "voted": true }
            ])
        );
        assert!(build_add_poll_options_body(98765, &["   ".into()], &[]).is_err());

        // lock_poll sends { poll_id, imei }
        let lock_body = build_lock_poll_body(98765, "imei-poll");
        assert_eq!(lock_body["poll_id"], 98765);
        assert_eq!(lock_body["imei"], "imei-poll");
    }

    #[test]
    fn parse_poll_dto_unwraps_nested_data_and_merge_preserves_metadata() {
        let nested_resp = json!({
            "error_code": 0,
            "data": {
                "data": {
                    "poll_id": 774411_i64,
                    "question": "Chốt lịch team building",
                    "allow_multi_choices": true,
                    "allow_add_new_option": true,
                    "is_anonymous": false,
                    "expired_time": 0,
                    "options": [
                        { "option_id": 501, "content": "Thứ 7", "votes": 3, "voted": true, "voters": ["u1", "u2", "u3"] },
                        { "option_id": 502, "content": "Chủ nhật", "votes": 1, "voted": false, "voters": ["u4"] }
                    ]
                }
            }
        });
        let full = parse_poll_dto(&nested_resp, 0).unwrap();
        assert_eq!(full.poll_id, 774411);
        assert_eq!(full.question, "Chốt lịch team building");
        assert!(full.allow_multi);
        assert!(full.allow_add_option);
        assert_eq!(full.options[0].id, 501);
        assert_eq!(full.options[0].votes, 3);

        // Simulate partial `/api/poll/vote` response containing ONLY options (no question/flags)
        let vote_only_resp = json!({
            "error_code": 0,
            "data": {
                "options": [
                    { "option_id": 501, "content": "Thứ 7", "votes": 4, "voted": true },
                    { "option_id": 502, "content": "Chủ nhật", "votes": 1, "voted": false }
                ]
            }
        });
        let partial = parse_poll_dto(&vote_only_resp, 774411).unwrap();
        assert_eq!(partial.question, "Bình chọn");
        let merged = merge_poll_dto(&full, &partial);
        assert_eq!(merged.poll_id, 774411);
        assert_eq!(merged.question, "Chốt lịch team building");
        assert!(merged.allow_multi);
        assert!(merged.allow_add_option);
        assert_eq!(merged.options[0].votes, 4);
    }

    #[test]
    fn merge_poll_dto_preserves_flags_on_sparse_payload_and_never_reopens_locked_poll() {
        let existing = PollDto {
            poll_id: 99001,
            question: "Chọn địa điểm họp".into(),
            options: vec![
                PollOptionDto {
                    id: 10,
                    content: "Phòng A".into(),
                    votes: 3,
                    voted: true,
                },
                PollOptionDto {
                    id: 11,
                    content: "Phòng B".into(),
                    votes: 1,
                    voted: false,
                },
            ],
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: true,
            expired_time: 1_900_000_000_000,
            closed: false,
        };

        // (a) Sparse incoming payload with real question + empty options must preserve flags & options
        let sparse_incoming = PollDto {
            poll_id: 99001,
            question: "Chọn địa điểm họp".into(),
            options: Vec::new(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let merged_sparse = merge_poll_dto(&existing, &sparse_incoming);
        assert_eq!(merged_sparse.poll_id, 99001);
        assert_eq!(merged_sparse.question, "Chọn địa điểm họp");
        assert_eq!(merged_sparse.options.len(), 2);
        assert_eq!(merged_sparse.options[0].votes, 3);
        assert!(merged_sparse.allow_multi);
        assert!(merged_sparse.allow_add_option);
        assert!(merged_sparse.is_anonymous);
        assert_eq!(merged_sparse.expired_time, 1_900_000_000_000);
        assert!(!merged_sparse.closed);

        // (b) History re-sync of an old creation payload (`closed: false`, with options)
        // must NEVER re-open an already locked poll (`closed: true`)
        let mut locked_existing = existing.clone();
        locked_existing.closed = true;
        let stale_history_creation = PollDto {
            poll_id: 99001,
            question: "Chọn địa điểm họp".into(),
            options: existing.options.clone(),
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: true,
            expired_time: 1_900_000_000_000,
            closed: false,
        };
        let merged_locked = merge_poll_dto(&locked_existing, &stale_history_creation);
        assert!(merged_locked.closed);
    }

    #[test]
    fn normalize_phone_number_supports_korean_international_and_vn_formats() {
        // Explicit international prefixes preserve country code and strip redundant trunk 0
        assert_eq!(
            normalize_phone_number("+82 10-1234-5678", "84").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("+82-010-1234-5678", "84").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("+84 912 345 678", "82").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("+84 0912 345 678", "84").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("+1 (415) 555-2671", "84").unwrap(),
            "14155552671"
        );

        // Korean local number with default_country == "82"
        assert_eq!(
            normalize_phone_number("010-1234-5678", "82").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("010-1234-5678", "+82").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("10-1234-5678", "82").unwrap(),
            "821012345678"
        );

        // VN default ("84") preserves existing VN behavior and does not mangle 82... numbers
        assert_eq!(
            normalize_phone_number("0912 345 678", "84").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("912345678", "84").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("821012345678", "84").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("8201012345678", "84").unwrap(),
            "821012345678"
        );

        // International `00` prefix behaves identically to `+` (strips `00`, never prepends default cc)
        assert_eq!(
            normalize_phone_number("0084912345678", "84").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("0084 0912 345 678", "84").unwrap(),
            "84912345678"
        );
        assert_eq!(
            normalize_phone_number("0082 10-1234-5678", "84").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("0082-010-1234-5678", "84").unwrap(),
            "821012345678"
        );
        assert_eq!(
            normalize_phone_number("001 (415) 555-2671", "82").unwrap(),
            "14155552671"
        );

        // Invalid inputs rejected
        assert!(normalize_phone_number("abc123", "82").is_err());
        assert!(normalize_phone_number("0123", "82").is_err());
        assert!(normalize_phone_number("00", "84").is_err());
        assert!(normalize_phone_number("00084912345678", "84").is_err());
        assert!(normalize_phone_number("+084912345678", "84").is_err());
    }

    #[test]
    fn validate_reminder_start_time_rejects_past_and_enforces_lead_time() {
        let now_ms = 1_760_000_000_000_i64;
        assert!(validate_reminder_start_time(0, now_ms, 0).is_err());
        assert!(validate_reminder_start_time(-1_000, now_ms, 0).is_err());
        assert!(validate_reminder_start_time(now_ms - 1, now_ms, 0).is_err());
        assert!(validate_reminder_start_time(now_ms, now_ms, 0).is_err());
        assert_eq!(
            validate_reminder_start_time(now_ms + 1, now_ms, 0).unwrap(),
            now_ms + 1
        );

        // With standard 30s lead time
        assert!(validate_reminder_start_time(now_ms + 15_000, now_ms, REMINDER_MIN_LEAD_MS).is_err());
        assert!(
            validate_reminder_start_time(now_ms + REMINDER_MIN_LEAD_MS, now_ms, REMINDER_MIN_LEAD_MS)
                .is_err()
        );
        assert_eq!(
            validate_reminder_start_time(
                now_ms + REMINDER_MIN_LEAD_MS + 1,
                now_ms,
                REMINDER_MIN_LEAD_MS
            )
            .unwrap(),
            now_ms + REMINDER_MIN_LEAD_MS + 1
        );

        // Normalizes 10-digit Unix seconds
        let future_secs = (now_ms / 1_000) + 60;
        assert_eq!(
            validate_reminder_start_time(future_secs, now_ms, REMINDER_MIN_LEAD_MS).unwrap(),
            future_secs * 1_000
        );
    }

    #[test]
    fn parse_remote_history_page_folds_duplicate_polls_and_vote_notices_preserving_question() {
        // Remote history often arrives newest-first and mixes poll creation, duplicate poll frames,
        // vote activity notices, and end-poll notices for the same `poll_id`.
        let resp = json!({
            "error_code": 0,
            "data": {
                "hasMore": 0,
                "groupMsgs": [
                    {
                        "msgId": "m-poll-4",
                        "uidFrom": "100",
                        "grid": "888111",
                        "msgType": "group.action",
                        "ts": 1_760_000_004_000_i64,
                        "content": {
                            "action": "end_poll",
                            "title": "Bình chọn đã kết thúc",
                            "params": "{\"topic\":{\"id\":555001}}"
                        }
                    },
                    {
                        "msgId": "m-poll-3",
                        "uidFrom": "200",
                        "grid": "888111",
                        "msgType": "group.action",
                        "ts": 1_760_000_003_000_i64,
                        "content": {
                            "action": "vote_poll",
                            "title": "Bình đã tham gia bình chọn",
                            "params": "{\"poll_id\":555001,\"options\":[{\"option_id\":1,\"content\":\"Phở\",\"votes\":3,\"voted\":true},{\"option_id\":2,\"content\":\"Bún\",\"votes\":1,\"voted\":false}]}"
                        }
                    },
                    {
                        "msgId": "m-poll-2",
                        "uidFrom": "300",
                        "grid": "888111",
                        "msgType": "group.poll",
                        "ts": 1_760_000_002_000_i64,
                        "content": {
                            "poll_id": 555001_i64,
                            "title": "An đã bình chọn",
                            "options": [
                                { "option_id": 1, "content": "Phở", "votes": 2, "voted": false },
                                { "option_id": 2, "content": "Bún", "votes": 1, "voted": false }
                            ]
                        }
                    },
                    {
                        "msgId": "m-poll-1",
                        "uidFrom": "100",
                        "grid": "888111",
                        "msgType": "group.poll",
                        "ts": 1_760_000_001_000_i64,
                        "content": {
                            "poll_id": 555001_i64,
                            "question": "Trưa nay ăn gì?",
                            "allow_multi_choices": true,
                            "allow_add_new_option": true,
                            "options": [
                                { "option_id": 1, "content": "Phở", "votes": 0, "voted": false },
                                { "option_id": 2, "content": "Bún", "votes": 0, "voted": false }
                            ]
                        }
                    },
                    {
                        "msgId": "m-text-1",
                        "uidFrom": "200",
                        "grid": "888111",
                        "msgType": "chat.text",
                        "ts": 1_760_000_005_000_i64,
                        "content": "OK chốt nhé"
                    }
                ]
            }
        });

        let page = parse_remote_history_page(&resp, "888111", "100", "", true, None).unwrap();
        assert_eq!(page.messages.len(), 2);
        let poll_msg = &page.messages[0];
        assert_eq!(poll_msg.msg_id, "m-poll-1");
        assert_eq!(poll_msg.ts, 1_760_000_001_000);
        assert_eq!(poll_msg.text, "Bình chọn: Trưa nay ăn gì?");
        let dto = extract_message_poll_dto(poll_msg).unwrap();
        assert_eq!(dto.poll_id, 555001);
        assert_eq!(dto.question, "Trưa nay ăn gì?");
        assert!(dto.allow_multi);
        assert!(dto.allow_add_option);
        assert_eq!(dto.options[0].votes, 3);
        assert!(dto.options[0].voted);
        assert!(dto.closed);

        assert_eq!(page.messages[1].msg_id, "m-text-1");

        // `merge_poll_dto` must never overwrite a real question with a vote-activity string
        let bad_incoming = PollDto {
            poll_id: 555001,
            question: "An đã tham gia bình chọn".into(),
            options: dto.options.clone(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let merged = merge_poll_dto(&dto, &bad_incoming);
        assert_eq!(merged.question, "Trưa nay ăn gì?");
        assert!(merged.allow_multi);
        assert!(merged.closed);
    }

    #[test]
    fn last_msgs_unwraps_nested_data_and_stranger_buckets_and_normalizes_uid_suffixes() {
        let mut threads: Vec<ThreadDto> = Vec::new();
        let last_msgs = json!({
            "error_code": 0,
            "data": {
                "data": {
                    "strangerMsgs": "[{\"uidFrom\":\"777001_0\",\"idTo\":\"100_0\",\"dName\":\"Khách lạ A\",\"content\":\"Xin chào shop\",\"ts\":1760000010000}]",
                    "otherMsgs": [
                        {
                            "uidFrom": "100_0",
                            "idTo": "777002_0",
                            "content": "Mình đã gửi báo giá",
                            "ts": 1_760_000_020_000_i64
                        }
                    ],
                    "conversations": [
                        {
                            "threadId": "777003_0",
                            "unread": 2,
                            "lastMsg": {
                                "uidFrom": "777003_0",
                                "idTo": "100_0",
                                "dName": "Khách lạ C",
                                "content": "Còn hàng không bạn?",
                                "ts": 1_760_000_030_000_i64
                            }
                        }
                    ]
                }
            }
        });

        let mut names = HashMap::new();
        names.insert("777002".to_string(), "Khách lạ B".to_string());
        let read_known = apply_last_msgs_to_threads_with_read_state(
            &mut threads,
            &last_msgs,
            "100",
            "cloud_100",
            &names,
        );

        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["777003", "777002", "777001"]);
        assert_eq!(threads[0].title, "Khách lạ C");
        assert_eq!(threads[0].unread_count, 2);
        assert!(read_known.contains("777003"));
        assert_eq!(threads[1].title, "Khách lạ B");
        assert_eq!(threads[1].last_preview, "Bạn: Mình đã gửi báo giá");
        assert_eq!(threads[2].title, "Khách lạ A");
        assert!(!threads.iter().any(|t| t.id == "100"));
    }

    #[test]
    fn non_friend_discovery_materializes_unread_pinned_alias_label_archived_and_hidden_threads() {
        let mut threads = vec![
            ThreadDto {
                id: "100001".into(),
                title: "Bạn Thân".into(),
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
                id: "888001".into(),
                title: "Nhóm A".into(),
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

        let unread_resp = json!({
            "error_code": 0,
            "data": { "convsUser": ["200001", "100001"], "convsGroup": ["g888999"] }
        });
        let (unread_users, unread_groups) = parse_unread_marks_split(&unread_resp).unwrap();
        assert!(unread_users.contains("200001"));
        assert!(unread_groups.contains("888999"));

        let pinned_resp = json!({
            "error_code": 0,
            "data": { "conversations": ["200002", "g888001", { "id": "888002", "isGroup": 1 }] }
        });
        let (all_pinned, direct_pinned) = parse_pinned_thread_ids_split(&pinned_resp);
        assert!(all_pinned.contains("888001"));
        assert!(all_pinned.contains("888002"));
        assert!(direct_pinned.contains("200002"));
        assert!(!direct_pinned.contains("888001"));
        assert!(!direct_pinned.contains("888002"));

        let label_resp = json!({
            "error_code": 0,
            "data": {
                "labelData": "[{\"id\":1,\"text\":\"VIP\",\"conversations\":[\"200003\",\"g888777\"]}]"
            }
        });
        let label_direct = parse_remote_label_direct_uids(&label_resp);
        assert_eq!(label_direct, vec!["200003"]);

        let archived_resp = json!({
            "error_code": 0,
            "data": {
                "items": [
                    { "id": "200004_0", "type": 0 },
                    { "id": "888333", "type": 1 }
                ]
            }
        });
        let archived_direct = parse_direct_thread_ids_from_conv_list(&archived_resp);
        assert_eq!(archived_direct, vec!["200004"]);

        let hidden_resp = json!({
            "error_code": 0,
            "data": {
                "threads": [
                    { "thread_id": "200005", "is_group": 0 },
                    { "thread_id": "888444", "is_group": 1 }
                ]
            }
        });
        let hidden_direct = parse_direct_thread_ids_from_conv_list(&hidden_resp);
        assert_eq!(hidden_direct, vec!["200005"]);

        let mut aliases = HashMap::new();
        aliases.insert("200006".to_string(), "Anh Shipper".to_string());
        aliases.insert("200004".to_string(), "Khách Lưu Trữ".to_string());

        let created = materialize_discovered_direct_threads(
            &mut threads,
            "999000",
            "cloud_999000",
            &unread_users,
            &direct_pinned,
            &aliases,
            &label_direct,
            &archived_direct,
            &hidden_direct,
        );

        assert_eq!(
            created,
            vec!["200001", "200002", "200004", "200005", "200003", "200006"]
        );
        let t_unread = threads.iter().find(|t| t.id == "200001").unwrap();
        assert_eq!(t_unread.unread_count, 1);
        assert_eq!(t_unread.kind, ThreadKind::Direct);
        let t_pinned = threads.iter().find(|t| t.id == "200002").unwrap();
        assert!(t_pinned.is_pinned);
        let t_archived = threads.iter().find(|t| t.id == "200004").unwrap();
        assert_eq!(t_archived.title, "Khách Lưu Trữ");
        assert!(!t_archived.is_archived);
        let t_alias = threads.iter().find(|t| t.id == "200006").unwrap();
        assert_eq!(t_alias.title, "Anh Shipper");
        // No group IDs leaked as direct threads
        assert!(!threads.iter().any(|t| t.id == "888999" || t.id == "888333" || t.id == "888444"));
    }

    #[test]
    fn mini_profiles_partial_miss_computes_missing_and_merges_v2_fallback() {
        let requested = vec![
            "1001".to_string(),
            "1002_0".to_string(),
            "1003".to_string(),
        ];
        // Primary `/api/social/group/members` returned 1001, an empty stub for 1002, and missed 1003
        let mut merged = Map::new();
        merged.insert(
            "1001".into(),
            json!({ "displayName": "Member One", "avatar": "https://s160-ava-talk.zadn.vn/1.jpg" }),
        );
        merged.insert("1002".into(), json!({}));

        let still_missing = compute_missing_profile_uids(&requested, &merged);
        assert_eq!(still_missing, vec!["1002", "1003"]);

        // Fallback `/api/social/friend/getprofiles/v2` resolves 1002 and 1003
        let mut v2_result = Map::new();
        v2_result.insert(
            "1002_0".into(),
            json!({ "displayName": "Stranger Two", "avatar": "https://s160-ava-talk.zadn.vn/2.jpg" }),
        );
        v2_result.insert(
            "1003".into(),
            json!({ "displayName": "Stranger Three", "avatar": "" }),
        );
        merge_mini_profile_maps(&mut merged, v2_result);

        assert_eq!(merged.len(), 3);
        assert_eq!(merged["1001"]["displayName"], "Member One");
        assert_eq!(merged["1002"]["displayName"], "Stranger Two");
        assert_eq!(merged["1003"]["displayName"], "Stranger Three");
        assert!(compute_missing_profile_uids(&requested, &merged).is_empty());
    }
}
