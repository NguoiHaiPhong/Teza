//! Tauri v2 CoreBridge connecting `zalo-core` to the Telegram-inspired React UI.

mod clipboard;
mod file_dialog;
mod screen_shake;
mod screenshot;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::ipc::InvokeBody;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    State, WebviewBuilder, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use zalo_core::chat::messaging;
use zalo_core::chat::parser::{
    format_thread_preview, is_raw_numeric_id, parse_ws_event, pick_str, sticker_image_url,
    WsParsedAction,
};
use zalo_core::chat::threads;
use zalo_core::chat::{
    AddGroupMembersResultDto, ChatFolderDto, FolderFilter, MessageDto, PollDto, ThreadDetailsDto,
    ThreadDto, ThreadKind, UserProfileDto,
};
use zalo_core::data_dir::StoragePaths;
use zalo_core::qr::{self, QrTicket};
use zalo_core::relay::{RelayConfig, RelayConfigSummary, RelayConfigUpdate, RelayMode};
use zalo_core::session::{AccountProfile, Session, SessionError, SessionStatus};
use zalo_core::socket::{self, WsSignal};
use zalo_core::store::MessageStore;
use zalo_core::translate::{
    classify_skip_reason, classify_skip_reason_for_mode, is_static_non_translatable,
    normalize_lang_base_code, provider_name, translate_api_only_with_relay, TranslateConfig,
    TranslateConfigSummary, TranslateConfigUpdate, TranslateDetectMode, TranslateInputItem,
    TranslateProvider, TranslationResult,
};

