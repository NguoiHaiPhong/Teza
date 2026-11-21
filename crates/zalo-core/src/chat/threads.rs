//! Thread list synchronization (`getfriends`, `getlg/v4`, `getmg-v2`, `get-last-msgs`) and history loading.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::chat::parser::{
    clean_group_id, extract_content_text, extract_thread_id, format_thread_preview,
    is_raw_numeric_id, normalize_avatar_url, normalize_media_url, normalize_ts_ms,
    parse_chat_message, parse_friend_thread, pick_str,
};
use crate::chat::{
    ChatFolderDto, MessageDto, ThreadDto, ThreadKind, ThreadMemberDto, UserProfileDto,
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

    if let Some(ref last_msgs) = last_msgs_opt {
        apply_last_msgs_to_threads_with_names(
            &mut threads,
            last_msgs,
            &session.profile.uid,
            &session.profile.send2me_id,
            &name_map,
        );
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
        is_self,
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
            let attachment_url = pick_str(&params_obj, &["href", "url", "thumb"])
                .or_else(|| pick_str(&item, &["href", "url", "thumb"]))
                .map(|u| normalize_media_url(&u));
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
            let msg_type = pick_str(&params_obj, &["msgType", "msg_type"])
                .or_else(|| pick_str(&item, &["msgType", "msg_type"]))
                .unwrap_or_else(|| "chat.text".to_string());
            let is_outgoing = !self_uid.is_empty() && sender_id == self_uid;
            MessageDto {
                msg_id,
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
    let mut msgs: Vec<MessageDto> = items
        .iter()
        .filter_map(|item| parse_chat_message(item, Some(thread_id), my_uid, send2me, is_group))
        .filter(|msg| {
            exclude_msg_id
                .map(|ex| msg.msg_id != ex)
                .unwrap_or(true)
        })
        .collect();
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
    let body = obj(&[
        ("toid", json!(toid)),
        ("duration", json!(-1)),
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
}
