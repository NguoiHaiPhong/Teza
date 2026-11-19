//! Tauri v2 CoreBridge connecting `zalo-core` to the Telegram-inspired React UI.

mod file_dialog;

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::InvokeBody;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use zalo_core::chat::messaging;
use zalo_core::chat::parser::{
    format_thread_preview, is_raw_numeric_id, parse_ws_event, pick_str, WsParsedAction,
};
use zalo_core::chat::threads;
use zalo_core::chat::{
    FolderFilter, MessageDto, ThreadDetailsDto, ThreadDto, ThreadKind, UserProfileDto,
};
use zalo_core::data_dir::StoragePaths;
use zalo_core::qr::{self, QrTicket};
use zalo_core::session::{AccountProfile, Session, SessionError, SessionStatus};
use zalo_core::socket::{self, WsSignal};
use zalo_core::store::MessageStore;
use zalo_core::translate::{
    translate_batch, TranslateConfig, TranslateConfigSummary, TranslateConfigUpdate,
    TranslateInputItem, TranslateProvider, TranslationResult,
};

pub struct AppState {
    pub paths: StoragePaths,
    pub session: Arc<Mutex<Session>>,
    pub store: Arc<Mutex<MessageStore>>,
    pub ws_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub qr_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub active_thread_id: Arc<Mutex<Option<String>>>,
    pub translate_cfg: Arc<Mutex<TranslateConfig>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AuthStatusDto {
    NeedsQr,
    Expired { reason: String },
    Authenticated { profile: AccountProfile },
}

#[derive(Debug, Clone, Serialize)]
pub struct QrStartDto {
    pub token_id: String,
    pub qr_svg: String,
    pub qr_timeout: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesPageDto {
    pub messages: Vec<MessageDto>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentMetaDto {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub is_image: bool,
}

fn expired_code(err: &anyhow::Error) -> Option<i64> {
    match err.downcast_ref::<SessionError>() {
        Some(SessionError::Expired { code }) => Some(*code),
        _ => None,
    }
}

async fn clear_expired_session(
    app: &AppHandle,
    sess_arc: &Arc<Mutex<Session>>,
    ws_task_arc: Option<&Arc<Mutex<Option<JoinHandle<()>>>>>,
    paths: &StoragePaths,
    reason: String,
    emit_event: bool,
) {
    if let Some(ws_arc) = ws_task_arc {
        if let Some(handle) = ws_arc.lock().await.take() {
            handle.abort();
        }
    }
    {
        let mut sess = sess_arc.lock().await;
        if let Ok(fresh) = Session::new() {
            *sess = fresh;
        } else {
            sess.zpw_sek = None;
            sess.secret_key = None;
        }
    }
    let session_path = paths.session_file();
    if session_path.exists() {
        let _ = std::fs::remove_file(&session_path);
    }
    if emit_event {
        let _ = app.emit("core://auth-expired", reason);
    }
}

async fn ensure_ws_worker(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    ws_task_arc: Arc<Mutex<Option<JoinHandle<()>>>>,
    active_thread_arc: Arc<Mutex<Option<String>>>,
    paths: StoragePaths,
) {
    let already_running = {
        let guard = ws_task_arc.lock().await;
        guard.as_ref().is_some_and(|h| !h.is_finished())
    };
    if !already_running {
        restart_ws_worker(
            app,
            sess_arc,
            store_arc,
            ws_task_arc,
            active_thread_arc,
            paths,
        )
        .await;
    }
}

async fn restart_ws_worker(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    ws_task_arc: Arc<Mutex<Option<JoinHandle<()>>>>,
    active_thread_arc: Arc<Mutex<Option<String>>>,
    paths: StoragePaths,
) {
    let mut guard = ws_task_arc.lock().await;
    if let Some(handle) = guard.take() {
        handle.abort();
    }
    let sess_clone = {
        let sess = sess_arc.lock().await;
        if !sess.is_authenticated() {
            return;
        }
        sess.clone_shallow()
    };

    let my_uid = sess_clone.profile.uid.clone();
    let send2me_id = sess_clone.profile.send2me_id.clone();
    let sess_arc_for_ws = sess_arc.clone();

    let handle = tokio::spawn(async move {
        let mut rx = socket::spawn(sess_clone.clone_shallow());
        let mut ws_name_cache: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        while let Some(sig) = rx.recv().await {
            match sig {
                WsSignal::Status(status) => {
                    let _ = app.emit("core://connection-status", status);
                }
                WsSignal::Expired { code, message } => {
                    let reason = format!(
                        "Phiên đăng nhập đã hết hạn (mã {code}: {message}). Vui lòng quét lại mã QR."
                    );
                    clear_expired_session(
                        &app,
                        &sess_arc_for_ws,
                        None,
                        &paths,
                        reason,
                        true,
                    )
                    .await;
                    break;
                }
                WsSignal::Event(ev) => {
                    if !ev.is_chat_ish() && !ev.is_control_ish() {
                        continue;
                    }
                    let mut actions = parse_ws_event(&ev, &my_uid, &send2me_id);
                    if actions.is_empty() {
                        continue;
                    }
                    let missing_uids: Vec<String> = {
                        let known = store_arc
                            .lock()
                            .await
                            .resolve_known_titles()
                            .unwrap_or_default();
                        for (k, v) in known {
                            ws_name_cache.entry(k).or_insert(v);
                        }
                        let mut out = Vec::new();
                        for act in &actions {
                            if let WsParsedAction::Message(msg) = act {
                                if !msg.is_outgoing
                                    && !msg.sender_id.is_empty()
                                    && (msg.sender_name == msg.sender_id
                                        || is_raw_numeric_id(&msg.sender_name))
                                    && !ws_name_cache.contains_key(&msg.sender_id)
                                    && !out.iter().any(|x| x == &msg.sender_id)
                                {
                                    out.push(msg.sender_id.clone());
                                }
                            }
                        }
                        out
                    };
                    if !missing_uids.is_empty() {
                        if let Ok(profiles) = threads::get_mini_profiles(&sess_clone, &missing_uids).await
                        {
                            for (uid, p) in profiles {
                                if let Some(name) = pick_str(
                                    &p,
                                    &["displayName", "zaloName", "name", "display_name"],
                                ) {
                                    ws_name_cache.insert(uid, name);
                                }
                            }
                        }
                    }

                    let active_id = active_thread_arc.lock().await.clone();
                    let mut store = store_arc.lock().await;
                    let mut threads_touched = false;
                    for act in &mut actions {
                        if let WsParsedAction::Message(msg) = act {
                            if !msg.is_outgoing
                                && (msg.sender_name == msg.sender_id
                                    || is_raw_numeric_id(&msg.sender_name))
                            {
                                if let Some(name) = ws_name_cache.get(&msg.sender_id) {
                                    msg.sender_name = name.clone();
                                }
                            }
                        }
                    }
                    for act in actions {
                        match act {
                            WsParsedAction::Message(msg) => {
                                let is_group = store
                                    .get_thread_kind(&msg.thread_id)
                                    .ok()
                                    .flatten()
                                    == Some(ThreadKind::Group);
                                let _ = store.upsert_message(&msg);
                                let preview = format_thread_preview(
                                    &msg.text,
                                    msg.is_outgoing,
                                    &msg.sender_name,
                                    is_group,
                                );
                                let inc_unread = !msg.is_outgoing
                                    && active_id.as_deref() != Some(msg.thread_id.as_str());
                                let clear_unread = msg.is_outgoing
                                    || active_id.as_deref() == Some(msg.thread_id.as_str());
                                let _ = store.update_thread_preview_with_unread(
                                    &msg.thread_id,
                                    &preview,
                                    msg.ts,
                                    inc_unread,
                                    clear_unread,
                                );
                                threads_touched = true;
                                let _ = app.emit("core://message-new", &*msg);
                            }
                            WsParsedAction::Read {
                                thread_id,
                                is_unread,
                            } => {
                                let _ = store.set_thread_unread(&thread_id, if is_unread { 1 } else { 0 });
                                threads_touched = true;
                            }
                            WsParsedAction::Recalled { thread_id, msg_id } => {
                                if let Ok(Some(updated)) = store.mark_recalled(&thread_id, &msg_id) {
                                    let _ = app.emit("core://message-updated", &updated);
                                }
                            }
                            WsParsedAction::Reaction {
                                thread_id,
                                msg_id,
                                emoji,
                                me,
                                removed,
                            } => {
                                if let Ok(Some(updated)) = store.apply_reaction(
                                    &thread_id,
                                    &msg_id,
                                    emoji.as_deref(),
                                    me,
                                    removed,
                                ) {
                                    let _ = app.emit("core://message-updated", &updated);
                                }
                            }
                        }
                    }
                    if threads_touched {
                        let _ = app.emit("core://threads-updated", ());
                    }
                }
            }
        }
    });

    *guard = Some(handle);
}

#[tauri::command]
async fn auth_get_status(app: AppHandle, state: State<'_, AppState>) -> Result<AuthStatusDto, String> {
    let (authed, mut profile, mut sess_snapshot) = {
        let sess = state.session.lock().await;
        (sess.is_authenticated(), sess.profile.clone(), sess.clone_shallow())
    };
    if !authed {
        return Ok(AuthStatusDto::NeedsQr);
    }

    match sess_snapshot.verify_and_refresh_profile().await {
        SessionStatus::Valid => {
            if sess_snapshot.profile.uid != profile.uid
                || sess_snapshot.profile.display_name != profile.display_name
                || sess_snapshot.profile.avatar != profile.avatar
            {
                profile = sess_snapshot.profile.clone();
                let mut sess = state.session.lock().await;
                sess.profile = profile.clone();
                let _ = sess.save_to_file(&state.paths.session_file());
            }
            ensure_ws_worker(
                app,
                state.session.clone(),
                state.store.clone(),
                state.ws_task.clone(),
                state.active_thread_id.clone(),
                state.paths.clone(),
            )
            .await;
            Ok(AuthStatusDto::Authenticated { profile })
        }
        SessionStatus::Unreachable => {
            ensure_ws_worker(
                app,
                state.session.clone(),
                state.store.clone(),
                state.ws_task.clone(),
                state.active_thread_id.clone(),
                state.paths.clone(),
            )
            .await;
            Ok(AuthStatusDto::Authenticated { profile })
        }
        SessionStatus::Expired { code } => {
            let reason = format!(
                "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
            );
            clear_expired_session(
                &app,
                &state.session,
                Some(&state.ws_task),
                &state.paths,
                reason.clone(),
                false,
            )
            .await;
            Ok(AuthStatusDto::Expired { reason })
        }
    }
}

#[tauri::command]
async fn auth_start_qr(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<QrStartDto, String> {
    if let Some(prev) = state.qr_task.lock().await.take() {
        prev.abort();
    }

    let mut sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let ticket: QrTicket = qr::request_qr(&sess_snapshot, "vi")
        .await
        .map_err(|e| format!("Không tạo được mã QR: {e}"))?;

    let dto = QrStartDto {
        token_id: ticket.token_id.clone(),
        qr_svg: ticket.qr_svg.clone(),
        qr_timeout: ticket.qr_timeout,
    };

    let sess_arc = state.session.clone();
    let store_arc = state.store.clone();
    let ws_task_arc = state.ws_task.clone();
    let active_thread_arc = state.active_thread_id.clone();
    let paths = state.paths.clone();

    let handle = tokio::spawn(async move {
        match qr::wait_for_scan(&sess_snapshot, &ticket).await {
            Ok(scanned_name) => {
                let _ = app.emit("core://qr-scanned", scanned_name);
                let _ = app.emit(
                    "core://auth-stage",
                    "Đã quét mã — vui lòng bấm Xác nhận trên điện thoại...",
                );
                if let Err(e) = qr::wait_for_confirm(&mut sess_snapshot, &ticket).await {
                    let _ = app.emit("core://auth-error", e.to_string());
                    return;
                }
                let _ = app.emit(
                    "core://auth-stage",
                    "Đã xác nhận! Đang hoàn tất phiên đăng nhập...",
                );
                if let Err(e) = sess_snapshot.save_to_file(&paths.session_file()) {
                    let _ = app.emit("core://auth-error", format!("Lỗi lưu phiên: {e}"));
                    return;
                }
                let profile = sess_snapshot.profile.clone();
                {
                    let mut guard = sess_arc.lock().await;
                    *guard = sess_snapshot;
                }

                if !profile.uid.is_empty() {
                    let db_path = paths.history_db(Some(&profile.uid));
                    if let Ok(new_store) = MessageStore::open(&db_path) {
                        let mut guard = store_arc.lock().await;
                        *guard = new_store;
                    }
                }

                let _ = app.emit("core://auth-authenticated", &profile);
                restart_ws_worker(
                    app.clone(),
                    sess_arc,
                    store_arc,
                    ws_task_arc,
                    active_thread_arc,
                    paths,
                )
                .await;
            }
            Err(e) => {
                let _ = app.emit("core://auth-error", e.to_string());
            }
        }
    });

    *state.qr_task.lock().await = Some(handle);
    Ok(dto)
}

#[tauri::command]
async fn auth_logout(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(handle) = state.qr_task.lock().await.take() {
        handle.abort();
    }
    if let Some(handle) = state.ws_task.lock().await.take() {
        handle.abort();
    }
    *state.active_thread_id.lock().await = None;
    {
        let mut sess = state.session.lock().await;
        *sess = Session::new().map_err(|e| e.to_string())?;
    }
    let path = state.paths.session_file();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Không xóa được session.json: {e}"))?;
    }
    let fallback_db = state.paths.history_db(None);
    if let Ok(anon_store) = MessageStore::open(&fallback_db) {
        let mut store = state.store.lock().await;
        *store = anon_store;
    }
    Ok(())
}

#[tauri::command]
async fn threads_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    folder: FolderFilter,
) -> Result<Vec<ThreadDto>, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        sess.clone_shallow()
    };
    let remote = match threads::sync_threads(&sess_snapshot).await {
        Ok(list) => list,
        Err(e) => {
            if let Some(code) = expired_code(&e) {
                let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
                clear_expired_session(&app, &state.session, Some(&state.ws_task), &state.paths, reason.clone(), true).await;
                return Err(reason);
            }
            return Err(format!("Đồng bộ danh bạ lỗi: {e}"));
        }
    };
    let store = state.store.lock().await;
    for t in &remote {
        let _ = store.upsert_thread(t);
    }
    store.list_threads(folder).map_err(|e| e.to_string())
}

