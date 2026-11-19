//! Thread list synchronization (`getfriends`, `getlg/v4`, `getmg-v2`, `get-last-msgs`) and history loading.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::chat::parser::{
    clean_group_id, extract_content_text, extract_thread_id, format_thread_preview,
    is_raw_numeric_id, normalize_avatar_url, normalize_media_url, normalize_ts_ms,
    parse_chat_message, parse_friend_thread, pick_str,
};
use crate::chat::{MessageDto, ThreadDto, ThreadKind, ThreadMemberDto, UserProfileDto};
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
        groups.push(ThreadDto {
            id: gid,
            title,
            avatar,
            kind: ThreadKind::Group,
            last_preview: String::new(),
            last_ts: 0,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
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
pub async fn sync_threads(session: &Session) -> Result<Vec<ThreadDto>> {
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

    match fetch_pinned_thread_ids(session).await {
        Ok(pinned_ids) => {
            for t in &mut threads {
                if pinned_ids.contains(clean_group_id(&t.id)) || pinned_ids.contains(&t.id) {
                    t.is_pinned = true;
                }
            }
        }
        Err(e) if is_expired_err(&e) => return Err(e),
        Err(_) => {}
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

    if let Some(ref last_msgs) = last_msgs_opt {
        apply_last_msgs_to_threads_with_names(
            &mut threads,
            last_msgs,
            &session.profile.uid,
            &session.profile.send2me_id,
            &name_map,
        );
    } else {
        sort_threads_by_recent(&mut threads);
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

    Ok(threads)
}

pub fn sort_threads_by_recent(threads: &mut [ThreadDto]) {
    threads.sort_by(|a, b| {
        (b.unread_count > 0)
            .cmp(&(a.unread_count > 0))
            .then_with(|| b.is_pinned.cmp(&a.is_pinned))
            .then_with(|| b.last_ts.cmp(&a.last_ts))
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub fn apply_last_msgs_to_threads(
    threads: &mut Vec<ThreadDto>,
    last_msgs: &Value,
    my_uid: &str,
    send2me_id: &str,
) {
    apply_last_msgs_to_threads_with_names(threads, last_msgs, my_uid, send2me_id, &HashMap::new());
}

pub fn apply_last_msgs_to_threads_with_names(
    threads: &mut Vec<ThreadDto>,
    last_msgs: &Value,
    my_uid: &str,
    send2me_id: &str,
    extra_names: &HashMap<String, String>,
) {
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
                let unread = if is_outgoing {
                    0
                } else {
                    item.get("unread")
                        .or_else(|| item.get("unreadCount"))
                        .or_else(|| item.get("totalUnread"))
                        .and_then(Value::as_u64)
                        .map(|n| n as u32)
                        .unwrap_or_else(|| {
                            if item.get("isUnread").and_then(Value::as_bool) == Some(true) {
                                1
                            } else {
                                0
                            }
                        })
                };
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
                            existing.unread = if is_outgoing { 0 } else { existing.unread.max(unread) };
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
                                fallback_title,
                            },
                        );
                    }
                }
            }
        }
    }

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
            } else if info.unread > t.unread_count {
                t.unread_count = info.unread;
            }
        }
    }

    for (tid, info) in previews {
        if seen_ids.contains(&tid) || (info.preview.is_empty() && info.ts <= 0) {
            continue;
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
        });
    }

    sort_threads_by_recent(threads);
}