pub struct AppState {
    pub paths: StoragePaths,
    pub session: Arc<Mutex<Session>>,
    pub store: Arc<Mutex<MessageStore>>,
    pub ws_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub qr_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub active_thread_id: Arc<Mutex<Option<String>>>,
    pub translate_cfg: Arc<Mutex<TranslateConfig>>,
    pub relay_cfg: Arc<Mutex<RelayConfig>>,
    pub pending_update: Arc<Mutex<Option<tauri_plugin_updater::Update>>>,
    pub close_to_tray: Arc<AtomicBool>,
    pub allow_exit: Arc<AtomicBool>,
    pub pending_notify: Arc<Mutex<Option<NotifyToastPayload>>>,
    pub notify_hide_gen: Arc<AtomicU64>,
    pub active_webapp_label: Arc<Mutex<Option<String>>>,
    pub recent_saved_files: Arc<Mutex<VecDeque<PathBuf>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyToastPayload {
    pub thread_id: String,
    pub title: String,
    pub body: String,
    pub avatar: String,
    pub theme: String,
    pub position: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub is_buzz: bool,
    #[serde(default)]
    pub buzz_streak: u32,
}

fn compute_notify_shake_offsets(scale: f64) -> Vec<(i32, i32)> {
    let max_amp = (10.0 * scale.max(0.5)).round() as i32;
    let frames = 26usize;
    let mut out = Vec::with_capacity(frames);
    for i in 0..(frames - 1) {
        let t = i as f64 / (frames - 1) as f64;
        let decay = 1.0 - t;
        let step_x = if i % 2 == 0 { 1.0 } else { -1.0 };
        let step_y = if i % 4 < 2 { 1.0 } else { -1.0 };
        let dx = (step_x * max_amp as f64 * decay).round() as i32;
        let dy = (step_y * (max_amp as f64 * 0.45) * decay).round() as i32;
        out.push((dx, dy));
    }
    out.push((0, 0));
    out
}

const TOAST_LOGICAL_W: f64 = 320.0;
const TOAST_LOGICAL_H_COMPACT: f64 = 72.0;
const TOAST_LOGICAL_H_TALL: f64 = 86.0;
const TOAST_MARGIN: f64 = 16.0;

fn compute_toast_logical_size(body: &str) -> (f64, f64) {
    let trimmed = body.trim();
    if trimmed.contains('\n') || trimmed.chars().count() > 38 {
        (TOAST_LOGICAL_W, TOAST_LOGICAL_H_TALL)
    } else {
        (TOAST_LOGICAL_W, TOAST_LOGICAL_H_COMPACT)
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_notify_position_for_size(
    work_x: i32,
    work_y: i32,
    work_w: u32,
    work_h: u32,
    win_w: u32,
    win_h: u32,
    scale: f64,
    position: &str,
) -> (i32, i32) {
    let pm = (TOAST_MARGIN * scale).round().max(0.0) as i32;
    let ww = work_w as i32;
    let wh = work_h as i32;
    let pw = (win_w as i32).min(ww.max(1));
    let ph = (win_h as i32).min(wh.max(1));
    let max_x = work_x + (ww - pw).max(0);
    let max_y = work_y + (wh - ph).max(0);
    let left_x = (work_x + pm).clamp(work_x, max_x);
    let right_x = (work_x + ww - pw - pm).clamp(work_x, max_x);
    let top_y = (work_y + pm).clamp(work_y, max_y);
    let bottom_y = (work_y + wh - ph - pm).clamp(work_y, max_y);
    match position {
        "top-left" => (left_x, top_y),
        "top-right" => (right_x, top_y),
        "bottom-left" => (left_x, bottom_y),
        _ => (right_x, bottom_y),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn compute_notify_position(
    work_x: i32,
    work_y: i32,
    work_w: u32,
    work_h: u32,
    scale: f64,
    position: &str,
) -> (i32, i32) {
    let pw = (TOAST_LOGICAL_W * scale).round() as u32;
    let ph = (TOAST_LOGICAL_H_COMPACT * scale).round() as u32;
    compute_notify_position_for_size(work_x, work_y, work_w, work_h, pw, ph, scale, position)
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
pub struct ThreadMessagesSyncedPayload {
    pub thread_id: String,
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
        if let Ok(fresh) = Session::new_with_relay(sess.relay.clone()) {
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
        let initial_titles = store_arc
            .lock()
            .await
            .resolve_known_titles()
            .unwrap_or_default();
        let ws_name_cache = Arc::new(Mutex::new(initial_titles));
        let in_flight_uids = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));

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
                    if !ev.is_chat_ish() && !ev.is_control_ish() && !ev.is_receipt_ish() {
                        continue;
                    }
                    let mut actions = parse_ws_event(&ev, &my_uid, &send2me_id);
                    if actions.is_empty() {
                        continue;
                    }

                    let mut missing_uids: Vec<String> = Vec::new();
                    {
                        let cache = ws_name_cache.lock().await;
                        let mut inflight = in_flight_uids.lock().await;
                        for act in &mut actions {
                            if let WsParsedAction::Message(msg) = act {
                                if !msg.is_outgoing
                                    && !msg.sender_id.is_empty()
                                    && (msg.sender_name == msg.sender_id
                                        || is_raw_numeric_id(&msg.sender_name))
                                {
                                    if let Some(name) = cache.get(&msg.sender_id) {
                                        msg.sender_name = name.clone();
                                    } else if inflight.insert(msg.sender_id.clone())
                                        && !missing_uids.iter().any(|x| x == &msg.sender_id)
                                    {
                                        missing_uids.push(msg.sender_id.clone());
                                    }
                                }
                            }
                        }
                    }

                    let active_id = active_thread_arc.lock().await.clone();
                    let mut pending_resolve_msgs: Vec<MessageDto> = Vec::new();
                    let mut threads_touched = false;
                    {
                        let mut store = store_arc.lock().await;
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
                                    let created = store
                                        .update_thread_preview_returning_created(
                                            &msg.thread_id,
                                            &preview,
                                            msg.ts,
                                            inc_unread,
                                            clear_unread,
                                        )
                                        .unwrap_or(false);
                                    if created {
                                        threads_touched = true;
                                    }
                                    let _ = app.emit("core://message-new", &*msg);
                                    if !missing_uids.is_empty()
                                        && missing_uids.iter().any(|u| u == &msg.sender_id)
                                    {
                                        pending_resolve_msgs.push(*msg);
                                    }
                                }
                            WsParsedAction::Read {
                                thread_id,
                                is_unread,
                            } => {
                                let _ = store.set_thread_unread(&thread_id, if is_unread { 1 } else { 0 });
                                threads_touched = true;
                            }
                            WsParsedAction::PeerSeen {
                                thread_id,
                                seen_ts,
                                is_delivered_only,
                            } => {
                                if !is_delivered_only {
                                    if let Ok(peer_ts) = store.update_peer_read_ts(&thread_id, seen_ts) {
                                        let _ = app.emit(
                                            "core://thread-receipt",
                                            serde_json::json!({
                                                "thread_id": thread_id,
                                                "peer_read_ts": peer_ts,
                                            }),
                                        );
                                    }
                                }
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
                            WsParsedAction::Voip(_) => {}
                        }
                        }
                    }
                    if threads_touched {
                        let _ = app.emit("core://threads-updated", ());
                    }
                    if !missing_uids.is_empty() {
                        let app_bg = app.clone();
                        let sess_bg = sess_clone.clone_shallow();
                        let store_bg = store_arc.clone();
                        let cache_bg = ws_name_cache.clone();
                        let inflight_bg = in_flight_uids.clone();
                        tokio::spawn(async move {
                            let mut resolved = std::collections::HashMap::<String, String>::new();
                            if let Ok(profiles) =
                                threads::get_mini_profiles(&sess_bg, &missing_uids).await
                            {
                                for (uid, p) in profiles {
                                    if let Some(name) = pick_str(
                                        &p,
                                        &["displayName", "zaloName", "name", "display_name"],
                                    ) {
                                        resolved.insert(uid, name);
                                    }
                                }
                            }
                            if !resolved.is_empty() {
                                {
                                    let mut cache = cache_bg.lock().await;
                                    for (k, v) in &resolved {
                                        cache.insert(k.clone(), v.clone());
                                    }
                                }
                                let mut store = store_bg.lock().await;
                                for mut msg in pending_resolve_msgs {
                                    if let Some(name) = resolved.get(&msg.sender_id) {
                                        msg.sender_name = name.clone();
                                        let _ = store.upsert_message(&msg);
                                        let _ = app_bg.emit("core://message-updated", &msg);
                                    }
                                }
                            }
                            let mut inflight = inflight_bg.lock().await;
                            for uid in &missing_uids {
                                inflight.remove(uid);
                            }
                        });
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
                    let latest_relay = guard.relay.clone();
                    if sess_snapshot.relay != latest_relay {
                        let _ = sess_snapshot.set_relay(latest_relay);
                    }
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
        *sess = Session::new_with_relay(sess.relay.clone()).map_err(|e| e.to_string())?;
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
    let remote = match threads::sync_threads_full(&sess_snapshot).await {
        Ok(res) => res,
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
    for t in &remote.threads {
        if remote.pinned_synced {
            let _ = store.upsert_thread_synced(t);
        } else {
            let _ = store.upsert_thread(t);
        }
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

fn spawn_best_effort_thread_sync<F>(
    app: AppHandle,
    state: &State<'_, AppState>,
    fut_fn: impl FnOnce(Session) -> F + Send + 'static,
) where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let sess_arc = state.session.clone();
    let ws_arc = state.ws_task.clone();
    let paths = state.paths.clone();
    tauri::async_runtime::spawn(async move {
        let sess_snapshot = {
            let sess = sess_arc.lock().await;
            sess.clone_shallow()
        };
        if !sess_snapshot.is_authenticated() {
            return;
        }
        if let Err(e) = fut_fn(sess_snapshot).await {
            if let Some(code) = expired_code(&e) {
                let reason = format!(
                    "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                );
                clear_expired_session(&app, &sess_arc, Some(&ws_arc), &paths, reason, true).await;
            }
        }
    });
}

#[tauri::command]
async fn thread_set_active(
    state: State<'_, AppState>,
    thread_id: Option<String>,
) -> Result<(), String> {
    *state.active_thread_id.lock().await = thread_id;
    Ok(())
}

#[tauri::command]
async fn thread_mark_read(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    *state.active_thread_id.lock().await = Some(thread_id.clone());
    let (kind, latest_incoming) = {
        let store = state.store.lock().await;
        store.mark_thread_read(&thread_id).map_err(|e| e.to_string())?;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let latest = store.get_latest_incoming_message(&thread_id).ok().flatten();
        (kind, latest)
    };
    if thread_id != "cloud-me" {
        let tid = thread_id.clone();
        spawn_best_effort_thread_sync(app, &state, move |sess| async move {
            let res = threads::sync_unread_mark(&sess, &tid, kind, false).await;
            if let Some((mid, sender_id, _)) = latest_incoming {
                let _ = threads::send_seen_event(&sess, &tid, kind, &mid, &sender_id).await;
            }
            res
        });
    }
    Ok(())
}

#[tauri::command]
async fn thread_mark_unread(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    {
        let mut active = state.active_thread_id.lock().await;
        if active.as_deref() == Some(thread_id.as_str()) {
            *active = None;
        }
    }
    let kind = {
        let store = state.store.lock().await;
        store
            .set_thread_unread(&thread_id, 1)
            .map_err(|e| e.to_string())?;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    if thread_id != "cloud-me" {
        let tid = thread_id.clone();
        spawn_best_effort_thread_sync(app, &state, move |sess| async move {
            threads::sync_unread_mark(&sess, &tid, kind, true).await
        });
    }
    Ok(())
}

#[tauri::command]
async fn thread_toggle_pin(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    pinned: bool,
) -> Result<(), String> {
    let kind = {
        let store = state.store.lock().await;
        store
            .set_thread_pinned(&thread_id, pinned)
            .map_err(|e| e.to_string())?;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    if thread_id != "cloud-me" {
        let tid = thread_id.clone();
        spawn_best_effort_thread_sync(app, &state, move |sess| async move {
            threads::sync_pin_thread(&sess, &tid, kind, pinned).await
        });
    }
    Ok(())
}

#[tauri::command]
async fn thread_toggle_mute(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    muted: bool,
    duration_secs: Option<i64>,
) -> Result<i64, String> {
    let muted_until_ts = if muted {
        duration_secs
            .filter(|d| *d > 0)
            .map(|d| chrono_now_ms().saturating_add(d.saturating_mul(1000)))
            .unwrap_or(0)
    } else {
        0
    };
    let kind = {
        let store = state.store.lock().await;
        store
            .set_thread_muted_until(&thread_id, muted, muted_until_ts)
            .map_err(|e| e.to_string())?;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    if thread_id != "cloud-me" {
        let tid = thread_id.clone();
        spawn_best_effort_thread_sync(app, &state, move |sess| async move {
            threads::sync_mute_thread(&sess, &tid, kind, muted, duration_secs).await
        });
    }
    Ok(muted_until_ts)
}

#[tauri::command]
async fn thread_toggle_archive(
    state: State<'_, AppState>,
    thread_id: String,
    archived: bool,
) -> Result<(), String> {
    let store = state.store.lock().await;
    store
        .set_thread_archived(&thread_id, archived)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn notify_show_popup(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: NotifyToastPayload,
) -> Result<(), String> {
    *state.pending_notify.lock().await = Some(payload.clone());

    let target_mon = app
        .get_webview_window("main")
        .filter(|m| m.is_visible().unwrap_or(false) && !m.is_minimized().unwrap_or(false))
        .and_then(|m| m.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());

    let (work_x, work_y, work_w, work_h, scale) = target_mon
        .map(|mon| {
            let work = mon.work_area();
            (
                work.position.x,
                work.position.y,
                work.size.width,
                work.size.height,
                mon.scale_factor().max(0.5),
            )
        })
        .unwrap_or((0, 0, 1920, 1040, 1.0));

    let (logical_w, logical_h) = compute_toast_logical_size(&payload.body);
    let pw = ((logical_w * scale).round() as u32).clamp(1, work_w.max(1));
    let ph = ((logical_h * scale).round() as u32).clamp(1, work_h.max(1));
    let (est_x, est_y) = compute_notify_position_for_size(
        work_x,
        work_y,
        work_w,
        work_h,
        pw,
        ph,
        scale,
        &payload.position,
    );

    let win = if let Some(existing) = app.get_webview_window("teza-notify") {
        existing
    } else {
        WebviewWindowBuilder::new(
            &app,
            "teza-notify",
            WebviewUrl::App("index.html?view=notify".into()),
        )
        .title("Teza Notification")
        .inner_size(logical_w, logical_h)
        .position(est_x as f64 / scale, est_y as f64 / scale)
        .resizable(false)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?
    };

    let gen = state.notify_hide_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = win.set_shadow(false);
    // Move to the target monitor first so DPI and outer_size reflect the destination monitor.
    let _ = win.set_position(PhysicalPosition::new(est_x, est_y));
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let (actual_w, actual_h) = win
        .outer_size()
        .ok()
        .filter(|s| s.width > 0 && s.height > 0)
        .map(|s| (s.width, s.height))
        .unwrap_or((pw, ph));
    let (px, py) = compute_notify_position_for_size(
        work_x,
        work_y,
        work_w,
        work_h,
        actual_w,
        actual_h,
        scale,
        &payload.position,
    );
    let _ = win.set_position(PhysicalPosition::new(px, py));
    let _ = app.emit_to("teza-notify", "core://notify-payload", &payload);
    let _ = win.show();

    if payload.is_buzz && payload.buzz_streak < 3 {
        let gen_shake = state.notify_hide_gen.clone();
        let app_shake = app.clone();
        let max_x = work_x + (work_w as i32 - actual_w as i32).max(0);
        let max_y = work_y + (work_h as i32 - actual_h as i32).max(0);
        let offsets = compute_notify_shake_offsets(scale);
        tokio::spawn(async move {
            for (dx, dy) in offsets {
                if gen_shake.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Some(w) = app_shake.get_webview_window("teza-notify") {
                    let sx = (px + dx).clamp(work_x, max_x);
                    let sy = (py + dy).clamp(work_y, max_y);
                    let _ = w.set_position(PhysicalPosition::new(sx, sy));
                }
                tokio::time::sleep(std::time::Duration::from_millis(28)).await;
            }
            if gen_shake.load(Ordering::SeqCst) == gen {
                if let Some(w) = app_shake.get_webview_window("teza-notify") {
                    let _ = w.set_position(PhysicalPosition::new(px, py));
                }
            }
        });
    }

    let gen_arc = state.notify_hide_gen.clone();
    let app_hide = app.clone();
    let ms = payload.duration_ms.clamp(2500, 15000);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        if gen_arc.load(Ordering::SeqCst) == gen {
            if let Some(w) = app_hide.get_webview_window("teza-notify") {
                let _ = w.hide();
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn buzz_shake_screen() -> Result<(), String> {
    screen_shake::trigger_screen_shake()
}

#[tauri::command]
async fn notify_get_current(state: State<'_, AppState>) -> Result<Option<NotifyToastPayload>, String> {
    Ok(state.pending_notify.lock().await.clone())
}

#[tauri::command]
async fn notify_dismiss(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.notify_hide_gen.fetch_add(1, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("teza-notify") {
        let _ = w.hide();
    }
    Ok(())
}

#[tauri::command]
async fn notify_click(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    state.notify_hide_gen.fetch_add(1, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("teza-notify") {
        let _ = w.hide();
    }
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.show();
        let _ = main_win.unminimize();
        let _ = main_win.set_focus();
    }
    let _ = app.emit_to("main", "core://open-thread", thread_id);
    Ok(())
}

#[tauri::command]
async fn thread_get_details(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<ThreadDetailsDto, String> {
    let (kind, media_items, file_items, link_items, known_profiles) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let (media, files) = store
            .list_thread_media(&thread_id, 60)
            .unwrap_or_default();
        let links = store
            .list_thread_links(&thread_id, 60)
            .unwrap_or_default();
        let known = if kind == ThreadKind::Group {
            store.resolve_known_profiles().unwrap_or_default()
        } else {
            Default::default()
        };
        (kind, media, files, links, known)
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
        link_items,
    })
}

#[tauri::command]
async fn folders_list(state: State<'_, AppState>) -> Result<Vec<ChatFolderDto>, String> {
    let store = state.store.lock().await;
    store.list_chat_folders().map_err(|e| e.to_string())
}

#[tauri::command]
async fn folder_save(
    state: State<'_, AppState>,
    mut folder: ChatFolderDto,
) -> Result<Vec<ChatFolderDto>, String> {
    folder.id = folder.id.trim().to_string();
    folder.name = folder.name.trim().to_string();
    if folder.id.is_empty() {
        return Err("Mã thư mục không hợp lệ".into());
    }
    if folder.name.is_empty() {
        return Err("Tên thư mục không được để trống".into());
    }
    if folder.icon.trim().is_empty() {
        folder.icon = "folder".to_string();
    }
    let store = state.store.lock().await;
    store.save_chat_folder(&folder).map_err(|e| e.to_string())?;
    store.list_chat_folders().map_err(|e| e.to_string())
}

#[tauri::command]
async fn folder_delete(
    state: State<'_, AppState>,
    folder_id: String,
) -> Result<Vec<ChatFolderDto>, String> {
    let store = state.store.lock().await;
    store
        .delete_chat_folder(&folder_id)
        .map_err(|e| e.to_string())?;
    store.list_chat_folders().map_err(|e| e.to_string())
}

#[tauri::command]
async fn folders_reorder(
    state: State<'_, AppState>,
    ordered_ids: Vec<String>,
) -> Result<Vec<ChatFolderDto>, String> {
    let store = state.store.lock().await;
    store
        .reorder_chat_folders(&ordered_ids)
        .map_err(|e| e.to_string())?;
    store.list_chat_folders().map_err(|e| e.to_string())
}

#[tauri::command]
async fn folders_sync_remote_labels(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ChatFolderDto>, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        sess.clone_shallow()
    };
    let remote = match threads::fetch_remote_labels(&sess_snapshot).await {
        Ok(list) => list,
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
            return Err(format!("Không đồng bộ được thẻ phân loại: {e}"));
        }
    };
    let store = state.store.lock().await;
    let existing = store.list_chat_folders().unwrap_or_default();
    for mut rf in remote {
        if let Some(old) = existing
            .iter()
            .find(|f| f.remote_label_id.is_some() && f.remote_label_id == rf.remote_label_id)
        {
            rf.id = old.id.clone();
            rf.icon = old.icon.clone();
            rf.include_kinds = old.include_kinds.clone();
            rf.excluded_thread_ids = old.excluded_thread_ids.clone();
            rf.only_unread = old.only_unread;
            rf.exclude_muted = old.exclude_muted;
            rf.position = old.position;
        }
        let _ = store.save_chat_folder(&rf);
    }
    store.list_chat_folders().map_err(|e| e.to_string())
}

#[tauri::command]
async fn user_get_profile(
    state: State<'_, AppState>,
    user_id: String,
) -> Result<UserProfileDto, String> {
    let clean_uid = user_id
        .trim()
        .split('_')
        .next()
        .unwrap_or(user_id.trim())
        .to_string();
    let (known_profiles, group_threads, (msg_count, media_count, last_ts)) = {
        let store = state.store.lock().await;
        let known = store.resolve_known_profiles().unwrap_or_default();
        let groups = store.list_threads(FolderFilter::Group).unwrap_or_default();
        let stats = store
            .thread_interaction_stats(&clean_uid)
            .unwrap_or((0, 0, 0));
        (known, groups, stats)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let mut profile = threads::fetch_user_profile(&sess_snapshot, &user_id, &known_profiles)
        .await
        .map_err(|e| format!("Không tra được thông tin tài khoản: {e}"))?;
    if !profile.is_self && sess_snapshot.is_authenticated() && !group_threads.is_empty() {
        profile.mutual_groups =
            threads::fetch_mutual_groups(&sess_snapshot, &profile.uid, &group_threads).await;
    }
    profile.local_message_count = msg_count;
    profile.local_media_count = media_count;
    profile.last_interaction_ts = last_ts;
    Ok(profile)
}

#[tauri::command]
async fn user_find_by_phone(
    state: State<'_, AppState>,
    phone: String,
) -> Result<UserProfileDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    threads::find_user_by_phone(&sess_snapshot, &phone)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn group_add_members(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    member_ids: Vec<String>,
) -> Result<AddGroupMembersResultDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        return Ok(AddGroupMembersResultDto {
            added: member_ids,
            pending: Vec::new(),
            failed: Vec::new(),
            message: String::new(),
        });
    }
    match threads::add_group_members(&sess_snapshot, &thread_id, &member_ids).await {
        Ok(res) => Ok(res),
        Err(e) => {
            if let Some(code) = expired_code(&e) {
                let reason =
                    format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
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
            Err(format!("Không thêm được thành viên vào nhóm: {e}"))
        }
    }
}

#[tauri::command]
async fn reminder_create(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
    start_time_ms: i64,
    repeat: i32,
) -> Result<MessageDto, String> {
    let clean_title = title.trim();
    if clean_title.is_empty() {
        return Err("Nội dung nhắc hẹn không được để trống".into());
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
        match threads::create_reminder(
            &sess_snapshot,
            &thread_id,
            kind,
            clean_title,
            start_time_ms,
            repeat,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason =
                        format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
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
                return Err(format!("Tạo nhắc hẹn thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        let meta = serde_json::json!({
            "title": clean_title,
            "startTime": start_time_ms,
            "repeat": repeat,
            "reminderId": format!("local-rem-{now_ms}"),
        })
        .to_string();
        MessageDto {
            msg_id: format!("local-rem-{now_ms}"),
            thread_id: thread_id.clone(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: format!("Nhắc hẹn: {clean_title}"),
            msg_type: "chat.reminder".into(),
            ts: now_ms,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!("teza-reminder:{meta}")),
        }
    };
    persist_and_emit_sent_message(&app, &state, msg).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn poll_create(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    question: String,
    options: Vec<String>,
    expired_time_ms: i64,
    allow_multi: bool,
    allow_add_option: bool,
    is_anonymous: bool,
) -> Result<MessageDto, String> {
    let clean_q = question.trim();
    if clean_q.is_empty() {
        return Err("Câu hỏi bình chọn không được để trống".into());
    }
    let clean_opts: Vec<String> = options
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if clean_opts.len() < 2 {
        return Err("Cần ít nhất 2 lựa chọn để tạo bình chọn".into());
    }
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let msg = if sess_snapshot.is_authenticated() {
        match threads::create_poll(
            &sess_snapshot,
            &thread_id,
            clean_q,
            &clean_opts,
            expired_time_ms,
            allow_multi,
            allow_add_option,
            is_anonymous,
        )
        .await
        {
            Ok(m) => m,
            Err(e) => {
                if let Some(code) = expired_code(&e) {
                    let reason =
                        format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
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
                return Err(format!("Tạo bình chọn thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        let dto = PollDto {
            poll_id: now_ms,
            question: clean_q.to_string(),
            options: clean_opts
                .iter()
                .enumerate()
                .map(|(i, content)| zalo_core::chat::PollOptionDto {
                    id: (i + 1) as i64,
                    content: content.clone(),
                    votes: 0,
                    voted: false,
                })
                .collect(),
            allow_multi,
            allow_add_option,
            is_anonymous,
            expired_time: expired_time_ms.max(0),
            closed: false,
        };
        let poll_json = serde_json::to_string(&dto).map_err(|e| e.to_string())?;
        MessageDto {
            msg_id: format!("local-poll-{now_ms}"),
            thread_id: thread_id.clone(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: format!("Bình chọn: {clean_q}"),
            msg_type: "chat.poll".into(),
            ts: now_ms,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!("teza-poll:{poll_json}")),
        }
    };
    persist_and_emit_sent_message(&app, &state, msg).await
}

async fn update_local_poll_message(
    app: &AppHandle,
    state: &State<'_, AppState>,
    thread_id: Option<&str>,
    msg_id: Option<&str>,
    poll: &PollDto,
) {
    let (Some(tid), Some(mid)) = (thread_id, msg_id) else {
        return;
    };
    let Ok(poll_json) = serde_json::to_string(poll) else {
        return;
    };
    let mut store = state.store.lock().await;
    if let Ok(Some(mut msg)) = store.get_message(tid, mid) {
        msg.msg_type = "chat.poll".to_string();
        msg.attachment_url = None;
        msg.attachment_name = Some(format!("teza-poll:{poll_json}"));
        if store.upsert_message(&msg).is_ok() {
            let _ = app.emit("core://message-updated", &msg);
        }
    }
}

#[tauri::command]
async fn poll_get_detail(
    app: AppHandle,
    state: State<'_, AppState>,
    poll_id: i64,
    thread_id: Option<String>,
    msg_id: Option<String>,
) -> Result<PollDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        return Err("Chế độ xem trước không đồng bộ dữ liệu máy chủ".into());
    }
    let poll = match threads::get_poll_detail(&sess_snapshot, poll_id).await {
        Ok(p) => p,
        Err(e) => {
            if let Some(code) = expired_code(&e) {
                let reason =
                    format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
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
            return Err(format!("Không tải được bình chọn: {e}"));
        }
    };
    update_local_poll_message(&app, &state, thread_id.as_deref(), msg_id.as_deref(), &poll).await;
    Ok(poll)
}

#[tauri::command]
async fn poll_vote(
    app: AppHandle,
    state: State<'_, AppState>,
    poll_id: i64,
    option_ids: Vec<i64>,
    thread_id: Option<String>,
    msg_id: Option<String>,
) -> Result<PollDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        return Err("Chế độ xem trước không đồng bộ bình chọn máy chủ".into());
    }
    let poll = match threads::vote_poll(&sess_snapshot, poll_id, &option_ids).await {
        Ok(p) => p,
        Err(e) => {
            if let Some(code) = expired_code(&e) {
                let reason =
                    format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
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
            return Err(format!("Bình chọn thất bại: {e}"));
        }
    };
    update_local_poll_message(&app, &state, thread_id.as_deref(), msg_id.as_deref(), &poll).await;
    Ok(poll)
}

struct RemoteThreadSyncCtx {
    app: AppHandle,
    store: Arc<Mutex<MessageStore>>,
    session: Arc<Mutex<Session>>,
    ws_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    paths: StoragePaths,
    sess: Session,
    thread_id: String,
    kind: ThreadKind,
    limit: usize,
}

fn message_sync_sig(m: &MessageDto) -> (String, i64, bool, bool, usize, String, String) {
    (
        m.msg_id.clone(),
        m.ts,
        m.is_recalled,
        m.is_pinned,
        m.reactions.len(),
        m.text.clone(),
        m.sender_name.clone(),
    )
}

fn is_server_msg_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with("local-")
        && !id.starts_with("out_")
        && !id.starts_with("opt-")
}

async fn resolve_and_upsert_remote_messages(
    app: &AppHandle,
    store_arc: &Arc<Mutex<MessageStore>>,
    sess: &Session,
    thread_id: &str,
    kind: ThreadKind,
    remote_msgs: &mut [MessageDto],
    update_preview: bool,
) {
    let mut known_titles = {
        let store = store_arc.lock().await;
        store.resolve_known_titles().unwrap_or_default()
    };
    let unresolved_senders: Vec<String> = remote_msgs
        .iter()
        .filter(|m| {
            !m.is_outgoing
                && !m.sender_id.is_empty()
                && (m.sender_name == m.sender_id || is_raw_numeric_id(&m.sender_name))
                && !known_titles.contains_key(&m.sender_id)
        })
        .map(|m| m.sender_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .take(50)
        .collect();
    if !unresolved_senders.is_empty() {
        if let Ok(mp) = threads::get_mini_profiles(sess, &unresolved_senders).await {
            for (uid, p) in mp {
                if let Some(name) =
                    pick_str(&p, &["displayName", "zaloName", "name", "display_name"])
                {
                    known_titles.insert(uid, name);
                }
            }
        }
    }

    let mut store = store_arc.lock().await;
    for m in remote_msgs.iter_mut() {
        if !m.is_outgoing && (m.sender_name == m.sender_id || is_raw_numeric_id(&m.sender_name)) {
            if let Some(name) = known_titles.get(&m.sender_id) {
                m.sender_name = name.clone();
            }
        }
        let _ = store.upsert_message(m);
    }
    if let Some(max_out_ts) = remote_msgs.iter().filter(|m| m.is_outgoing).map(|m| m.ts).max() {
        if max_out_ts > 0 {
            let _ = store.advance_last_read_in_ts(thread_id, max_out_ts);
        }
    }
    if update_preview {
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
            let created = store
                .update_thread_preview_returning_created(
                    thread_id,
                    &preview,
                    newest.ts,
                    false,
                    newest.is_outgoing,
                )
                .unwrap_or(false);
            if created {
                let _ = app.emit("core://threads-updated", ());
            }
        }
    }
}

async fn sync_remote_thread_history(ctx: &RemoteThreadSyncCtx) -> Option<bool> {
    match threads::fetch_thread_history_page(&ctx.sess, &ctx.thread_id, ctx.kind, ctx.limit as u32)
        .await
    {
        Ok(mut page) => {
            if !page.messages.is_empty() {
                resolve_and_upsert_remote_messages(
                    &ctx.app,
                    &ctx.store,
                    &ctx.sess,
                    &ctx.thread_id,
                    ctx.kind,
                    &mut page.messages,
                    true,
                )
                .await;
            }
            Some(page.has_more)
        }
        Err(e) => {
            if let Some(code) = expired_code(&e) {
                let reason = format!(
                    "Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR."
                );
                clear_expired_session(
                    &ctx.app,
                    &ctx.session,
                    Some(&ctx.ws_task),
                    &ctx.paths,
                    reason,
                    true,
                )
                .await;
            }
            None
        }
    }
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
            .load_messages_before(&thread_id, before_ts, before_msg_id.as_deref(), limit + 1)
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
    let is_authed = sess_snapshot.is_some();
    let mut remote_has_more: Option<bool> = None;

    if let Some(sess) = sess_snapshot {
        let kind = {
            let store = state.store.lock().await;
            store
                .get_thread_kind(&thread_id)
                .ok()
                .flatten()
                .unwrap_or(ThreadKind::Direct)
        };

        if before_ts.is_none() && cached.is_empty() {
            let ctx = RemoteThreadSyncCtx {
                app: app.clone(),
                store: Arc::clone(&state.store),
                session: Arc::clone(&state.session),
                ws_task: Arc::clone(&state.ws_task),
                paths: state.paths.clone(),
                sess,
                thread_id: thread_id.clone(),
                kind,
                limit,
            };
            remote_has_more = sync_remote_thread_history(&ctx).await;
            if remote_has_more.is_some() {
                let store = state.store.lock().await;
                cached = store
                    .load_messages_before(&thread_id, None, None, limit + 1)
                    .map_err(|e| e.to_string())?;
            }
        } else if before_ts.is_none() && sync_remote == Some(true) {
            let prev_has_more = cached.len() > limit || (is_authed && !cached.is_empty());
            let mut snapshot = cached.clone();
            if snapshot.len() > limit {
                let excess = snapshot.len() - limit;
                snapshot.drain(0..excess);
            }
            let prev_sigs: Vec<_> = snapshot.iter().map(message_sync_sig).collect();
            let ctx = RemoteThreadSyncCtx {
                app: app.clone(),
                store: Arc::clone(&state.store),
                session: Arc::clone(&state.session),
                ws_task: Arc::clone(&state.ws_task),
                paths: state.paths.clone(),
                sess,
                thread_id: thread_id.clone(),
                kind,
                limit,
            };
            tokio::spawn(async move {
                if let Some(rm_has_more) = sync_remote_thread_history(&ctx).await {
                    let latest = {
                        let store = ctx.store.lock().await;
                        store
                            .load_messages_before(&ctx.thread_id, None, None, ctx.limit + 1)
                            .ok()
                    };
                    if let Some(mut msgs) = latest {
                        let synced_has_more = msgs.len() > ctx.limit || rm_has_more;
                        if msgs.len() > ctx.limit {
                            let excess = msgs.len() - ctx.limit;
                            msgs.drain(0..excess);
                        }
                        let next_sigs: Vec<_> = msgs.iter().map(message_sync_sig).collect();
                        if next_sigs != prev_sigs || synced_has_more != prev_has_more {
                            let _ = ctx.app.emit(
                                "core://thread-messages-synced",
                                ThreadMessagesSyncedPayload {
                                    thread_id: ctx.thread_id.clone(),
                                    messages: msgs,
                                    has_more: synced_has_more,
                                },
                            );
                        }
                    }
                }
            });
        } else if before_ts.is_some() && cached.len() <= limit {
            let cursor_id = cached
                .iter()
                .find(|m| is_server_msg_id(&m.msg_id))
                .map(|m| m.msg_id.clone())
                .or_else(|| {
                    before_msg_id
                        .as_deref()
                        .filter(|s| is_server_msg_id(s))
                        .map(str::to_string)
                })
                .or_else(|| {
                    // Fallback when oldest visible bubble in UI has a local synthetic ID
                    let store = state.store.try_lock().ok()?;
                    store
                        .oldest_server_msg_id(&thread_id)
                        .ok()
                        .flatten()
                        .map(|(_, id)| id)
                });
            let cursor_id = if cursor_id.is_some() {
                cursor_id
            } else {
                let store = state.store.lock().await;
                store
                    .oldest_server_msg_id(&thread_id)
                    .ok()
                    .flatten()
                    .map(|(_, id)| id)
            };
            if let Some(cursor_id) = cursor_id {
                match threads::fetch_older_thread_history_page(
                    &sess,
                    &thread_id,
                    kind,
                    &cursor_id,
                    limit as u32,
                )
                .await
                {
                    Ok(mut page) => {
                        remote_has_more = Some(page.has_more);
                        if !page.messages.is_empty() {
                            resolve_and_upsert_remote_messages(
                                &app,
                                &state.store,
                                &sess,
                                &thread_id,
                                kind,
                                &mut page.messages,
                                false,
                            )
                            .await;
                            let store = state.store.lock().await;
                            cached = store
                                .load_messages_before(
                                    &thread_id,
                                    before_ts,
                                    before_msg_id.as_deref(),
                                    limit + 1,
                                )
                                .map_err(|e| e.to_string())?;
                        }
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
                }
            }
        }
    }

    let local_has_more = cached.len() > limit;
    if local_has_more {
        let excess = cached.len() - limit;
        cached.drain(0..excess);
    }
    let has_more = if local_has_more {
        true
    } else if before_ts.is_some() {
        remote_has_more.unwrap_or(false)
    } else {
        remote_has_more.unwrap_or(is_authed && !cached.is_empty())
    };
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
async fn messages_list_pinned(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<MessageDto>, String> {
    let (kind, known_titles) = {
        let store = state.store.lock().await;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let titles = store.resolve_known_titles().unwrap_or_default();
        (kind, titles)
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
        if let Ok(remote_pins) =
            threads::fetch_pinned_messages(&sess, &thread_id, kind, &known_titles).await
        {
            if !remote_pins.is_empty() {
                let mut store = state.store.lock().await;
                for mut pm in remote_pins {
                    if store.get_message(&thread_id, &pm.msg_id).ok().flatten().is_some() {
                        let _ = store.set_message_pinned(&thread_id, &pm.msg_id, true);
                    } else if !pm.text.is_empty() || pm.attachment_url.is_some() {
                        pm.is_pinned = true;
                        let _ = store.upsert_message(&pm);
                    }
                }
            }
        }
    }
    let store = state.store.lock().await;
    store
        .list_pinned_messages(&thread_id, 50)
        .map_err(|e| e.to_string())
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

    persist_and_emit_sent_message(&app, &state, msg).await
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

    persist_and_emit_sent_message(&app, &state, msg).await
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

#[tauri::command]
async fn message_toggle_pin(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
    pinned: bool,
) -> Result<MessageDto, String> {
    let store = state.store.lock().await;
    let updated = store
        .set_message_pinned(&thread_id, &msg_id, pinned)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Không tìm thấy tin nhắn để ghim".to_string())?;
    let _ = app.emit("core://message-updated", &updated);
    Ok(updated)
}

#[tauri::command]
async fn stickers_list(state: State<'_, AppState>) -> Result<Vec<messaging::StickerItem>, String> {
    let recent: Vec<messaging::StickerItem> = {
        let store = state.store.lock().await;
        store
            .list_recent_stickers(40)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, cate_id, url)| messaging::StickerItem {
                id,
                cate_id,
                sticker_type: 1,
                url,
                category: "Gần đây".into(),
            })
            .collect()
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    messaging::list_stickers(&sess_snapshot, recent)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn stickers_search(
    state: State<'_, AppState>,
    keyword: String,
) -> Result<Vec<messaging::StickerItem>, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    messaging::search_stickers(&sess_snapshot, &keyword)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn message_send_sticker(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    sticker_id: u64,
    cate_id: u64,
    sticker_type: u64,
) -> Result<MessageDto, String> {
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
        match messaging::send_sticker(
            &sess_snapshot,
            &thread_id,
            kind,
            sticker_id,
            cate_id,
            sticker_type,
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
                return Err(format!("Gửi nhãn dán thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        MessageDto {
            msg_id: format!("local-{now_ms}"),
            thread_id: thread_id.clone(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "[Nhãn dán]".into(),
            msg_type: "chat.sticker".into(),
            ts: now_ms,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: sticker_image_url(sticker_id, cate_id),
            attachment_name: Some(format!("sticker_{cate_id}_{sticker_id}.png")),
        }
    };

    persist_and_emit_sent_message(&app, &state, msg).await
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
    let created = store
        .update_thread_preview_returning_created(&msg.thread_id, &preview, msg.ts, false, true)
        .unwrap_or(false);
    let _ = app.emit("core://message-new", &msg);
    if created {
        let _ = app.emit("core://threads-updated", ());
    }
    Ok(msg)
}

#[tauri::command]
async fn pick_attachment_file(app: AppHandle) -> Result<Option<AttachmentMetaDto>, String> {
    let owner_hwnd = main_window_hwnd(&app);

    let selected = tokio::task::spawn_blocking(move || file_dialog::pick_single_file(owner_hwnd))
        .await
        .map_err(|e| format!("Lỗi tác vụ chọn tệp: {e}"))??;
    selected.map(|p| attachment_meta_from_path(&p)).transpose()
}

fn main_window_hwnd(app: &AppHandle) -> Option<usize> {
    #[cfg(target_os = "windows")]
    {
        app.get_webview_window("main")
            .and_then(|w| w.hwnd().ok())
            .map(|h| h.0 as usize)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        None
    }
}

const MAX_RECENT_SAVED_FILES: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgressPayload {
    pub id: String,
    pub file_name: String,
    pub stage: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub saved_path: Option<String>,
    pub can_open_directly: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedFileResult {
    pub path: String,
    pub file_name: String,
    pub can_open_directly: bool,
}

fn emit_download_progress(app: &AppHandle, payload: DownloadProgressPayload) {
    let _ = app.emit("core://download-progress", payload);
}

fn normalize_saved_lookup_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn record_recent_saved_file(list: &mut VecDeque<PathBuf>, path: &Path) -> PathBuf {
    let canonical = normalize_saved_lookup_path(path);
    if let Some(pos) = list.iter().position(|p| p == &canonical) {
        list.remove(pos);
    }
    list.push_back(canonical.clone());
    while list.len() > MAX_RECENT_SAVED_FILES {
        list.pop_front();
    }
    canonical
}

fn is_recent_saved_file(list: &VecDeque<PathBuf>, candidate: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(candidate).ok()?;
    list.iter().find(|p| *p == &canonical).cloned()
}

fn validate_open_saved_file(list: &VecDeque<PathBuf>, candidate: &Path) -> Result<PathBuf, String> {
    let canonical = is_recent_saved_file(list, candidate)
        .ok_or_else(|| "Tệp không nằm trong danh sách tệp vừa tải trong phiên này".to_string())?;
    if file_dialog::is_dangerous_executable_ext(&canonical) {
        return Err(
            "Vì lý do an toàn, không mở trực tiếp tệp thực thi. Hãy mở thư mục chứa tệp để kiểm tra."
                .into(),
        );
    }
    Ok(canonical)
}

fn validate_reveal_saved_file(list: &VecDeque<PathBuf>, candidate: &Path) -> Result<PathBuf, String> {
    is_recent_saved_file(list, candidate)
        .ok_or_else(|| "Tệp không nằm trong danh sách tệp vừa tải trong phiên này".to_string())
}

#[tauri::command]
async fn save_remote_file(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    suggested_name: String,
    download_id: Option<String>,
) -> Result<Option<SavedFileResult>, String> {
    let dl_id = download_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("dl-{}", chrono_now_ms()));
    let initial_name = {
        let s = suggested_name.trim();
        if s.is_empty() {
            "download".to_string()
        } else {
            s.to_string()
        }
    };

    emit_download_progress(
        &app,
        DownloadProgressPayload {
            id: dl_id.clone(),
            file_name: initial_name.clone(),
            stage: "downloading".to_string(),
            downloaded_bytes: 0,
            total_bytes: None,
            saved_path: None,
            can_open_directly: false,
            error: None,
        },
    );

    let trimmed = url.trim_start();
    let is_data_url = trimmed.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:"));
    let (bytes, mime, final_url, cd_name) = if is_data_url {
        let (data, mime) = match zalo_core::session::decode_data_url_bytes(trimmed, 25 * 1024 * 1024)
        {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                emit_download_progress(
                    &app,
                    DownloadProgressPayload {
                        id: dl_id,
                        file_name: initial_name,
                        stage: "error".to_string(),
                        downloaded_bytes: 0,
                        total_bytes: None,
                        saved_path: None,
                        can_open_directly: false,
                        error: Some(msg.clone()),
                    },
                );
                return Err(msg);
            }
        };
        let total = data.len() as u64;
        emit_download_progress(
            &app,
            DownloadProgressPayload {
                id: dl_id.clone(),
                file_name: initial_name.clone(),
                stage: "downloading".to_string(),
                downloaded_bytes: total,
                total_bytes: Some(total),
                saved_path: None,
                can_open_directly: false,
                error: None,
            },
        );
        (data, mime, String::new(), None)
    } else {
        let sess_snapshot = {
            let sess = state.session.lock().await;
            sess.clone_shallow()
        };
        let app_prog = app.clone();
        let dl_id_prog = dl_id.clone();
        let initial_name_prog = initial_name.clone();
        let downloaded = match sess_snapshot
            .download_media_with_meta_progress(&url, 100 * 1024 * 1024, move |bytes, total| {
                emit_download_progress(
                    &app_prog,
                    DownloadProgressPayload {
                        id: dl_id_prog.clone(),
                        file_name: initial_name_prog.clone(),
                        stage: "downloading".to_string(),
                        downloaded_bytes: bytes,
                        total_bytes: total,
                        saved_path: None,
                        can_open_directly: false,
                        error: None,
                    },
                );
            })
            .await
        {
            Ok(d) => d,
            Err(e) => {
                let msg = e.to_string();
                emit_download_progress(
                    &app,
                    DownloadProgressPayload {
                        id: dl_id,
                        file_name: initial_name,
                        stage: "error".to_string(),
                        downloaded_bytes: 0,
                        total_bytes: None,
                        saved_path: None,
                        can_open_directly: false,
                        error: Some(msg.clone()),
                    },
                );
                return Err(msg);
            }
        };
        (
            downloaded.bytes,
            downloaded.mime,
            downloaded.final_url,
            downloaded.content_disposition_filename,
        )
    };

    let bytes_len = bytes.len() as u64;
    let source_url = if is_data_url { "" } else { url.as_str() };
    let (resolved_name, resolved_ext) = file_dialog::resolve_download_filename_and_ext(
        &suggested_name,
        cd_name.as_deref(),
        source_url,
        &final_url,
        &mime,
        &bytes,
    );

    emit_download_progress(
        &app,
        DownloadProgressPayload {
            id: dl_id.clone(),
            file_name: resolved_name.clone(),
            stage: "saving".to_string(),
            downloaded_bytes: bytes_len,
            total_bytes: Some(bytes_len),
            saved_path: None,
            can_open_directly: false,
            error: None,
        },
    );

    let owner_hwnd = main_window_hwnd(&app);
    let resolved_name_for_dlg = resolved_name.clone();
    let target = match tokio::task::spawn_blocking(move || {
        file_dialog::pick_save_file(owner_hwnd, &resolved_name_for_dlg, resolved_ext.as_deref())
    })
    .await
    {
        Ok(Ok(t)) => t,
        Ok(Err(msg)) => {
            emit_download_progress(
                &app,
                DownloadProgressPayload {
                    id: dl_id,
                    file_name: resolved_name,
                    stage: "error".to_string(),
                    downloaded_bytes: bytes_len,
                    total_bytes: Some(bytes_len),
                    saved_path: None,
                    can_open_directly: false,
                    error: Some(msg.clone()),
                },
            );
            return Err(msg);
        }
        Err(e) => {
            let msg = format!("Lỗi hộp thoại lưu tệp: {e}");
            emit_download_progress(
                &app,
                DownloadProgressPayload {
                    id: dl_id,
                    file_name: resolved_name,
                    stage: "error".to_string(),
                    downloaded_bytes: bytes_len,
                    total_bytes: Some(bytes_len),
                    saved_path: None,
                    can_open_directly: false,
                    error: Some(msg.clone()),
                },
            );
            return Err(msg);
        }
    };
    let Some(path) = target else {
        emit_download_progress(
            &app,
            DownloadProgressPayload {
                id: dl_id,
                file_name: resolved_name,
                stage: "cancelled".to_string(),
                downloaded_bytes: bytes_len,
                total_bytes: Some(bytes_len),
                saved_path: None,
                can_open_directly: false,
                error: None,
            },
        );
        return Ok(None);
    };
    if let Err(e) = tokio::fs::write(&path, &bytes).await {
        let msg = format!("Không ghi được tệp: {e}");
        emit_download_progress(
            &app,
            DownloadProgressPayload {
                id: dl_id,
                file_name: resolved_name,
                stage: "error".to_string(),
                downloaded_bytes: bytes_len,
                total_bytes: Some(bytes_len),
                saved_path: None,
                can_open_directly: false,
                error: Some(msg.clone()),
            },
        );
        return Err(msg);
    }

    {
        let mut recent = state.recent_saved_files.lock().await;
        record_recent_saved_file(&mut recent, &path);
    }

    let display_path = file_dialog::normalize_os_display_path(&path);
    let path_str = display_path.to_string_lossy().into_owned();
    let saved_name = display_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&resolved_name)
        .to_string();
    let can_open_directly = !file_dialog::is_dangerous_executable_ext(&display_path);

    emit_download_progress(
        &app,
        DownloadProgressPayload {
            id: dl_id,
            file_name: saved_name.clone(),
            stage: "done".to_string(),
            downloaded_bytes: bytes_len,
            total_bytes: Some(bytes_len),
            saved_path: Some(path_str.clone()),
            can_open_directly,
            error: None,
        },
    );

    Ok(Some(SavedFileResult {
        path: path_str,
        file_name: saved_name,
        can_open_directly,
    }))
}

#[tauri::command]
async fn open_saved_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Đường dẫn tệp trống".into());
    }
    let canonical = {
        let recent = state.recent_saved_files.lock().await;
        validate_open_saved_file(&recent, Path::new(trimmed))?
    };
    tokio::task::spawn_blocking(move || file_dialog::open_local_file(&canonical))
        .await
        .map_err(|e| format!("Không thể mở tệp: {e}"))?
}

#[tauri::command]
async fn reveal_saved_file(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Đường dẫn tệp trống".into());
    }
    let canonical = {
        let recent = state.recent_saved_files.lock().await;
        validate_reveal_saved_file(&recent, Path::new(trimmed))?
    };
    tokio::task::spawn_blocking(move || file_dialog::reveal_local_file(&canonical))
        .await
        .map_err(|e| format!("Không thể mở thư mục chứa tệp: {e}"))?
}

#[tauri::command]
async fn fetch_image_data_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<String, String> {
    let trimmed = url.trim_start();
    if trimmed.get(..11).is_some_and(|p| {
        p.eq_ignore_ascii_case("data:image/")
            || p.eq_ignore_ascii_case("data:audio/")
            || p.eq_ignore_ascii_case("data:video/")
    }) {
        return Ok(trimmed.to_string());
    }
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    sess_snapshot
        .download_media_data_url(&url, 25 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())
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

    let bytes = extract_invoke_body_bytes(request.body(), messaging::MAX_ATTACHMENT_BYTES as usize)?;
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
            &bytes,
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
    force_refresh: Option<bool>,
) -> Result<Vec<TranslationResult>, String> {
    let cfg = state.translate_cfg.lock().await.clone();
    let relay = state.relay_cfg.lock().await.clone();
    let eff_provider = provider.unwrap_or(cfg.provider);
    let eff_target = target_lang
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(cfg.target_lang.as_str())
        .to_ascii_uppercase();
    let target_base = normalize_lang_base_code(&eff_target);
    let force = force_refresh.unwrap_or(false);

    let mut results: Vec<TranslationResult> = Vec::with_capacity(items.len());
    let mut check_cache: Vec<TranslateInputItem> = Vec::new();
    for it in items {
        let skip = if force {
            is_static_non_translatable(&it.text)
        } else {
            classify_skip_reason_for_mode(&it.text, &eff_target, TranslateDetectMode::ApiCache)
                == Some("non_linguistic")
        };
        if skip {
            results.push(TranslationResult {
                msg_id: it.msg_id,
                translated_text: it.text,
                detected_source_lang: None,
                skipped: true,
            });
        } else {
            check_cache.push(it);
        }
    }

    if check_cache.is_empty() {
        return Ok(results);
    }

    let after_cache: Vec<TranslateInputItem> = if !force {
        let pairs: Vec<(String, String)> = check_cache
            .iter()
            .map(|it| (it.msg_id.clone(), it.text.clone()))
            .collect();
        let cached = {
            let store = state.store.lock().await;
            store
                .get_cached_translations(&pairs, &eff_target)
                .unwrap_or_default()
        };
        let mut misses = Vec::new();
        for it in check_cache {
            if let Some(hit) = cached.get(&it.msg_id) {
                results.push(hit.clone());
            } else {
                misses.push(it);
            }
        }
        misses
    } else {
        check_cache
    };

    if after_cache.is_empty() {
        return Ok(results);
    }

    let api_items: Vec<TranslateInputItem> =
        if !force && cfg.detect_mode == TranslateDetectMode::LocalSmartSkip {
            let mut rest = Vec::new();
            for it in after_cache {
                if classify_skip_reason(&it.text, &eff_target) == Some("same_language") {
                    results.push(TranslationResult {
                        msg_id: it.msg_id,
                        translated_text: it.text,
                        detected_source_lang: Some(target_base.clone()),
                        skipped: true,
                    });
                } else {
                    rest.push(it);
                }
            }
            rest
        } else {
            after_cache
        };

    if api_items.is_empty() {
        return Ok(results);
    }

    let orig_by_id: std::collections::HashMap<String, String> = api_items
        .iter()
        .map(|it| (it.msg_id.clone(), it.text.clone()))
        .collect();
    let api_res = translate_api_only_with_relay(
        &cfg,
        &relay,
        &api_items,
        Some(&eff_target),
        Some(eff_provider),
    )
    .await
    .map_err(|e| e.to_string())?;

    let to_persist: Vec<(String, String, TranslationResult)> = api_res
        .iter()
        .filter_map(|tr| {
            if tr.translated_text.trim().is_empty() && !tr.skipped {
                return None;
            }
            orig_by_id
                .get(&tr.msg_id)
                .map(|orig| (tr.msg_id.clone(), orig.clone(), tr.clone()))
        })
        .collect();
    if !to_persist.is_empty() {
        let mut store = state.store.lock().await;
        let _ = store.upsert_cached_translations(
            &to_persist,
            &eff_target,
            provider_name(eff_provider),
        );
    }

    results.extend(api_res);
    Ok(results)
}

#[tauri::command]
async fn translate_clear_cache(
    state: State<'_, AppState>,
    msg_ids: Option<Vec<String>>,
    target_lang: Option<String>,
) -> Result<usize, String> {
    let store = state.store.lock().await;
    store
        .clear_cached_translations(msg_ids.as_deref(), target_lang.as_deref())
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize)]
struct RelayTestDto {
    latency_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct AppUpdateDto {
    version: String,
    current_version: String,
    body: Option<String>,
    date: Option<String>,
}

#[tauri::command]
async fn relay_get_config(state: State<'_, AppState>) -> Result<RelayConfigSummary, String> {
    let cfg = state.relay_cfg.lock().await;
    Ok(cfg.summary())
}

#[tauri::command]
async fn relay_save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    update: RelayConfigUpdate,
) -> Result<RelayConfigSummary, String> {
    let next_cfg = {
        let cfg = state.relay_cfg.lock().await;
        cfg.with_update(update).map_err(|e| e.to_string())?
    };
    next_cfg
        .save_to_file(&state.paths.relay_file())
        .map_err(|e| format!("Không lưu được cấu hình Relay: {e}"))?;

    let is_authed = {
        let mut sess = state.session.lock().await;
        sess.set_relay(next_cfg.clone())
            .map_err(|e| format!("Không áp dụng được Relay cho HTTP client: {e}"))?;
        sess.is_authenticated()
    };

    let summary = {
        let mut cfg = state.relay_cfg.lock().await;
        *cfg = next_cfg;
        cfg.summary()
    };

    if is_authed {
        restart_ws_worker(
            app.clone(),
            state.session.clone(),
            state.store.clone(),
            state.ws_task.clone(),
            state.active_thread_id.clone(),
            state.paths.clone(),
        )
        .await;
    }
    let _ = app.emit("core://relay-updated", &summary);
    Ok(summary)
}

#[tauri::command]
async fn relay_test_connection(
    state: State<'_, AppState>,
    update: Option<RelayConfigUpdate>,
) -> Result<RelayTestDto, String> {
    let candidate = {
        let cfg = state.relay_cfg.lock().await;
        match update {
            Some(upd) => cfg.with_update(upd).map_err(|e| e.to_string())?,
            None => cfg.clone(),
        }
    };
    let latency_ms = candidate.test_connection().await.map_err(|e| e.to_string())?;
    Ok(RelayTestDto { latency_ms })
}

#[tauri::command]
async fn app_check_update(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<AppUpdateDto>, String> {
    let relay_cfg = state.relay_cfg.lock().await.clone();
    let mut builder = app
        .updater_builder()
        .timeout(std::time::Duration::from_secs(20));

    if relay_cfg.enabled {
        let mut http_relay = relay_cfg;
        http_relay.mode = RelayMode::Http;
        if let Some(proxy_str) = http_relay.proxy_url_string().map_err(|e| e.to_string())? {
            let proxy_url = tauri::Url::parse(&proxy_str).map_err(|e| e.to_string())?;
            builder = builder.proxy(proxy_url);
        }
    }

    let updater = builder.build().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    let mut pending = state.pending_update.lock().await;
    if let Some(upd) = found {
        let dto = AppUpdateDto {
            version: upd.version.clone(),
            current_version: upd.current_version.clone(),
            body: upd.body.clone(),
            date: upd.date.map(|d| d.to_string()),
        };
        *pending = Some(upd);
        Ok(Some(dto))
    } else {
        *pending = None;
        Ok(None)
    }
}

#[tauri::command]
async fn app_install_update(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let upd = state
        .pending_update
        .lock()
        .await
        .take()
        .ok_or_else(|| "Không có bản cập nhật nào đang chờ cài đặt".to_string())?;

    let app_c = app.clone();
    let app_d = app.clone();
    let mut started = false;
    upd.download_and_install(
        move |chunk_len, content_len| {
            if !started {
                started = true;
                let _ = app_c.emit(
                    "core://update-progress",
                    serde_json::json!({ "event": "started", "total": content_len }),
                );
            }
            let _ = app_c.emit(
                "core://update-progress",
                serde_json::json!({ "event": "progress", "chunk": chunk_len }),
            );
        },
        move || {
            let _ = app_d.emit(
                "core://update-progress",
                serde_json::json!({ "event": "finished" }),
            );
        },
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn app_set_close_to_tray(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state.close_to_tray.store(enabled, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
async fn clipboard_copy_text(text: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || clipboard::copy_text_native(&text))
        .await
        .map_err(|e| format!("Lỗi tác vụ clipboard: {e}"))?
}

fn extract_invoke_body_bytes<'a>(
    body: &'a InvokeBody,
    max_bytes: usize,
) -> Result<std::borrow::Cow<'a, [u8]>, String> {
    match body {
        InvokeBody::Raw(raw) => {
            if raw.len() > max_bytes {
                return Err("Dữ liệu vượt quá giới hạn kích thước cho phép".into());
            }
            Ok(std::borrow::Cow::Borrowed(raw.as_slice()))
        }
        InvokeBody::Json(serde_json::Value::Array(arr)) => {
            if arr.len() > max_bytes {
                return Err("Dữ liệu vượt quá giới hạn kích thước cho phép".into());
            }
            let mut bytes = Vec::with_capacity(arr.len());
            for elem in arr {
                let b = elem
                    .as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| "Dữ liệu nhị phân IPC không hợp lệ".to_string())?;
                bytes.push(b);
            }
            Ok(std::borrow::Cow::Owned(bytes))
        }
        InvokeBody::Json(_) => Err("Dữ liệu nhị phân IPC không hợp lệ".into()),
    }
}

#[tauri::command]
async fn clipboard_copy_rgba_image(request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let width = request
        .headers()
        .get("x-width")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| "Thiếu chiều rộng ảnh để sao chép".to_string())?;
    let height = request
        .headers()
        .get("x-height")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| "Thiếu chiều cao ảnh để sao chép".to_string())?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .filter(|&n| n <= 128 * 1024 * 1024)
        .ok_or_else(|| "Kích thước ảnh vượt giới hạn".to_string())?;
    let bytes = extract_invoke_body_bytes(request.body(), expected)?.into_owned();
    tokio::task::spawn_blocking(move || clipboard::copy_rgba_image_native(width, height, &bytes))
        .await
        .map_err(|e| format!("Lỗi tác vụ clipboard ảnh: {e}"))?
}

#[tauri::command]
async fn capture_screen(
    app: AppHandle,
    hide_window: Option<bool>,
    monitor_index: Option<usize>,
    capture_all: Option<bool>,
) -> Result<screenshot::ScreenCaptureDto, String> {
    let owner_hwnd = main_window_hwnd(&app);
    let main_win = app.get_webview_window("main");
    let mut hid_window = false;

    if hide_window.unwrap_or(true) {
        if let Some(ref win) = main_win {
            if win.is_visible().unwrap_or(false) {
                hide_all_webapp_children(&app);
                if win.hide().is_ok() {
                    hid_window = true;
                    tokio::time::sleep(std::time::Duration::from_millis(240)).await;
                }
            }
        }
    }

    let res = tokio::task::spawn_blocking(move || {
        screenshot::capture_screen(owner_hwnd, monitor_index, capture_all.unwrap_or(false))
    })
    .await
    .map_err(|e| format!("Lỗi tác vụ chụp màn hình: {e}"))
    .and_then(|inner| inner);

    if hid_window {
        if let Some(ref win) = main_win {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
    }

    res
}

const WEBAPP_LABEL_PREFIX: &str = "teza-webapp-";

fn webapp_label(id: &str) -> String {
    let clean: String = id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = clean.trim_matches('-');
    if trimmed.is_empty() {
        format!("{WEBAPP_LABEL_PREFIX}default")
    } else {
        format!("{WEBAPP_LABEL_PREFIX}{trimmed}")
    }
}

fn validate_web_url(raw: &str) -> Result<tauri::Url, String> {
    let trimmed = raw.trim();
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed =
        tauri::Url::parse(&with_scheme).map_err(|e| format!("URL không hợp lệ: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("Chỉ hỗ trợ giao thức http/https (nhận: {other})")),
    }
    if parsed.host_str().map(str::is_empty).unwrap_or(true) {
        return Err("URL phải có tên miền hợp lệ".into());
    }
    Ok(parsed)
}

fn hide_all_webapp_children(app: &AppHandle) {
    for (label, wv) in app.webviews() {
        if label.starts_with(WEBAPP_LABEL_PREFIX) {
            let _ = wv.hide();
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn webapp_open(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let parsed = validate_web_url(&url)?;
    let label = webapp_label(&id);
    let safe_w = width.max(120.0);
    let safe_h = height.max(120.0);

    for (existing_label, wv) in app.webviews() {
        if existing_label.starts_with(WEBAPP_LABEL_PREFIX) && existing_label != label {
            let _ = wv.hide();
        }
    }

    if let Some(wv) = app.get_webview(&label) {
        wv.set_position(LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        wv.set_size(LogicalSize::new(safe_w, safe_h))
            .map_err(|e| e.to_string())?;
        wv.show().map_err(|e| e.to_string())?;
        *state.active_webapp_label.lock().await = Some(label);
        return Ok(());
    }

    let main_win = app
        .get_webview_window("main")
        .ok_or_else(|| "Không tìm thấy cửa sổ chính".to_string())?;
    let parent_window = main_win.as_ref().window();
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed));
    parent_window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(safe_w, safe_h),
        )
        .map_err(|e| format!("Không thể nhúng Web App: {e}"))?;

    *state.active_webapp_label.lock().await = Some(label);
    Ok(())
}

#[tauri::command]
async fn webapp_set_bounds(
    app: AppHandle,
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let label = webapp_label(&id);
    if let Some(wv) = app.get_webview(&label) {
        let safe_w = width.max(120.0);
        let safe_h = height.max(120.0);
        wv.set_position(LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        wv.set_size(LogicalSize::new(safe_w, safe_h))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn webapp_hide_all(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    hide_all_webapp_children(&app);
    *state.active_webapp_label.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn webapp_close(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let label = webapp_label(&id);
    if let Some(wv) = app.get_webview(&label) {
        wv.close().map_err(|e| e.to_string())?;
    }
    let mut active = state.active_webapp_label.lock().await;
    if active.as_deref() == Some(label.as_str()) {
        *active = None;
    }
    Ok(())
}

#[tauri::command]
async fn webapp_navigate(
    app: AppHandle,
    id: String,
    action: String,
    url: Option<String>,
) -> Result<(), String> {
    let label = webapp_label(&id);
    let Some(wv) = app.get_webview(&label) else {
        return Ok(());
    };
    match action.as_str() {
        "back" => {
            let _ = wv.eval("window.history.back()");
        }
        "forward" => {
            let _ = wv.eval("window.history.forward()");
        }
        "reload" => {
            wv.reload().map_err(|e| e.to_string())?;
        }
        "home" => {
            if let Some(target_url) = url {
                let parsed = validate_web_url(&target_url)?;
                wv.navigate(parsed).map_err(|e| e.to_string())?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    let parsed = validate_web_url(&url)?;
    let target = parsed.as_str().to_string();
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &target])
            .spawn()
            .map_err(|e| format!("Không mở được trình duyệt: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("Không mở được trình duyệt: {e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .map_err(|e| format!("Không mở được trình duyệt: {e}"))?;
    }
    Ok(())
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn run() {
    let paths = StoragePaths::resolve_default(None).expect("Failed to resolve storage directory");
    let relay_cfg = RelayConfig::load_from_file(&paths.relay_file());
    let session_path = paths.session_file();
    let session = Session::load_from_file_with_relay(&session_path, relay_cfg.clone())
        .unwrap_or_else(|_| {
            Session::new_with_relay(relay_cfg.clone()).expect("Failed to initialize session")
        });
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

    let close_to_tray = Arc::new(AtomicBool::new(true));
    let allow_exit = Arc::new(AtomicBool::new(false));
    let state = AppState {
        paths,
        session: sess_arc,
        store: store_arc,
        ws_task: ws_task_arc,
        qr_task: qr_task_arc,
        active_thread_id: Arc::new(Mutex::new(None)),
        translate_cfg: Arc::new(Mutex::new(translate_cfg)),
        relay_cfg: Arc::new(Mutex::new(relay_cfg)),
        pending_update: Arc::new(Mutex::new(None)),
        close_to_tray: close_to_tray.clone(),
        allow_exit: allow_exit.clone(),
        pending_notify: Arc::new(Mutex::new(None)),
        notify_hide_gen: Arc::new(AtomicU64::new(0)),
        active_webapp_label: Arc::new(Mutex::new(None)),
        recent_saved_files: Arc::new(Mutex::new(VecDeque::new())),
    };

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }));

    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            auth_get_status,
            auth_start_qr,
            auth_logout,
            threads_sync,
            threads_list,
            thread_set_active,
            thread_mark_read,
            thread_mark_unread,
            thread_toggle_pin,
            thread_toggle_mute,
            thread_toggle_archive,
            thread_get_details,
            folders_list,
            folder_save,
            folder_delete,
            folders_reorder,
            folders_sync_remote_labels,
            user_get_profile,
            user_find_by_phone,
            group_add_members,
            reminder_create,
            poll_create,
            poll_get_detail,
            poll_vote,
            messages_load,
            messages_load_around,
            messages_list_pinned,
            message_get_by_id,
            message_send_text,
            message_send_quote,
            message_react,
            message_recall,
            message_delete_local,
            message_toggle_pin,
            stickers_list,
            stickers_search,
            message_send_sticker,
            pick_attachment_file,
            save_remote_file,
            open_saved_file,
            reveal_saved_file,
            fetch_image_data_url,
            inspect_attachment_path,
            message_send_attachment_path,
            message_send_raw_attachment,
            search_global,
            translate_get_config,
            translate_save_config,
            translate_messages,
            translate_clear_cache,
            relay_get_config,
            relay_save_config,
            relay_test_connection,
            notify_show_popup,
            notify_get_current,
            notify_dismiss,
            notify_click,
            buzz_shake_screen,
            app_check_update,
            app_install_update,
            app_set_close_to_tray,
            clipboard_copy_text,
            clipboard_copy_rgba_image,
            capture_screen,
            webapp_open,
            webapp_set_bounds,
            webapp_hide_all,
            webapp_close,
            webapp_navigate,
            open_external_url,
        ])
        .setup(move |app| {
            let _ = app.get_webview_window("main");
            let open_item = MenuItem::with_id(app, "open", "Mở Teza", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Thoát Teza", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &quit_item])?;
            let exit_flag = allow_exit.clone();
            let mut tray = TrayIconBuilder::new()
                .tooltip("Teza")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app_handle, event| {
                    if event.id() == "open" {
                        if let Some(w) = app_handle.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    } else if event.id() == "quit" {
                        exit_flag.store(true, Ordering::SeqCst);
                        app_handle.exit(0);
                    }
                })
                .on_tray_icon_event(|tray_icon, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray_icon.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                tray = tray.icon(icon);
            }
            let _ = tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let st = window.state::<AppState>();
                    if st.close_to_tray.load(Ordering::SeqCst)
                        && !st.allow_exit.load(Ordering::SeqCst)
                    {
                        api.prevent_close();
                        hide_all_webapp_children(window.app_handle());
                        let _ = window.hide();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Teza");
}

#[cfg(test)]
mod tests {
    use super::{
        compute_notify_position, compute_notify_position_for_size, compute_notify_shake_offsets,
        compute_toast_logical_size,
    };

    #[test]
    fn compute_notify_position_places_all_four_corners_with_dpi_scaling() {
        // 1920x1040 work area starting at (0, 0), scale 1.0 -> pw=320, ph=72, margin=16
        assert_eq!(
            compute_notify_position(0, 0, 1920, 1040, 1.0, "top-left"),
            (16, 16)
        );
        assert_eq!(
            compute_notify_position(0, 0, 1920, 1040, 1.0, "top-right"),
            (1584, 16)
        );
        assert_eq!(
            compute_notify_position(0, 0, 1920, 1040, 1.0, "bottom-left"),
            (16, 952)
        );
        assert_eq!(
            compute_notify_position(0, 0, 1920, 1040, 1.0, "bottom-right"),
            (1584, 952)
        );

        // Scale 1.25 on 1920x1042 work area -> pw=400, ph=90, margin=20
        assert_eq!(
            compute_notify_position(0, 0, 1920, 1042, 1.25, "bottom-right"),
            (1500, 932)
        );

        // Scale 1.5 with non-zero work area origin (taskbar top/left): pw=480, ph=108, margin=24
        assert_eq!(
            compute_notify_position(100, 40, 1920, 1080, 1.5, "top-left"),
            (124, 64)
        );
        assert_eq!(
            compute_notify_position(100, 40, 1920, 1080, 1.5, "bottom-right"),
            (1516, 988)
        );
    }

    #[test]
    fn compute_notify_position_for_size_stays_within_work_area_and_handles_secondary_monitors() {
        // Negative X monitor on the left
        let (nx, ny) =
            compute_notify_position_for_size(-1920, 0, 1920, 1040, 340, 96, 1.0, "bottom-right");
        assert_eq!((nx, ny), (-356, 928));
        assert!(nx >= -1920 && nx + 340 <= 0);
        assert!(ny >= 0 && ny + 96 <= 1040);

        // Constrained work area where margins must clamp inside [work_x..work_x+work_w]
        let (tx, ty) =
            compute_notify_position_for_size(50, 20, 350, 100, 340, 95, 1.0, "top-left");
        assert_eq!((tx, ty), (60, 25));
        assert!(tx >= 50 && tx + 340 <= 400);
        assert!(ty >= 20 && ty + 95 <= 120);

        let (bx, by) =
            compute_notify_position_for_size(50, 20, 350, 100, 340, 95, 1.0, "bottom-right");
        assert_eq!((bx, by), (50, 20));
    }

    #[test]
    fn compute_toast_logical_size_adapts_height_for_multiline_and_long_text() {
        assert_eq!(compute_toast_logical_size("Xin chào"), (320.0, 72.0));
        assert_eq!(compute_toast_logical_size("Dòng 1\nDòng 2"), (320.0, 86.0));
        assert_eq!(
            compute_toast_logical_size("Đây là một tin nhắn khá dài hơn ba mươi tám ký tự"),
            (320.0, 86.0)
        );
    }

    #[test]
    fn compute_notify_shake_offsets_is_bounded_and_returns_to_origin() {
        let offsets = compute_notify_shake_offsets(1.25);
        assert_eq!(offsets.len(), 26);
        assert_eq!(offsets.last().copied(), Some((0, 0)));
        let bound = (10.0 * 1.25f64).round() as i32;
        assert!(offsets.iter().any(|&(dx, _)| dx != 0));
        assert!(offsets
            .iter()
            .all(|&(dx, dy)| dx.abs() <= bound && dy.abs() <= bound));
    }

    #[test]
    fn validate_web_url_rejects_non_http_schemes() {
        assert!(super::validate_web_url("https://calendar.google.com").is_ok());
        assert!(super::validate_web_url("notion.so").is_ok());
        assert_eq!(
            super::validate_web_url("notion.so").unwrap().as_str(),
            "https://notion.so/"
        );
        assert!(super::validate_web_url("javascript:alert(1)").is_err());
        assert!(super::validate_web_url("file:///C:/Windows/win.ini").is_err());
        assert!(super::validate_web_url("data:text/html,hi").is_err());
    }

    #[test]
    fn extract_invoke_body_bytes_handles_raw_and_json_fallback() {
        use tauri::ipc::InvokeBody;

        let raw = InvokeBody::Raw(vec![137, 80, 78, 71]);
        assert_eq!(
            super::extract_invoke_body_bytes(&raw, 16).unwrap().as_ref(),
            &[137, 80, 78, 71]
        );
        assert!(super::extract_invoke_body_bytes(&raw, 3).is_err());

        let arr = InvokeBody::Json(serde_json::json!([137, 80, 78, 71]));
        assert_eq!(
            super::extract_invoke_body_bytes(&arr, 16).unwrap().as_ref(),
            &[137, 80, 78, 71]
        );
        assert!(super::extract_invoke_body_bytes(&arr, 3).is_err());

        let out_of_range = InvokeBody::Json(serde_json::json!([1, 256]));
        assert!(super::extract_invoke_body_bytes(&out_of_range, 16).is_err());

        let negative = InvokeBody::Json(serde_json::json!([-1]));
        assert!(super::extract_invoke_body_bytes(&negative, 16).is_err());

        let obj = InvokeBody::Json(serde_json::json!({ "bytes": [1, 2] }));
        assert!(super::extract_invoke_body_bytes(&obj, 16).is_err());

        let text = InvokeBody::Json(serde_json::json!("abc"));
        assert!(super::extract_invoke_body_bytes(&text, 16).is_err());
    }

    #[test]
    fn recent_saved_files_allowlist_validates_enforces_cap_and_blocks_executables() {
        use std::collections::VecDeque;
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_saved_allowlist_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();

        let safe_file = dir.join("report.xlsx");
        std::fs::write(&safe_file, b"ok").unwrap();
        let exe_file = dir.join("payload.exe");
        std::fs::write(&exe_file, b"mz").unwrap();
        let other_file = dir.join("untracked.pdf");
        std::fs::write(&other_file, b"pdf").unwrap();

        let mut list = VecDeque::new();
        super::record_recent_saved_file(&mut list, &safe_file);
        super::record_recent_saved_file(&mut list, &exe_file);

        assert!(super::validate_open_saved_file(&list, &safe_file).is_ok());
        assert!(super::validate_reveal_saved_file(&list, &safe_file).is_ok());
        assert!(super::validate_open_saved_file(&list, &exe_file).is_err());
        assert!(super::validate_reveal_saved_file(&list, &exe_file).is_ok());
        assert!(super::validate_open_saved_file(&list, &other_file).is_err());
        assert!(super::validate_reveal_saved_file(&list, &other_file).is_err());

        for idx in 0..(super::MAX_RECENT_SAVED_FILES + 5) {
            super::record_recent_saved_file(&mut list, &dir.join(format!("synthetic_{idx}.txt")));
        }
        assert_eq!(list.len(), super::MAX_RECENT_SAVED_FILES);
        assert!(super::is_recent_saved_file(&list, &safe_file).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