#[tauri::command]
async fn threads_list(
    state: State<'_, AppState>,
    folder: FolderFilter,
) -> Result<Vec<ThreadDto>, String> {
    let store = state.store.lock().await;
    store.list_threads(folder).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_mark_read(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    *state.active_thread_id.lock().await = Some(thread_id.clone());
    let store = state.store.lock().await;
    store.mark_thread_read(&thread_id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_mark_unread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    {
        let mut active = state.active_thread_id.lock().await;
        if active.as_deref() == Some(thread_id.as_str()) {
            *active = None;
        }
    }
    let store = state.store.lock().await;
    store.set_thread_unread(&thread_id, 1).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_toggle_pin(
    state: State<'_, AppState>,
    thread_id: String,
    pinned: bool,
) -> Result<(), String> {
    let store = state.store.lock().await;
    store.set_thread_pinned(&thread_id, pinned).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_toggle_mute(
    state: State<'_, AppState>,
    thread_id: String,
    muted: bool,
) -> Result<(), String> {
    let store = state.store.lock().await;
    store.set_thread_muted(&thread_id, muted).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_get_details(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDetailsDto, String> {
    let (kind, media_items, file_items, known_profiles) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let (media, files) = store
            .list_thread_media(&thread_id, 60)
            .unwrap_or_default();
        let known = if kind == ThreadKind::Group {
            store.resolve_known_profiles().unwrap_or_default()
        } else {
            Default::default()
        };
        (kind, media, files, known)
    };

    let mut total_members = None;
    let mut members = Vec::new();
    if kind == ThreadKind::Group {
        let sess_snapshot = {
            let sess = state.session.lock().await;
            sess.clone_shallow()
        };
        if sess_snapshot.is_authenticated() {
            if let Ok((tot, list)) =
                threads::fetch_group_details(&sess_snapshot, &thread_id, &known_profiles).await
            {
                total_members = tot;
                members = list;
            }
        }
    }

    Ok(ThreadDetailsDto {
        thread_id,
        total_members,
        members,
        media_items,
        file_items,
    })
}

#[tauri::command]
async fn user_get_profile(
    state: State<'_, AppState>,
    user_id: String,
) -> Result<UserProfileDto, String> {
    let known_profiles = {
        let store = state.store.lock().await;
        store.resolve_known_profiles().unwrap_or_default()
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    threads::fetch_user_profile(&sess_snapshot, &user_id, &known_profiles)
        .await
        .map_err(|e| format!("Không tra được thông tin tài khoản: {e}"))
}

#[tauri::command]
async fn messages_load(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    before_ts: Option<i64>,
    before_msg_id: Option<String>,
    limit: Option<usize>,
    sync_remote: Option<bool>,
) -> Result<MessagesPageDto, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let mut cached = {
        let store = state.store.lock().await;
        store
            .load_messages(&thread_id, before_ts, limit + 1)
            .map_err(|e| e.to_string())?
    };

    let sess_snapshot = {
        let sess = state.session.lock().await;
        if sess.is_authenticated() {
            Some(sess.clone_shallow())
        } else {
            None
        }
    };

    if let Some(sess) = sess_snapshot {
        let kind = {
            let store = state.store.lock().await;
            store
                .get_thread_kind(&thread_id)
                .ok()
                .flatten()
                .unwrap_or(ThreadKind::Direct)
        };

        if before_ts.is_none() && (cached.is_empty() || sync_remote == Some(true)) {
            match threads::fetch_thread_history(&sess, &thread_id, kind, limit as u32).await {
                Ok(mut remote_msgs) if !remote_msgs.is_empty() => {
                    let mut known_titles = {
                        let store = state.store.lock().await;
                        store.resolve_known_titles().unwrap_or_default()
                    };
                    let unresolved_senders: Vec<String> = remote_msgs
                        .iter()
                        .filter(|m| {
                            !m.is_outgoing
                                && !m.sender_id.is_empty()
                                && (m.sender_name == m.sender_id
                                    || is_raw_numeric_id(&m.sender_name))
                                && !known_titles.contains_key(&m.sender_id)
                        })
                        .map(|m| m.sender_id.clone())
                        .collect::<std::collections::HashSet<_>>()
                        .into_iter()
                        .take(50)
                        .collect();
                    if !unresolved_senders.is_empty() {
                        if let Ok(mp) = threads::get_mini_profiles(&sess, &unresolved_senders).await {
                            for (uid, p) in mp {
                                if let Some(name) = pick_str(
                                    &p,
                                    &["displayName", "zaloName", "name", "display_name"],
                                ) {
                                    known_titles.insert(uid, name);
                                }
                            }
                        }
                    }

                    let mut store = state.store.lock().await;
                    for m in &mut remote_msgs {
                        if !m.is_outgoing
                            && (m.sender_name == m.sender_id || is_raw_numeric_id(&m.sender_name))
                        {
                            if let Some(name) = known_titles.get(&m.sender_id) {
                                m.sender_name = name.clone();
                            }
                        }
                        let _ = store.upsert_message(m);
                    }
                    if let Some(newest) = remote_msgs.iter().max_by_key(|m| m.ts) {
                        let raw_text = if newest.text.is_empty() {
                            newest
                                .attachment_name
                                .as_deref()
                                .map(|n| format!("[Tệp: {n}]"))
                                .unwrap_or_else(|| format!("[{}]", newest.msg_type))
                        } else {
                            newest.text.clone()
                        };
                        let preview = format_thread_preview(
                            &raw_text,
                            newest.is_outgoing,
                            &newest.sender_name,
                            kind == ThreadKind::Group,
                        );
                        let _ = store.update_thread_preview_with_unread(
                            &thread_id,
                            &preview,
                            newest.ts,
                            false,
                            newest.is_outgoing,
                        );
                        let _ = app.emit("core://threads-updated", ());
                    }
                    cached = store
                        .load_messages(&thread_id, None, limit + 1)
                        .map_err(|e| e.to_string())?;
                }
                Err(e) => {
                    if let Some(code) = expired_code(&e) {
                        let reason = format!(
                            "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                        );
                        clear_expired_session(
                            &app,
                            &state.session,
                            Some(&state.ws_task),
                            &state.paths,
                            reason,
                            true,
                        )
                        .await;
                    }
                }
                _ => {}
            }
        } else if before_ts.is_some() && cached.len() < limit {
            if let Some(cursor_id) = before_msg_id.as_deref().filter(|s| !s.is_empty()) {
                match threads::fetch_older_thread_history(
                    &sess,
                    &thread_id,
                    kind,
                    cursor_id,
                    limit as u32,
                )
                .await
                {
                    Ok(remote_older) if !remote_older.is_empty() => {
                        let mut store = state.store.lock().await;
                        for m in &remote_older {
                            let _ = store.upsert_message(m);
                        }
                        cached = store
                            .load_messages(&thread_id, before_ts, limit + 1)
                            .map_err(|e| e.to_string())?;
                    }
                    Err(e) => {
                        if let Some(code) = expired_code(&e) {
                            let reason = format!(
                                "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                            );
                            clear_expired_session(
                                &app,
                                &state.session,
                                Some(&state.ws_task),
                                &state.paths,
                                reason,
                                true,
                            )
                            .await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let has_more = cached.len() > limit;
    if has_more {
        let excess = cached.len() - limit;
        cached.drain(0..excess);
    }
    Ok(MessagesPageDto {
        messages: cached,
        has_more,
    })
}

#[tauri::command]
async fn messages_load_around(
    state: State<'_, AppState>,
    thread_id: String,
    target_ts: i64,
) -> Result<MessagesPageDto, String> {
    let store = state.store.lock().await;
    let msgs = store
        .load_messages_around(&thread_id, target_ts, 25, 1000)
        .map_err(|e| e.to_string())?;
    let has_more = if let Some(first) = msgs.first() {
        !store
            .load_messages(&thread_id, Some(first.ts), 1)
            .map_err(|e| e.to_string())?
            .is_empty()
    } else {
        false
    };
    Ok(MessagesPageDto {
        messages: msgs,
        has_more,
    })
}

#[tauri::command]
async fn message_get_by_id(
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
) -> Result<Option<MessageDto>, String> {
    let store = state.store.lock().await;
    store
        .get_message(&thread_id, &msg_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn message_send_text(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    text: String,
) -> Result<MessageDto, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Tin nhắn không được để trống".into());
    }
    let kind = {
        let store = state.store.lock().await;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_text(&sess_snapshot, &thread_id, kind, trimmed).await {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
                    clear_expired_session(&app, &state.session, Some(&state.ws_task), &state.paths, reason.clone(), true).await;
                    return Err(reason);
                }
                return Err(format!("Gửi tin nhắn thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        MessageDto {
            msg_id: format!("local-{now_ms}"),
            thread_id: thread_id.clone(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: trimmed.to_string(),
            msg_type: "chat.text".into(),
            ts: now_ms,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        }
    };

    let mut store = state.store.lock().await;
    store.upsert_message(&msg).map_err(|e| e.to_string())?;
    let preview = format_thread_preview(&msg.text, true, "", false);
    let _ = store.update_thread_preview_with_unread(&msg.thread_id, &preview, msg.ts, false, true);
    let _ = app.emit("core://message-new", &msg);
    let _ = app.emit("core://threads-updated", ());
    Ok(msg)
}

#[tauri::command]
async fn message_send_quote(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    text: String,
    quoted_msg_id: String,
) -> Result<MessageDto, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Tin nhắn không được để trống".into());
    }
    let (kind, quoted) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let quoted = store
            .get_message(&thread_id, &quoted_msg_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Không tìm thấy tin nhắn gốc để trích dẫn".to_string())?;
        (kind, quoted)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_quote(&sess_snapshot, &thread_id, kind, trimmed, &quoted).await {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
                    clear_expired_session(&app, &state.session, Some(&state.ws_task), &state.paths, reason.clone(), true).await;
                    return Err(reason);
                }
                return Err(format!("Gửi trích dẫn thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        MessageDto {
            msg_id: format!("local-{now_ms}"),
            thread_id: thread_id.clone(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: trimmed.to_string(),
            msg_type: "chat.text".into(),
            ts: now_ms,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: Some(zalo_core::chat::QuoteRef {
                msg_id: quoted.msg_id,
                sender_name: quoted.sender_name,
                snippet: quoted.text,
            }),
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: None,
        }
    };

    let mut store = state.store.lock().await;
    store.upsert_message(&msg).map_err(|e| e.to_string())?;
    let preview = format_thread_preview(&msg.text, true, "", false);
    let _ = store.update_thread_preview_with_unread(&msg.thread_id, &preview, msg.ts, false, true);
    let _ = app.emit("core://message-new", &msg);
    let _ = app.emit("core://threads-updated", ());
    Ok(msg)
}

#[tauri::command]
async fn message_react(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
    emoji: String,
) -> Result<MessageDto, String> {
    let (kind, msg) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let msg = store
            .get_message(&thread_id, &msg_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Không tìm thấy tin nhắn".to_string())?;
        (kind, msg)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let remove = msg.reactions.iter().any(|r| r.emoji == emoji && r.me);
    if sess_snapshot.is_authenticated() {
        if let Err(e) =
            messaging::send_reaction(&sess_snapshot, &thread_id, kind, &msg, &emoji, remove).await
        {
            if let Some(code) = expired_code(&e) {
                let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
                clear_expired_session(&app, &state.session, Some(&state.ws_task), &state.paths, reason.clone(), true).await;
                return Err(reason);
            }
            return Err(format!("Thả cảm xúc thất bại: {e}"));
        }
    }
    let mut store = state.store.lock().await;
    let updated = store
        .apply_reaction(&thread_id, &msg_id, Some(&emoji), true, remove)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Không cập nhật được cảm xúc".to_string())?;
    let _ = app.emit("core://message-updated", &updated);
    Ok(updated)
}

#[tauri::command]
async fn message_recall(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
) -> Result<MessageDto, String> {
    let (kind, msg) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let msg = store
            .get_message(&thread_id, &msg_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Không tìm thấy tin nhắn".to_string())?;
        (kind, msg)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if sess_snapshot.is_authenticated() {
        if let Err(e) = messaging::recall_message(&sess_snapshot, &thread_id, kind, &msg).await {
            if let Some(code) = expired_code(&e) {
                let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
                clear_expired_session(&app, &state.session, Some(&state.ws_task), &state.paths, reason.clone(), true).await;
                return Err(reason);
            }
            return Err(format!("Thu hồi tin nhắn thất bại: {e}"));
        }
    }
    let mut store = state.store.lock().await;
    let updated = store
        .mark_recalled(&thread_id, &msg_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Không cập nhật được trạng thái thu hồi".to_string())?;
    let _ = app.emit("core://message-updated", &updated);
    Ok(updated)
}

#[tauri::command]
async fn message_delete_local(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
) -> Result<(), String> {
    let mut store = state.store.lock().await;
    store
        .delete_message(&thread_id, &msg_id)
        .map_err(|e| e.to_string())?;
    let _ = app.emit(
        "core://message-deleted",
        serde_json::json!({ "thread_id": thread_id, "msg_id": msg_id }),
    );
    Ok(())
}

fn attachment_meta_from_path(path: &std::path::Path) -> Result<AttachmentMetaDto, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Không đọc được tệp: {e}"))?;
    if !meta.is_file() {
        return Err("Đường dẫn không phải tệp hợp lệ".into());
    }
    let size_bytes = meta.len();
    messaging::validate_attachment_size(size_bytes).map_err(|e| e.to_string())?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("attachment.bin")
        .to_string();
    let is_image = messaging::is_image_extension(&file_name);
    Ok(AttachmentMetaDto {
        path: path.to_string_lossy().into_owned(),
        file_name,
        size_bytes,
        is_image,
    })
}

fn percent_decode_utf8(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn header_value_utf8(headers: &tauri::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(percent_decode_utf8)
}

fn local_demo_attachment_message(
    thread_id: &str,
    file_name: &str,
    caption: &str,
) -> MessageDto {
    let now_ms = chrono_now_ms();
    let is_image = messaging::is_image_extension(file_name);
    let trimmed_cap = caption.trim();
    let text = if is_image {
        if trimmed_cap.is_empty() {
            "[Hình ảnh]".into()
        } else {
            trimmed_cap.to_string()
        }
    } else if trimmed_cap.is_empty() {
        format!("[Tập tin] {file_name}")
    } else {
        format!("[Tập tin] {file_name}\n{trimmed_cap}")
    };
    MessageDto {
        msg_id: format!("local-{now_ms}"),
        thread_id: thread_id.to_string(),
        sender_id: "me".into(),
        sender_name: "Bạn".into(),
        text,
        msg_type: if is_image { "chat.photo" } else { "chat.file" }.into(),
        ts: now_ms,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: None,
        reactions: Vec::new(),
        attachment_url: None,
        attachment_name: Some(file_name.to_string()),
    }
}

async fn persist_and_emit_sent_message(
    app: &AppHandle,
    state: &State<'_, AppState>,
    msg: MessageDto,
) -> Result<MessageDto, String> {
    let mut store = state.store.lock().await;
    store.upsert_message(&msg).map_err(|e| e.to_string())?;
    let preview = format_thread_preview(&msg.text, true, "", false);
    let _ = store.update_thread_preview_with_unread(&msg.thread_id, &preview, msg.ts, false, true);
    let _ = app.emit("core://message-new", &msg);
    let _ = app.emit("core://threads-updated", ());
    Ok(msg)
}

#[tauri::command]
async fn pick_attachment_file(app: AppHandle) -> Result<Option<AttachmentMetaDto>, String> {
    #[cfg(target_os = "windows")]
    let owner_hwnd = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as usize);
    #[cfg(not(target_os = "windows"))]
    let owner_hwnd = {
        let _ = app;
        None
    };

    let selected = tokio::task::spawn_blocking(move || file_dialog::pick_single_file(owner_hwnd))
        .await
        .map_err(|e| format!("Lỗi tác vụ chọn tệp: {e}"))??;
    selected.map(|p| attachment_meta_from_path(&p)).transpose()
}

#[tauri::command]
async fn inspect_attachment_path(file_path: String) -> Result<AttachmentMetaDto, String> {
    attachment_meta_from_path(std::path::Path::new(&file_path))
}

#[tauri::command]
async fn message_send_attachment_path(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    file_path: String,
    caption: String,
) -> Result<MessageDto, String> {
    let path = PathBuf::from(&file_path);
    let meta = attachment_meta_from_path(&path)?;

    let kind = {
        let store = state.store.lock().await;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_attachment_file(&sess_snapshot, &thread_id, kind, &path, &caption)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason = format!(
                        "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                    );
                    clear_expired_session(
                        &app,
                        &state.session,
                        Some(&state.ws_task),
                        &state.paths,
                        reason.clone(),
                        true,
                    )
                    .await;
                    return Err(reason);
                }
                return Err(format!("Gửi tệp thất bại: {e}"));
            }
        }
    } else {
        local_demo_attachment_message(&thread_id, &meta.file_name, &caption)
    };

    persist_and_emit_sent_message(&app, &state, msg).await
}

#[tauri::command]
async fn message_send_raw_attachment(
    app: AppHandle,
    state: State<'_, AppState>,
    request: tauri::ipc::Request<'_>,
) -> Result<MessageDto, String> {
    let thread_id = header_value_utf8(request.headers(), "x-thread-id")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "Thiếu x-thread-id".to_string())?;
    let file_name = header_value_utf8(request.headers(), "x-file-name")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "clipboard.png".to_string());
    let caption = header_value_utf8(request.headers(), "x-caption").unwrap_or_default();

    let bytes = match request.body() {
        InvokeBody::Raw(raw) => raw.as_slice(),
        InvokeBody::Json(_) => return Err("Dữ liệu đính kèm phải truyền dạng nhị phân raw".into()),
    };
    messaging::validate_attachment_size(bytes.len() as u64).map_err(|e| e.to_string())?;

    let kind = {
        let store = state.store.lock().await;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_attachment_bytes(
            &sess_snapshot,
            &thread_id,
            kind,
            &file_name,
            bytes,
            &caption,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason = format!(
                        "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                    );
                    clear_expired_session(
                        &app,
                        &state.session,
                        Some(&state.ws_task),
                        &state.paths,
                        reason.clone(),
                        true,
                    )
                    .await;
                    return Err(reason);
                }
                return Err(format!("Gửi tệp thất bại: {e}"));
            }
        }
    } else {
        local_demo_attachment_message(&thread_id, &file_name, &caption)
    };

    persist_and_emit_sent_message(&app, &state, msg).await
}

#[tauri::command]
async fn search_global(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<MessageDto>, String> {
    let store = state.store.lock().await;
    store
        .search_messages(&query, limit.unwrap_or(30))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn translate_get_config(state: State<'_, AppState>) -> Result<TranslateConfigSummary, String> {
    let cfg = state.translate_cfg.lock().await;
    Ok(cfg.summary())
}

#[tauri::command]
async fn translate_save_config(
    state: State<'_, AppState>,
    update: TranslateConfigUpdate,
) -> Result<TranslateConfigSummary, String> {
    let mut cfg = state.translate_cfg.lock().await;
    cfg.apply_update(update);
    cfg.save_to_file(&state.paths.translate_file())
        .map_err(|e| format!("Không lưu được cấu hình dịch: {e}"))?;
    Ok(cfg.summary())
}

#[tauri::command]
async fn translate_messages(
    state: State<'_, AppState>,
    items: Vec<TranslateInputItem>,
    target_lang: Option<String>,
    provider: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>, String> {
    let cfg = state.translate_cfg.lock().await.clone();
    translate_batch(&cfg, &items, target_lang.as_deref(), provider)
        .await
        .map_err(|e| e.to_string())
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn run() {
    let paths = StoragePaths::resolve_default(None).expect("Failed to resolve storage directory");
    let session_path = paths.session_file();
    let session = Session::load_from_file(&session_path)
        .unwrap_or_else(|_| Session::new().expect("Failed to initialize session"));
    let uid = if session.profile.uid.is_empty() {
        None
    } else {
        Some(session.profile.uid.as_str())
    };
    let db_path = paths.history_db(uid);
    let store = MessageStore::open(&db_path).expect("Failed to open SQLite history database");

    let translate_cfg = TranslateConfig::load_from_file(&paths.translate_file());
    let sess_arc = Arc::new(Mutex::new(session));
    let store_arc = Arc::new(Mutex::new(store));
    let ws_task_arc = Arc::new(Mutex::new(None));
    let qr_task_arc = Arc::new(Mutex::new(None));

    let state = AppState {
        paths,
        session: sess_arc,
        store: store_arc,
        ws_task: ws_task_arc,
        qr_task: qr_task_arc,
        active_thread_id: Arc::new(Mutex::new(None)),
        translate_cfg: Arc::new(Mutex::new(translate_cfg)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            auth_get_status,
            auth_start_qr,
            auth_logout,
            threads_sync,
            threads_list,
            thread_mark_read,
            thread_mark_unread,
            thread_toggle_pin,
            thread_toggle_mute,
            thread_get_details,
            user_get_profile,
            messages_load,
            messages_load_around,
            message_get_by_id,
            message_send_text,
            message_send_quote,
            message_react,
            message_recall,
            message_delete_local,
            pick_attachment_file,
            inspect_attachment_path,
            message_send_attachment_path,
            message_send_raw_attachment,
            search_global,
            translate_get_config,
            translate_save_config,
            translate_messages,
        ])
        .setup(move |app| {
            let _ = app.get_webview_window("main");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running litechat");
}