pub async fn fetch_thread_history(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    count: u32,
) -> Result<Vec<MessageDto>> {
    if !session.is_authenticated() {
        return Ok(Vec::new());
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

    ensure_not_expired(&resp)?;
    let data_node = resp.get("data").unwrap_or(&resp);
    let items: Vec<&Value> = if let Some(arr) = data_node.as_array() {
        arr.iter().collect()
    } else if let Some(arr) = data_node
        .get("msgs")
        .or_else(|| data_node.get("list"))
        .and_then(Value::as_array)
    {
        arr.iter().collect()
    } else {
        Vec::new()
    };

    let mut msgs: Vec<MessageDto> = items
        .into_iter()
        .filter_map(|item| {
            parse_chat_message(
                item,
                Some(thread_id),
                my_uid,
                send2me,
                kind == ThreadKind::Group,
            )
        })
        .collect();
    msgs.sort_by_key(|m| m.ts);
    Ok(msgs)
}

/// Fetch older messages strictly before `before_msg_id` via `/api/cm/getoldv2`.
pub async fn fetch_older_thread_history(
    session: &Session,
    thread_id: &str,
    kind: ThreadKind,
    before_msg_id: &str,
    count: u32,
) -> Result<Vec<MessageDto>> {
    if !session.is_authenticated()
        || before_msg_id.is_empty()
        || before_msg_id.starts_with("local-")
        || before_msg_id.starts_with("out_")
    {
        return Ok(Vec::new());
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
    ensure_not_expired(&resp)?;

    let data_node = resp.get("data").unwrap_or(&resp);
    let items: Vec<&Value> = if let Some(arr) = data_node.as_array() {
        arr.iter().collect()
    } else if let Some(arr) = data_node
        .get("msgs")
        .or_else(|| data_node.get("list"))
        .and_then(Value::as_array)
    {
        arr.iter().collect()
    } else {
        Vec::new()
    };

    let mut msgs: Vec<MessageDto> = items
        .into_iter()
        .filter_map(|item| {
            parse_chat_message(
                item,
                Some(thread_id),
                my_uid,
                send2me,
                kind == ThreadKind::Group,
            )
        })
        .filter(|msg| msg.msg_id != before_msg_id)
        .collect();
    msgs.sort_by_key(|m| m.ts);
    Ok(msgs)
}

pub async fn fetch_group_details(
    session: &Session,
    thread_id: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> Result<(Option<u32>, Vec<ThreadMemberDto>)> {
    if !session.is_authenticated() {
        return Ok((None, Vec::new()));
    }
    let clean = clean_group_id(thread_id).to_string();
    let (info_map, _) = fetch_group_profiles(session, std::slice::from_ref(&clean)).await?;
    let Some(info) = info_map.get(&clean).or_else(|| info_map.get(thread_id)) else {
        return Ok((None, Vec::new()));
    };

    let creator = pick_str(info, &["creatorId"]).unwrap_or_default();
    let mut admin_set: HashSet<String> = HashSet::new();
    if let Some(admins) = info.get("adminIds").and_then(Value::as_array) {
        for a in admins {
            if let Some(s) = a
                .as_str()
                .map(str::to_string)
                .or_else(|| a.as_u64().map(|n| n.to_string()))
            {
                admin_set.insert(s);
            }
        }
    }

    let mut ordered_ids: Vec<String> = Vec::new();
    if !creator.is_empty() {
        ordered_ids.push(creator.clone());
    }
    for aid in &admin_set {
        if !ordered_ids.contains(aid) {
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
                let uid = s.split('_').next().unwrap_or("").trim();
                if !uid.is_empty() && !ordered_ids.iter().any(|x| x == uid) {
                    ordered_ids.push(uid.to_string());
                }
            }
        }
    }

    let total_members = info
        .get("totalMember")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .or_else(|| (!ordered_ids.is_empty()).then_some(ordered_ids.len() as u32));

    let sample_ids: Vec<String> = ordered_ids.into_iter().take(100).collect();
    let mut profiles = Map::new();
    for chunk in sample_ids.chunks(50) {
        if let Ok(mp) = get_mini_profiles(session, chunk).await {
            for (k, v) in mp {
                profiles.insert(k, v);
            }
        }
    }

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

            if !session.profile.uid.is_empty() && uid == session.profile.uid {
                if name.is_empty() && !session.profile.display_name.is_empty() {
                    name = format!("{} (Bạn)", session.profile.display_name);
                }
                if avatar.is_empty() && !session.profile.avatar.is_empty() {
                    avatar = normalize_avatar_url(&session.profile.avatar);
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

            let role = if uid == creator {
                "Trưởng nhóm".to_string()
            } else if admin_set.contains(&uid) {
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

    Ok((total_members, members))
}

pub async fn fetch_user_profile(
    session: &Session,
    user_id: &str,
    known_profiles: &HashMap<String, (String, String)>,
) -> Result<UserProfileDto> {
    let clean_uid = user_id.trim().split('_').next().unwrap_or(user_id.trim()).to_string();
    let is_self = !session.profile.uid.is_empty() && clean_uid == session.profile.uid;

    let mut remote_val: Option<Value> = None;
    if session.is_authenticated() && !clean_uid.is_empty() {
        let mut profile_hosts = session.domains.candidates("profile");
        for extra in ["https://profile-wpa.zaloapp.com", "https://tt-profile-wpa.chat.zalo.me"] {
            if !profile_hosts.iter().any(|h| h == extra) {
                profile_hosts.push(extra.to_string());
            }
        }
        let body = obj(&[
            ("friend_pversion_map", json!([format!("{clean_uid}_0")])),
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
                    if let Some(found) = map.get(&clean_uid) {
                        remote_val = Some(found.clone());
                        break;
                    }
                }
            }
        }
        if remote_val.is_none() {
            if let Ok(mp) = get_mini_profiles(session, std::slice::from_ref(&clean_uid)).await {
                if let Some(found) = mp.get(&clean_uid) {
                    remote_val = Some(found.clone());
                }
            }
        }
    }

    let rv = remote_val.as_ref();
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
        .and_then(|v| v.get("isFr").and_then(Value::as_i64))
        .map(|n| n == 1);

    if is_self {
        if display_name.is_empty() && !session.profile.display_name.is_empty() {
            display_name = session.profile.display_name.clone();
        }
        if avatar.is_empty() && !session.profile.avatar.is_empty() {
            avatar = normalize_avatar_url(&session.profile.avatar);
        }
    }
    if let Some((local_name, local_ava)) = known_profiles.get(&clean_uid) {
        if display_name.is_empty() && !local_name.is_empty() && local_name != &clean_uid {
            display_name = local_name.clone();
        }
        if avatar.is_empty() && !local_ava.is_empty() {
            avatar = local_ava.clone();
        }
    }
    if display_name.is_empty() {
        display_name = clean_uid.clone();
    }

    Ok(UserProfileDto {
        uid: clean_uid,
        display_name,
        zalo_name,
        avatar,
        cover,
        status,
        gender,
        sdob,
        phone_number,
        is_friend,
        is_self,
    })
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

        // Unread thread moves above pinned and read threads
        threads[3].unread_count = 2;
        sort_threads_by_recent(&mut threads);
        let ids_after_unread: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids_after_unread, vec!["888111", "cloud-1", "friend-a", "stranger-b"]);
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
}
