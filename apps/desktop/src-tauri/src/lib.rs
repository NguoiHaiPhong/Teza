//! Tauri v2 CoreBridge connecting `zalo-core` to the Telegram-inspired React UI.

mod call;
mod clipboard;
mod file_dialog;
mod local_api;
mod notify_popup;
mod scheduler;
mod screen_shake;
mod screenshot;
mod webapp_accounts;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as B64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::ipc::InvokeBody;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewBuilder, WebviewUrl,
    WindowEvent,
};
use tauri_plugin_updater::UpdaterExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use zalo_core::ai::{
    ai_chat_stream_with_relay, ai_config_file, ai_summarize_unread_with_relay,
    ai_translate_text_with_relay, probe_gemini_cli, AiChatTurn, AiCliProbeDto, AiConfig,
    AiConfigPublic, AiConfigUpdate, AiSummaryDto, AiSummaryInputMsg, MAX_CHAT_HISTORY_TURNS,
};
use zalo_core::chat::messaging;
use zalo_core::chat::parser::{
    clean_group_id, format_thread_preview, is_raw_numeric_id, parse_ws_event, pick_str,
    sticker_image_url, WsParsedAction,
};
use zalo_core::chat::threads;
use zalo_core::chat::{
    AddGroupMembersResultDto, ChatFolderDto, FolderFilter, FriendActionResultDto,
    FriendRequestsListDto, KickGroupMembersResultDto, LeaveGroupResultDto, MessageDto, PollDto,
    ThreadDetailsDto, ThreadDto, ThreadKind, UserProfileDto,
};
use zalo_core::data_dir::StoragePaths;
use zalo_core::gdrive::{
    ensure_relay_disabled, load_or_create_device_id, CloudMasterKey, GDriveClient, GDriveConfigDto,
    GDriveError, GDrivePruneResultDto, GDriveStatusDto, GDriveStorageOverviewDto,
    GDriveStoredConfig, GDriveTokens, OAuthLoopbackFlow, VaultFolderIds, GDRIVE_PHASE_CONNECTING,
    GDRIVE_PHASE_ERROR, GDRIVE_PHASE_IDLE, GDRIVE_PHASE_PAUSED_RELAY, GDRIVE_PHASE_PRUNING,
    GDRIVE_PHASE_REHYDRATING, GDRIVE_PHASE_SYNCING_HISTORY, GDRIVE_PHASE_UPLOADING_MEDIA,
    GDRIVE_PROVIDER_NAME, MIN_VAULT_PASSPHRASE_LEN,
};
use zalo_core::media_vault::{
    constant_time_eq, generate_protocol_token, MediaVault, MediaVaultConfig,
};
use zalo_core::qr::{self, QrTicket};
use zalo_core::relay::{RelayConfig, RelayConfigSummary, RelayConfigUpdate, RelayMode};
use zalo_core::session::{
    is_forbidden_svg_mime, AccountProfile, MediaDownloadError, Session, SessionError, SessionStatus,
};
use zalo_core::socket::{self, WsSignal};
use zalo_core::store::{
    AiStoredMessageDto, ApiAuditLogDto, ApiClientDto, CloudMediaBreakdown, CreatedApiClientDto,
    MediaObjectRecord, MediaVaultStatsDto, MessageMediaRecord, MessageStore,
    NewScheduledMessageInput, ScheduledMessageDto, UpdateScheduledMessageInput,
    MEDIA_CDN_STATUS_EXPIRED, MEDIA_CDN_STATUS_OK, MEDIA_LOCAL_STATE_LOCAL,
    MEDIA_LOCAL_STATE_MISSING,
};
use zalo_core::translate::{
    classify_skip_reason, classify_skip_reason_for_mode, is_static_non_translatable,
    normalize_lang_base_code, provider_name, translate_api_only_with_relay, TranslateConfig,
    TranslateConfigSummary, TranslateConfigUpdate, TranslateDetectMode, TranslateInputItem,
    TranslateProvider, TranslationResult,
};

const MAX_STAGED_RAW_UPLOADS: usize = 16;
const STAGED_RAW_UPLOAD_TTL_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct StagedRawUpload {
    pub path: PathBuf,
    pub file_name: String,
    pub bytes_written: u64,
    pub created_ms: i64,
}

pub struct AppState {
    pub paths: StoragePaths,
    pub session: Arc<Mutex<Session>>,
    pub store: Arc<Mutex<MessageStore>>,
    pub media_vault: Arc<RwLock<Arc<MediaVault>>>,
    pub media_protocol_token: String,
    pub gdrive: Arc<GDriveRuntime>,
    pub ws_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub qr_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    pub active_thread_id: Arc<Mutex<Option<String>>>,
    pub translate_cfg: Arc<Mutex<TranslateConfig>>,
    pub relay_cfg: Arc<Mutex<RelayConfig>>,
    pub pending_update: Arc<Mutex<Option<tauri_plugin_updater::Update>>>,
    pub close_to_tray: Arc<AtomicBool>,
    pub allow_exit: Arc<AtomicBool>,
    pub notify_popup: notify_popup::NotifyPopupController,
    pub webapp_op_lock: Arc<Mutex<()>>,
    pub webapp_epoch: Arc<AtomicU64>,
    pub webapp_base_urls: Arc<Mutex<HashMap<String, String>>>,
    pub webapp_last_popup_ms: Arc<AtomicU64>,
    pub webapp_accounts: Arc<Mutex<webapp_accounts::WebAppAccountStore>>,
    pub recent_saved_files: Arc<Mutex<VecDeque<PathBuf>>>,
    pub approved_attachment_paths: Arc<std::sync::Mutex<VecDeque<PathBuf>>>,
    pub staged_raw_uploads: Arc<std::sync::Mutex<HashMap<String, StagedRawUpload>>>,
    pub call_mgr: Arc<call::CallManager>,
    pub ai_config: Arc<Mutex<AiConfig>>,
    pub scheduler: scheduler::SchedulerHandle,
    pub local_api: Arc<local_api::LocalApiManager>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialWsEventDto {
    pub kind: String,
    pub thread_id: Option<String>,
    pub user_id: Option<String>,
    pub self_removed: bool,
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
    if let Some(st) = app.try_state::<AppState>() {
        st.call_mgr.teardown_for_session(Some(app), sess_arc).await;
        st.gdrive
            .clear_for_logout_and_emit(app, &st.relay_cfg, &st.store)
            .await;
    }
    let session_path = paths.session_file();
    if session_path.exists() {
        let _ = std::fs::remove_file(&session_path);
    }
    if emit_event {
        let _ = app.emit("core://auth-expired", reason);
    }
}

fn classify_ws_message_unread(
    is_outgoing: bool,
    active_thread_id: Option<&str>,
    msg_thread_id: &str,
    main_focused: bool,
) -> (bool, bool) {
    if is_outgoing {
        return (false, true);
    }
    let is_active_and_focused = main_focused && active_thread_id == Some(msg_thread_id);
    if is_active_and_focused {
        (false, true)
    } else {
        (true, false)
    }
}

fn build_thread_read_synced_payload(thread_id: &str, is_unread: bool) -> serde_json::Value {
    serde_json::json!({
        "thread_id": thread_id,
        "is_unread": is_unread,
    })
}

fn record_read_sync_watermark(
    cleared_up_to: &mut std::collections::HashMap<String, i64>,
    thread_id: &str,
    is_unread: bool,
    last_msg_id: Option<&str>,
) {
    if is_unread {
        cleared_up_to.remove(thread_id);
        return;
    }
    if let Some(id_num) = last_msg_id
        .map(str::trim)
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
    {
        cleared_up_to
            .entry(thread_id.to_string())
            .and_modify(|cur| *cur = (*cur).max(id_num))
            .or_insert(id_num);
    }
}

fn is_msg_cleared_by_read_sync(
    cleared_up_to: &std::collections::HashMap<String, i64>,
    thread_id: &str,
    msg_id: &str,
) -> bool {
    let Some(&cleared_id) = cleared_up_to.get(thread_id) else {
        return false;
    };
    msg_id
        .trim()
        .parse::<i64>()
        .ok()
        .is_some_and(|mid| mid > 0 && mid <= cleared_id)
}

fn pick_latest_incoming_meta_for_seen(
    messages: &[MessageDto],
) -> Option<zalo_core::store::IncomingSeenMeta> {
    messages
        .iter()
        .filter(|m| !m.is_outgoing && is_server_msg_id(&m.msg_id))
        .max_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.msg_id.cmp(&b.msg_id)))
        .map(|m| {
            (
                m.msg_id.clone(),
                m.cli_msg_id.clone(),
                m.sender_id.clone(),
                m.msg_type.clone(),
                m.ts,
            )
        })
}

#[cfg(test)]
fn pick_latest_incoming_for_seen(messages: &[MessageDto]) -> Option<(String, String, String)> {
    pick_latest_incoming_meta_for_seen(messages)
        .map(|(mid, _cli, sender, msg_type, _ts)| (mid, sender, msg_type))
}

fn is_main_window_focused(app: &AppHandle) -> bool {
    app.get_window("main")
        .map(|w| {
            w.is_visible().unwrap_or(false)
                && !w.is_minimized().unwrap_or(false)
                && w.is_focused().unwrap_or(false)
        })
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
async fn ensure_ws_worker(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    media_vault_arc: Arc<RwLock<Arc<MediaVault>>>,
    ws_task_arc: Arc<Mutex<Option<JoinHandle<()>>>>,
    active_thread_arc: Arc<Mutex<Option<String>>>,
    call_mgr: Arc<call::CallManager>,
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
            media_vault_arc,
            ws_task_arc,
            active_thread_arc,
            call_mgr,
            paths,
        )
        .await;
    }
}

fn should_dispatch_ws_event(ev: &zalo_core::socket::WsEvent) -> bool {
    ev.is_chat_ish()
        || ev.has_chat_message_payload()
        || ev.is_control_ish()
        || ev.is_receipt_ish()
        || zalo_core::chat::parser::is_reaction_ws_cmd(ev.cmd)
        || !zalo_core::chat::parser::extract_voip_controls(&ev.payload).is_empty()
}

fn is_direct_peer_thread_id(thread_id: &str, my_uid: &str, send2me_id: &str) -> bool {
    let trimmed = thread_id.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with('g') || lower.starts_with("cloud_") || lower.starts_with("cloud-") {
        return false;
    }
    if !my_uid.is_empty() && trimmed == my_uid {
        return false;
    }
    if !send2me_id.is_empty() && trimmed == send2me_id {
        return false;
    }
    true
}

fn is_raw_numeric_uid_like(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if is_raw_numeric_id(t) {
        return true;
    }
    if let Some((head, tail)) = t.split_once('_') {
        return !head.is_empty()
            && !tail.is_empty()
            && head.bytes().all(|b| b.is_ascii_digit())
            && tail.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

fn ws_message_fallback_title(msg: &MessageDto) -> Option<&str> {
    if msg.is_outgoing {
        return None;
    }
    let trimmed = msg.sender_name.trim();
    if trimmed.is_empty()
        || trimmed == msg.sender_id.trim()
        || trimmed == msg.thread_id.trim()
        || is_raw_numeric_uid_like(trimmed)
    {
        return None;
    }
    Some(trimmed)
}

#[allow(clippy::too_many_arguments)]
async fn restart_ws_worker(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    media_vault_arc: Arc<RwLock<Arc<MediaVault>>>,
    ws_task_arc: Arc<Mutex<Option<JoinHandle<()>>>>,
    active_thread_arc: Arc<Mutex<Option<String>>>,
    call_mgr: Arc<call::CallManager>,
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
        let in_flight_thread_uids =
            Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
        let mut cleared_up_to_msg_id: std::collections::HashMap<String, i64> =
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
                    if !should_dispatch_ws_event(&ev) {
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
                    let main_focused = is_main_window_focused(&app);
                    let mut pending_resolve_msgs: Vec<MessageDto> = Vec::new();
                    let mut missing_direct_thread_uids: Vec<String> = Vec::new();
                    let mut auto_cache_jobs: Vec<(String, &'static str, String, String)> =
                        Vec::new();
                    let mut orphaned_media_paths: Vec<String> = Vec::new();
                    let mut voip_events = Vec::new();
                    let mut read_synced_events: Vec<(String, bool)> = Vec::new();
                    let mut social_events: Vec<SocialWsEventDto> = Vec::new();
                    let mut poll_updated_events: Vec<(String, Option<i64>)> = Vec::new();
                    let mut poll_detail_refresh_jobs: Vec<(String, i64)> = Vec::new();
                    let mut self_removed_thread_ids: Vec<String> = Vec::new();
                    let mut threads_touched = false;
                    {
                        let mut store = store_arc.lock().await;
                        let mut unresolved_direct_cache: Option<
                            std::collections::HashSet<String>,
                        > = None;
                        for act in actions {
                            match act {
                                WsParsedAction::Message(mut msg) => {
                                    let is_group = store
                                        .get_thread_kind(&msg.thread_id)
                                        .ok()
                                        .flatten()
                                        == Some(ThreadKind::Group);
                                    let incoming_poll_sparse = parse_message_poll_dto(&msg)
                                        .is_some_and(|p| {
                                            p.options.is_empty()
                                                || p.question.trim().is_empty()
                                                || p.question.trim() == "Bình chọn"
                                                || zalo_core::chat::parser::is_poll_activity_text(
                                                    &p.question,
                                                )
                                        });
                                    let was_updated_existing_poll =
                                        upsert_incoming_message(&mut store, &mut msg)
                                            .unwrap_or(false);
                                    if was_updated_existing_poll {
                                        let _ = app.emit("core://message-updated", &*msg);
                                        if let Some(poll) = parse_message_poll_dto(&msg)
                                            .filter(|p| p.poll_id > 0)
                                        {
                                            if poll.options.is_empty() || incoming_poll_sparse {
                                                poll_detail_refresh_jobs
                                                    .push((msg.thread_id.clone(), poll.poll_id));
                                            }
                                        }
                                        continue;
                                    }
                                    let preview = format_thread_preview(
                                        &msg.text,
                                        msg.is_outgoing,
                                        &msg.sender_name,
                                        is_group,
                                    );
                                    let already_cleared = !msg.is_outgoing
                                        && is_msg_cleared_by_read_sync(
                                            &cleared_up_to_msg_id,
                                            &msg.thread_id,
                                            &msg.msg_id,
                                        );
                                    let (inc_unread, clear_unread) = if already_cleared {
                                        (false, false)
                                    } else {
                                        classify_ws_message_unread(
                                            msg.is_outgoing,
                                            active_id.as_deref(),
                                            &msg.thread_id,
                                            main_focused,
                                        )
                                    };
                                    let fallback_title = ws_message_fallback_title(&msg);
                                    let created = store
                                        .update_thread_preview_with_fallback_meta(
                                            &msg.thread_id,
                                            &preview,
                                            msg.ts,
                                            inc_unread,
                                            clear_unread,
                                            fallback_title,
                                            None,
                                        )
                                        .unwrap_or(false);
                                    if !is_group
                                        && is_direct_peer_thread_id(
                                            &msg.thread_id,
                                            &my_uid,
                                            &send2me_id,
                                        )
                                    {
                                        let needs_profile = created || {
                                            let set = unresolved_direct_cache.get_or_insert_with(
                                                || {
                                                    store
                                                        .list_unresolved_direct_thread_ids(500)
                                                        .unwrap_or_default()
                                                        .into_iter()
                                                        .collect()
                                                },
                                            );
                                            set.contains(&msg.thread_id)
                                        };
                                        if needs_profile
                                            && !missing_direct_thread_uids
                                                .iter()
                                                .any(|u| u == &msg.thread_id)
                                        {
                                            missing_direct_thread_uids.push(msg.thread_id.clone());
                                        }
                                    }
                                    if already_cleared {
                                        let _ = store.advance_last_read_in_ts(
                                            &msg.thread_id,
                                            msg.ts,
                                        );
                                        threads_touched = true;
                                    } else if created {
                                        threads_touched = true;
                                    }
                                    if let Some(ref att_url) = msg.attachment_url {
                                        if let Some(kind) =
                                            classify_ws_auto_cache_kind(&msg.msg_type)
                                        {
                                            if validate_teza_media_remote_url(att_url).is_ok() {
                                                auto_cache_jobs.push((
                                                    att_url.clone(),
                                                    kind,
                                                    msg.thread_id.clone(),
                                                    msg.msg_id.clone(),
                                                ));
                                            }
                                        }
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
                                    last_msg_id,
                                } => {
                                    record_read_sync_watermark(
                                        &mut cleared_up_to_msg_id,
                                        &thread_id,
                                        is_unread,
                                        last_msg_id.as_deref(),
                                    );
                                    let _ = store.set_thread_unread(
                                        &thread_id,
                                        if is_unread { 1 } else { 0 },
                                    );
                                    read_synced_events.push((thread_id, is_unread));
                                    threads_touched = true;
                                }
                                WsParsedAction::PeerSeen {
                                    thread_id,
                                    seen_ts,
                                    is_delivered_only,
                                } => {
                                    if !is_delivered_only {
                                        if let Ok(peer_ts) =
                                            store.update_peer_read_ts(&thread_id, seen_ts)
                                        {
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
                                    if let Ok(orphans) =
                                        store.cleanup_orphaned_media_for_msg(&thread_id, &msg_id)
                                    {
                                        orphaned_media_paths.extend(orphans);
                                    }
                                    if let Ok(Some(updated)) =
                                        store.mark_recalled(&thread_id, &msg_id)
                                    {
                                        let _ = app.emit("core://message-updated", &updated);
                                    }
                                }
                                WsParsedAction::Reaction {
                                    thread_id,
                                    msg_id,
                                    cli_msg_id,
                                    actor_id,
                                    emoji,
                                    me,
                                    removed,
                                } => {
                                    if let Ok(Some(updated)) = store.apply_reaction_event(
                                        &thread_id,
                                        &msg_id,
                                        cli_msg_id.as_deref(),
                                        actor_id.as_deref(),
                                        emoji.as_deref(),
                                        me,
                                        removed,
                                    ) {
                                        let _ = app.emit("core://message-updated", &updated);
                                    }
                                }
                                WsParsedAction::Voip(v) => voip_events.push(v),
                                WsParsedAction::SocialEvent {
                                    kind,
                                    thread_id,
                                    user_id,
                                    self_removed,
                                } => {
                                    if self_removed {
                                        if let Some(ref tid) = thread_id {
                                            let trimmed = tid.trim();
                                            if !trimmed.is_empty() {
                                                if let Ok(orphans) = store.delete_thread(trimmed) {
                                                    orphaned_media_paths.extend(orphans);
                                                }
                                                self_removed_thread_ids.push(trimmed.to_string());
                                                threads_touched = true;
                                            }
                                        }
                                    }
                                    social_events.push(SocialWsEventDto {
                                        kind,
                                        thread_id,
                                        user_id,
                                        self_removed,
                                    });
                                }
                                WsParsedAction::PollUpdated { thread_id, poll_id } => {
                                    poll_updated_events.push((thread_id, poll_id));
                                }
                            }
                        }
                    }
                    if !self_removed_thread_ids.is_empty() {
                        let mut active = active_thread_arc.lock().await;
                        if active.as_deref().is_some_and(|cur| {
                            self_removed_thread_ids.iter().any(|tid| tid == cur)
                        }) {
                            *active = None;
                        }
                    }
                    if !orphaned_media_paths.is_empty() {
                        orphaned_media_paths.sort();
                        orphaned_media_paths.dedup();
                        let vault = media_vault_arc.read().await.clone();
                        vault.unlink_rel_paths(&orphaned_media_paths).await;
                    }
                    if !auto_cache_jobs.is_empty() {
                        let vault = media_vault_arc.read().await.clone();
                        let cfg = vault.config();
                        for (url, kind, tid, mid) in auto_cache_jobs {
                            if !should_auto_cache_kind(&cfg, kind) {
                                continue;
                            }
                            let vault_bg = vault.clone();
                            let sess_bg = sess_clone.clone_shallow();
                            let store_bg = store_arc.clone();
                            let app_bg = app.clone();
                            tokio::spawn(async move {
                                if let Ok(obj) = vault_bg
                                    .ensure_cached_url(
                                        &sess_bg,
                                        &store_bg,
                                        &url,
                                        kind,
                                        Some((tid.as_str(), mid.as_str())),
                                        |_, _| {},
                                    )
                                    .await
                                {
                                    if obj.cloud_file_id.is_none() {
                                        if let Some(st) = app_bg.try_state::<AppState>() {
                                            st.gdrive.notify_sync();
                                        }
                                    }
                                }
                            });
                        }
                    }
                    for v in voip_events {
                        call_mgr.feed_ws_voip(&v).await;
                    }
                    for (thread_id, is_unread) in read_synced_events {
                        let _ = app.emit(
                            "core://thread-read-synced",
                            build_thread_read_synced_payload(&thread_id, is_unread),
                        );
                    }
                    for social_ev in social_events {
                        let _ = app.emit("core://social-event", &social_ev);
                    }
                    let mut refreshed_poll_ids = std::collections::HashSet::<i64>::new();
                    for (thread_id, poll_id) in poll_updated_events {
                        let _ = app.emit(
                            "core://poll-updated",
                            serde_json::json!({
                                "thread_id": thread_id,
                                "poll_id": poll_id,
                            }),
                        );
                        if let Some(pid) = poll_id.filter(|&id| id > 0) {
                            if refreshed_poll_ids.insert(pid) {
                                poll_detail_refresh_jobs.push((thread_id, pid));
                            }
                        }
                    }
                    if !poll_detail_refresh_jobs.is_empty() {
                        let sess_bg_opt = {
                            let sess = sess_arc_for_ws.lock().await;
                            if sess.is_authenticated() {
                                Some(sess.clone_shallow())
                            } else {
                                None
                            }
                        };
                        if let Some(sess_base) = sess_bg_opt {
                            let mut seen_refresh = std::collections::HashSet::<i64>::new();
                            for (tid_bg, pid) in poll_detail_refresh_jobs {
                                if pid <= 0 || !seen_refresh.insert(pid) {
                                    continue;
                                }
                                let sess_bg = sess_base.clone_shallow();
                                let app_bg = app.clone();
                                let store_bg = store_arc.clone();
                                tokio::spawn(async move {
                                    if let Ok(poll) = threads::get_poll_detail(&sess_bg, pid).await
                                    {
                                        let tid_opt =
                                            (!tid_bg.trim().is_empty()).then_some(tid_bg.as_str());
                                        let _ = update_local_poll_message(
                                            &app_bg,
                                            &store_bg,
                                            tid_opt,
                                            None,
                                            &poll,
                                        )
                                        .await;
                                    }
                                });
                            }
                        }
                    }
                    if threads_touched {
                        let _ = app.emit("core://threads-updated", ());
                    }
                    let mut direct_uids_to_fetch: Vec<String> = Vec::new();
                    if !missing_direct_thread_uids.is_empty() {
                        let mut inflight_t = in_flight_thread_uids.lock().await;
                        for uid in missing_direct_thread_uids {
                            if inflight_t.insert(uid.clone()) {
                                direct_uids_to_fetch.push(uid);
                            }
                        }
                    }
                    if !missing_uids.is_empty() || !direct_uids_to_fetch.is_empty() {
                        let mut all_uids = missing_uids.clone();
                        for uid in &direct_uids_to_fetch {
                            if !all_uids.iter().any(|u| u == uid) {
                                all_uids.push(uid.clone());
                            }
                        }
                        let app_bg = app.clone();
                        let sess_bg = sess_clone.clone_shallow();
                        let store_bg = store_arc.clone();
                        let cache_bg = ws_name_cache.clone();
                        let inflight_bg = in_flight_uids.clone();
                        let inflight_thread_bg = in_flight_thread_uids.clone();
                        tokio::spawn(async move {
                            let resolved =
                                threads::hydrate_unresolved_direct_threads(&sess_bg, &all_uids)
                                    .await;
                            if !resolved.is_empty() {
                                {
                                    let mut cache = cache_bg.lock().await;
                                    for (k, (name, _avatar)) in &resolved {
                                        if !name.trim().is_empty() && !is_raw_numeric_uid_like(name)
                                        {
                                            cache.insert(k.clone(), name.clone());
                                        }
                                    }
                                }
                                let mut thread_meta_changed = false;
                                {
                                    let mut store = store_bg.lock().await;
                                    for mut msg in pending_resolve_msgs {
                                        let found = resolved.get(&msg.sender_id).or_else(|| {
                                            msg.sender_id
                                                .split('_')
                                                .next()
                                                .and_then(|bare| resolved.get(bare))
                                        });
                                        if let Some((name, _)) = found {
                                            if !name.trim().is_empty() {
                                                msg.sender_name = name.clone();
                                                let _ = upsert_incoming_message(&mut store, &mut msg);
                                                let _ = app_bg.emit("core://message-updated", &msg);
                                            }
                                        }
                                    }
                                    for uid in &direct_uids_to_fetch {
                                        let found = resolved.get(uid).or_else(|| {
                                            uid.split('_')
                                                .next()
                                                .and_then(|bare| resolved.get(bare))
                                        });
                                        if let Some((name, avatar)) = found {
                                            if store
                                                .upsert_thread_profile_meta(uid, name, avatar)
                                                .unwrap_or(false)
                                            {
                                                thread_meta_changed = true;
                                            }
                                        }
                                    }
                                }
                                if thread_meta_changed {
                                    let _ = app_bg.emit("core://threads-updated", ());
                                }
                            }
                            if !missing_uids.is_empty() {
                                let mut inflight = inflight_bg.lock().await;
                                for uid in &missing_uids {
                                    inflight.remove(uid);
                                }
                            }
                            if !direct_uids_to_fetch.is_empty() {
                                let mut inflight_t = inflight_thread_bg.lock().await;
                                for uid in &direct_uids_to_fetch {
                                    inflight_t.remove(uid);
                                }
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
                let uid_changed = sess_snapshot.profile.uid != profile.uid;
                profile = sess_snapshot.profile.clone();
                let mut sess = state.session.lock().await;
                sess.profile = profile.clone();
                let _ = sess.save_to_file(&state.paths.session_file());
                drop(sess);
                if uid_changed && !profile.uid.is_empty() {
                    state
                        .gdrive
                        .rebind_for_uid_and_emit(
                            &app,
                            Some(&profile.uid),
                            &state.relay_cfg,
                            &state.store,
                        )
                        .await;
                }
            }
            ensure_ws_worker(
                app.clone(),
                state.session.clone(),
                state.store.clone(),
                state.media_vault.clone(),
                state.ws_task.clone(),
                state.active_thread_id.clone(),
                state.call_mgr.clone(),
                state.paths.clone(),
            )
            .await;
            let _ = state
                .call_mgr
                .ensure_started_from_session(&app, &state.session, &state.store)
                .await;
            Ok(AuthStatusDto::Authenticated { profile })
        }
        SessionStatus::Unreachable => {
            ensure_ws_worker(
                app.clone(),
                state.session.clone(),
                state.store.clone(),
                state.media_vault.clone(),
                state.ws_task.clone(),
                state.active_thread_id.clone(),
                state.call_mgr.clone(),
                state.paths.clone(),
            )
            .await;
            let _ = state
                .call_mgr
                .ensure_started_from_session(&app, &state.session, &state.store)
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
    let media_vault_arc = state.media_vault.clone();
    let gdrive_arc = state.gdrive.clone();
    let relay_cfg_arc = state.relay_cfg.clone();
    let ws_task_arc = state.ws_task.clone();
    let active_thread_arc = state.active_thread_id.clone();
    let call_mgr_arc = state.call_mgr.clone();
    let scheduler_for_qr = state.scheduler.clone();
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
                    let prev_vault = media_vault_arc.read().await.clone();
                    prev_vault.flush_access_touches(&store_arc).await;
                    let db_path = paths.history_db(Some(&profile.uid));
                    if let Ok(new_store) = MessageStore::open(&db_path) {
                        let _ = new_store.recover_stuck_sending_schedules(chrono_now_ms());
                        let mut guard = store_arc.lock().await;
                        *guard = new_store;
                    }
                    scheduler_for_qr.notify_changed();
                    rebind_media_vault_unlocked(&media_vault_arc, &paths, Some(&profile.uid)).await;
                    gdrive_arc
                        .rebind_for_uid_and_emit(
                            &app,
                            Some(&profile.uid),
                            &relay_cfg_arc,
                            &store_arc,
                        )
                        .await;
                }

                let _ = app.emit("core://auth-authenticated", &profile);
                restart_ws_worker(
                    app.clone(),
                    sess_arc.clone(),
                    store_arc.clone(),
                    media_vault_arc,
                    ws_task_arc,
                    active_thread_arc,
                    call_mgr_arc.clone(),
                    paths,
                )
                .await;
                let _ = call_mgr_arc
                    .restart_from_session(&app, &sess_arc, &store_arc)
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
async fn auth_logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
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
    state
        .call_mgr
        .teardown_for_session(Some(&app), &state.session)
        .await;
    let path = state.paths.session_file();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Không xóa được session.json: {e}"))?;
    }
    {
        let prev_vault = state.media_vault.read().await.clone();
        prev_vault.flush_access_touches(&state.store).await;
    }
    let fallback_db = state.paths.history_db(None);
    if let Ok(anon_store) = MessageStore::open(&fallback_db) {
        let mut store = state.store.lock().await;
        *store = anon_store;
    }
    state.scheduler.notify_changed();
    rebind_media_vault_unlocked(&state.media_vault, &state.paths, None).await;
    state
        .gdrive
        .clear_for_logout_and_emit(&app, &state.relay_cfg, &state.store)
        .await;
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
    let unresolved = {
        let store = state.store.lock().await;
        for t in &remote.threads {
            let remote_read_authoritative = remote.is_remote_read_authoritative(&t.id);
            let _ = store.upsert_thread_synced_with_read_state(
                t,
                remote.pinned_synced,
                remote_read_authoritative,
            );
        }
        store
            .list_unresolved_direct_thread_ids(60)
            .unwrap_or_default()
    };
    if !unresolved.is_empty() && sess_snapshot.is_authenticated() {
        let resolved =
            threads::hydrate_unresolved_direct_threads(&sess_snapshot, &unresolved).await;
        if !resolved.is_empty() {
            let store = state.store.lock().await;
            for uid in &unresolved {
                let found = resolved.get(uid).or_else(|| {
                    uid.split('_')
                        .next()
                        .and_then(|bare| resolved.get(bare))
                });
                if let Some((name, avatar)) = found {
                    let _ = store.upsert_thread_profile_meta(uid, name, avatar);
                }
            }
        }
    }
    let store = state.store.lock().await;
    store.list_threads(folder).map_err(|e| e.to_string())
}

#[tauri::command]
async fn thread_ensure_direct(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    title: String,
    avatar: Option<String>,
) -> Result<ThreadDto, String> {
    let thread = {
        let store = state.store.lock().await;
        store
            .ensure_direct_thread(&thread_id, &title, avatar.as_deref().unwrap_or(""))
            .map_err(|e| e.to_string())?
    };
    let _ = app.emit("core://threads-updated", ());
    Ok(thread)
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
    let (kind, latest_incoming) = {
        let store = state.store.lock().await;
        store.mark_thread_read(&thread_id).map_err(|e| e.to_string())?;
        let kind = store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let latest = store
            .get_latest_incoming_message_for_seen(&thread_id)
            .ok()
            .flatten()
            .filter(|(mid, _, _, _, _)| is_server_msg_id(mid));
        (kind, latest)
    };
    if thread_id != "cloud-me" {
        let tid = thread_id.clone();
        spawn_best_effort_thread_sync(app, &state, move |sess| async move {
            let res = threads::sync_unread_mark(&sess, &tid, kind, false).await;
            if res.as_ref().is_err_and(|e| expired_code(e).is_some()) {
                return res;
            }
            let is_cloud = kind == ThreadKind::Cloud
                || (!sess.profile.send2me_id.is_empty() && tid == sess.profile.send2me_id);
            if !is_cloud {
                let target = match latest_incoming {
                    Some(item) => Some(item),
                    None => match threads::fetch_thread_history(&sess, &tid, kind, 10).await {
                        Ok(msgs) => pick_latest_incoming_meta_for_seen(&msgs),
                        Err(e) if expired_code(&e).is_some() => return Err(e),
                        Err(_) => None,
                    },
                };
                if let Some((mid, cli_mid, sender_id, msg_type, ts)) = target {
                    let seen_res = threads::send_seen_event_with_meta(
                        &sess,
                        &tid,
                        kind,
                        &mid,
                        cli_mid.as_deref(),
                        &sender_id,
                        &msg_type,
                        ts,
                    )
                    .await;
                    if seen_res.as_ref().is_err_and(|e| expired_code(e).is_some()) {
                        return seen_res;
                    }
                }
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
    state.notify_popup.show_popup(&app, payload).await
}

#[tauri::command]
async fn buzz_shake_screen() -> Result<(), String> {
    screen_shake::trigger_screen_shake()
}

#[tauri::command]
async fn notify_get_current(state: State<'_, AppState>) -> Result<Option<NotifyToastPayload>, String> {
    Ok(state.notify_popup.get_current().await)
}

#[tauri::command]
async fn notify_dismiss(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.notify_popup.dismiss_popup(&app).await
}

#[tauri::command]
async fn notify_click(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    state.notify_popup.click_popup(&app, thread_id).await
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
    let mut creator_id = String::new();
    let mut admin_ids = Vec::new();
    if kind == ThreadKind::Group {
        let sess_snapshot = {
            let sess = state.session.lock().await;
            sess.clone_shallow()
        };
        if sess_snapshot.is_authenticated() {
            if let Ok(details) =
                threads::fetch_group_details(&sess_snapshot, &thread_id, &known_profiles).await
            {
                total_members = details.total_members;
                members = details.members;
                creator_id = details.creator_id;
                admin_ids = details.admin_ids;
            }
        }
    }

    Ok(ThreadDetailsDto {
        thread_id,
        total_members,
        members,
        creator_id,
        admin_ids,
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

fn resolve_phone_country_code(country_code: Option<&str>) -> &str {
    country_code
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("84")
}

#[tauri::command]
async fn user_find_by_phone(
    state: State<'_, AppState>,
    phone: String,
    country_code: Option<String>,
) -> Result<UserProfileDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let country = resolve_phone_country_code(country_code.as_deref());
    threads::find_user_by_phone_with_country(&sess_snapshot, &phone, country)
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

fn validate_social_user_id(raw: &str) -> Result<String, String> {
    let cleaned = raw.trim().split('_').next().unwrap_or("").trim();
    if cleaned.is_empty() {
        return Err("Mã người dùng không hợp lệ".into());
    }
    Ok(cleaned.to_string())
}

fn validate_social_group_id(raw: &str) -> Result<String, String> {
    let cleaned = clean_group_id(raw).trim();
    if cleaned.is_empty() {
        return Err("Mã nhóm không hợp lệ".into());
    }
    Ok(cleaned.to_string())
}

fn normalize_social_member_ids(
    member_ids: &[String],
    self_uid: Option<&str>,
) -> Result<Vec<String>, String> {
    let clean_self = self_uid
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.split('_').next())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut out = Vec::new();
    for raw in member_ids {
        let uid = raw.trim().split('_').next().unwrap_or("").trim();
        if uid.is_empty() {
            continue;
        }
        if clean_self == Some(uid) {
            return Err(
                "Không thể tự xóa chính mình khỏi nhóm — hãy dùng chức năng Rời nhóm.".into(),
            );
        }
        if !out.iter().any(|existing| existing == uid) {
            out.push(uid.to_string());
        }
    }
    if out.is_empty() {
        return Err("Vui lòng chọn ít nhất 1 thành viên".into());
    }
    Ok(out)
}

fn normalize_friend_request_message(message: Option<&str>) -> Option<String> {
    message
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(150).collect())
}

async fn require_authenticated_session(state: &State<'_, AppState>) -> Result<Session, String> {
    let sess = state.session.lock().await;
    if !sess.is_authenticated() {
        return Err("Chưa đăng nhập".into());
    }
    Ok(sess.clone_shallow())
}

async fn map_social_call_error(
    app: &AppHandle,
    state: &State<'_, AppState>,
    err: anyhow::Error,
) -> String {
    if let Some(code) = expired_code(&err) {
        let reason =
            format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
        clear_expired_session(
            app,
            &state.session,
            Some(&state.ws_task),
            &state.paths,
            reason.clone(),
            true,
        )
        .await;
        return reason;
    }
    err.to_string()
}

fn spawn_best_effort_threads_refresh(
    app: AppHandle,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
) {
    tauri::async_runtime::spawn(async move {
        let sess_snapshot = {
            let sess = sess_arc.lock().await;
            if !sess.is_authenticated() {
                return;
            }
            sess.clone_shallow()
        };
        if let Ok(remote) = threads::sync_threads_full(&sess_snapshot).await {
            {
                let store = store_arc.lock().await;
                for t in &remote.threads {
                    let remote_read_authoritative = remote.is_remote_read_authoritative(&t.id);
                    let _ = store.upsert_thread_synced_with_read_state(
                        t,
                        remote.pinned_synced,
                        remote_read_authoritative,
                    );
                }
            }
            let _ = app.emit("core://threads-updated", ());
        }
    });
}

#[tauri::command]
async fn friend_remove(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    // Intentionally keep local DM message history intact after unfriending.
    let res = match threads::remove_friend(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_removed".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn friend_send_request(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
    message: Option<String>,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let clean_msg = normalize_friend_request_message(message.as_deref());
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res =
        match threads::send_friend_request(&sess_snapshot, &clean_uid, clean_msg.as_deref()).await {
            Ok(r) => r,
            Err(e) => return Err(map_social_call_error(&app, &state, e).await),
        };
    if res.auto_accepted {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_added".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
        spawn_best_effort_threads_refresh(app, state.session.clone(), state.store.clone());
    } else if res.ok && !res.already_friends {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_request_sent".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn friend_accept_request(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res = match threads::accept_friend_request(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_added".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
        spawn_best_effort_threads_refresh(app, state.session.clone(), state.store.clone());
    }
    Ok(res)
}

#[tauri::command]
async fn friend_reject_request(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res = match threads::reject_friend_request(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_request_rejected".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn friend_undo_request(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res = match threads::undo_friend_request(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "friend_request_undone".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn friend_list_requests(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<FriendRequestsListDto, String> {
    let sess_snapshot = require_authenticated_session(&state).await?;
    match threads::list_friend_requests(&sess_snapshot).await {
        Ok(list) => Ok(list),
        Err(e) => Err(map_social_call_error(&app, &state, e).await),
    }
}

#[tauri::command]
async fn friend_block_user(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res = match threads::block_user(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "user_blocked".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn friend_unblock_user(
    app: AppHandle,
    state: State<'_, AppState>,
    user_id: String,
) -> Result<FriendActionResultDto, String> {
    let clean_uid = validate_social_user_id(&user_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res = match threads::unblock_user(&sess_snapshot, &clean_uid).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if res.ok {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "user_unblocked".to_string(),
                thread_id: None,
                user_id: Some(res.user_id.clone()),
                self_removed: false,
            },
        );
    }
    Ok(res)
}

#[tauri::command]
async fn group_leave(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    silent: Option<bool>,
) -> Result<LeaveGroupResultDto, String> {
    let clean_gid = validate_social_group_id(&group_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let res =
        match threads::leave_group(&sess_snapshot, &clean_gid, silent.unwrap_or(false)).await {
            Ok(r) => r,
            Err(e) => return Err(map_social_call_error(&app, &state, e).await),
        };
    if res.left {
        let mut orphans = {
            let mut store = state.store.lock().await;
            let mut list = store.delete_thread(&res.group_id).unwrap_or_default();
            let raw_trimmed = group_id.trim();
            if !raw_trimmed.is_empty() && raw_trimmed != res.group_id {
                if let Ok(extra) = store.delete_thread(raw_trimmed) {
                    list.extend(extra);
                }
            }
            list
        };
        orphans.sort();
        orphans.dedup();
        {
            let mut active = state.active_thread_id.lock().await;
            if active
                .as_deref()
                .is_some_and(|cur| cur == res.group_id || cur == group_id.trim())
            {
                *active = None;
            }
        }
        if !orphans.is_empty() {
            let vault = state.media_vault.read().await.clone();
            vault.unlink_rel_paths(&orphans).await;
        }
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "group_left".to_string(),
                thread_id: Some(res.group_id.clone()),
                user_id: None,
                self_removed: true,
            },
        );
        let _ = app.emit("core://threads-updated", ());
    }
    Ok(res)
}

#[tauri::command]
async fn group_kick_members(
    app: AppHandle,
    state: State<'_, AppState>,
    group_id: String,
    member_ids: Vec<String>,
) -> Result<KickGroupMembersResultDto, String> {
    let clean_gid = validate_social_group_id(&group_id)?;
    let sess_snapshot = require_authenticated_session(&state).await?;
    let my_uid = (!sess_snapshot.profile.uid.trim().is_empty())
        .then_some(sess_snapshot.profile.uid.as_str());
    let clean_members = normalize_social_member_ids(&member_ids, my_uid)?;
    let res = match threads::kick_group_members(&sess_snapshot, &clean_gid, &clean_members).await {
        Ok(r) => r,
        Err(e) => return Err(map_social_call_error(&app, &state, e).await),
    };
    if !res.removed.is_empty() {
        let _ = app.emit(
            "core://social-event",
            &SocialWsEventDto {
                kind: "group_member_removed".to_string(),
                thread_id: Some(res.group_id.clone()),
                user_id: res.removed.first().cloned(),
                self_removed: false,
            },
        );
    }
    Ok(res)
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
            cli_msg_id: None,
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
            cli_msg_id: None,
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
    let persist_result = {
        let mut store = state.store.lock().await;
        persist_created_poll_message_in_store(&mut store, msg)?
    };
    match persist_result {
        PollCreatePersistResult::InsertedNew {
            msg,
            thread_created,
        } => {
            let _ = app.emit("core://message-new", &msg);
            if thread_created {
                let _ = app.emit("core://threads-updated", ());
            }
            Ok(msg)
        }
        PollCreatePersistResult::MergedIntoReal { msg } => {
            let _ = app.emit("core://message-updated", &msg);
            Ok(msg)
        }
    }
}

enum PollCreatePersistResult {
    InsertedNew {
        msg: MessageDto,
        thread_created: bool,
    },
    MergedIntoReal {
        msg: MessageDto,
    },
}

fn persist_created_poll_message_in_store(
    store: &mut MessageStore,
    msg: MessageDto,
) -> Result<PollCreatePersistResult, String> {
    if is_synthetic_poll_msg_id(&msg.msg_id) {
        if let Some(created_poll) = parse_message_poll_dto(&msg).filter(|p| p.poll_id > 0) {
            if let Some(existing_real) =
                find_local_poll_message(store, Some(&msg.thread_id), None, created_poll.poll_id)
                    .filter(|m| !is_synthetic_poll_msg_id(&m.msg_id))
            {
                if let Some((merged_msg, _)) = apply_poll_update_to_store(
                    store,
                    Some(&existing_real.thread_id),
                    Some(&existing_real.msg_id),
                    &created_poll,
                ) {
                    return Ok(PollCreatePersistResult::MergedIntoReal { msg: merged_msg });
                }
            }
        }
    }
    store.upsert_message(&msg).map_err(|e| e.to_string())?;
    let preview = format_thread_preview(&msg.text, true, "", false);
    let thread_created = store
        .update_thread_preview_returning_created(&msg.thread_id, &preview, msg.ts, false, true)
        .unwrap_or(false);
    Ok(PollCreatePersistResult::InsertedNew {
        msg,
        thread_created,
    })
}

fn parse_message_poll_dto(msg: &MessageDto) -> Option<PollDto> {
    let raw = msg
        .attachment_name
        .as_deref()
        .and_then(|s| s.strip_prefix("teza-poll:"))?;
    serde_json::from_str::<PollDto>(raw).ok()
}

fn is_synthetic_poll_msg_id(msg_id: &str) -> bool {
    msg_id.starts_with("poll-") || msg_id.starts_with("local-poll-")
}

fn real_poll_id_for_dedupe(msg: &MessageDto) -> Option<i64> {
    if is_synthetic_poll_msg_id(&msg.msg_id) {
        return None;
    }
    parse_message_poll_dto(msg)
        .map(|p| p.poll_id)
        .filter(|&id| id > 0)
}

fn candidate_thread_ids(thread_id: Option<&str>) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    if let Some(raw) = thread_id.map(str::trim).filter(|s| !s.is_empty()) {
        out.push(raw.to_string());
        let bare = clean_group_id(raw);
        if !bare.is_empty() && !out.iter().any(|t| t == bare) {
            out.push(bare.to_string());
        }
    }
    out
}

fn upsert_incoming_message(
    store: &mut MessageStore,
    msg: &mut MessageDto,
) -> Result<bool, String> {
    if let Some(poll_id) = real_poll_id_for_dedupe(msg) {
        let (merged, was_updated_existing) = store
            .upsert_or_merge_poll_message(&msg.thread_id, poll_id, msg)
            .map_err(|e| e.to_string())?;
        *msg = merged;
        Ok(was_updated_existing)
    } else {
        store.upsert_message(msg).map_err(|e| e.to_string())?;
        Ok(false)
    }
}

fn find_local_poll_message(
    store: &MessageStore,
    thread_id: Option<&str>,
    msg_id: Option<&str>,
    poll_id: i64,
) -> Option<MessageDto> {
    let mut tids = candidate_thread_ids(thread_id);
    if tids.is_empty() && poll_id > 0 {
        if let Ok(groups) = store.list_threads(FolderFilter::Group) {
            tids.extend(groups.into_iter().map(|t| t.id));
        }
    }

    if let Some(mid) = msg_id.map(str::trim).filter(|s| !s.is_empty()) {
        for tid in &tids {
            if let Ok(Some(msg)) = store.get_message(tid, mid) {
                return Some(msg);
            }
        }
    }

    if poll_id <= 0 {
        return None;
    }

    for tid in &tids {
        if let Ok(msgs) = store.load_messages(tid, None, 200) {
            let mut synthetic_match: Option<MessageDto> = None;
            for m in msgs.into_iter().rev() {
                if parse_message_poll_dto(&m).is_some_and(|p| p.poll_id == poll_id) {
                    if !is_synthetic_poll_msg_id(&m.msg_id) {
                        return Some(m);
                    }
                    if synthetic_match.is_none() {
                        synthetic_match = Some(m);
                    }
                }
            }
            if let Some(syn) = synthetic_match {
                return Some(syn);
            }
        }
        for syn_id in [format!("poll-{poll_id}"), format!("local-poll-{poll_id}")] {
            if let Ok(Some(msg)) = store.get_message(tid, &syn_id) {
                return Some(msg);
            }
        }
    }

    None
}

fn apply_poll_update_to_store(
    store: &mut MessageStore,
    thread_id: Option<&str>,
    msg_id: Option<&str>,
    poll: &PollDto,
) -> Option<(MessageDto, PollDto)> {
    let mut msg = find_local_poll_message(store, thread_id, msg_id, poll.poll_id)?;
    let merged = match parse_message_poll_dto(&msg) {
        Some(existing) => threads::merge_poll_dto(&existing, poll),
        None => poll.clone(),
    };
    let poll_json = serde_json::to_string(&merged).ok()?;
    msg.msg_type = "chat.poll".to_string();
    if (msg.text.trim().is_empty() || msg.text.trim() == "Bình chọn")
        && !merged.question.trim().is_empty()
        && merged.question != "Bình chọn"
    {
        msg.text = format!("Bình chọn: {}", merged.question);
    }
    msg.attachment_url = None;
    msg.attachment_name = Some(format!("teza-poll:{poll_json}"));
    store.upsert_message(&msg).ok()?;
    Some((msg, merged))
}

async fn update_local_poll_message(
    app: &AppHandle,
    store_arc: &Arc<Mutex<MessageStore>>,
    thread_id: Option<&str>,
    msg_id: Option<&str>,
    poll: &PollDto,
) -> Option<PollDto> {
    let updated = {
        let mut store = store_arc.lock().await;
        apply_poll_update_to_store(&mut store, thread_id, msg_id, poll)
    };
    if let Some((msg, merged)) = updated {
        let _ = app.emit("core://message-updated", &msg);
        Some(merged)
    } else {
        None
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
    let merged = update_local_poll_message(
        &app,
        &state.store,
        thread_id.as_deref(),
        msg_id.as_deref(),
        &poll,
    )
    .await;
    Ok(merged.unwrap_or(poll))
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
    let merged = update_local_poll_message(
        &app,
        &state.store,
        thread_id.as_deref(),
        msg_id.as_deref(),
        &poll,
    )
    .await;
    Ok(merged.unwrap_or(poll))
}

#[tauri::command]
async fn poll_add_options(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    poll_id: i64,
    new_options: Vec<String>,
    voted_option_ids: Vec<i64>,
    msg_id: Option<String>,
) -> Result<PollDto, String> {
    let clean_opts: Vec<String> = new_options
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if clean_opts.is_empty() {
        return Err("Lựa chọn mới không được để trống".into());
    }
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        return Err("Chế độ xem trước không đồng bộ bình chọn máy chủ".into());
    }
    let poll = match threads::add_poll_options(
        &sess_snapshot,
        poll_id,
        &clean_opts,
        &voted_option_ids,
    )
    .await
    {
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
            return Err(format!("Không thêm được lựa chọn bình chọn: {e}"));
        }
    };
    let merged = update_local_poll_message(
        &app,
        &state.store,
        Some(thread_id.as_str()),
        msg_id.as_deref(),
        &poll,
    )
    .await;
    Ok(merged.unwrap_or(poll))
}

#[tauri::command]
async fn poll_lock(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    poll_id: i64,
    msg_id: Option<String>,
) -> Result<PollDto, String> {
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        return Err("Chế độ xem trước không đồng bộ bình chọn máy chủ".into());
    }
    let poll = match threads::lock_poll(&sess_snapshot, poll_id).await {
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
            return Err(format!("Không khóa được bình chọn: {e}"));
        }
    };
    let merged = update_local_poll_message(
        &app,
        &state.store,
        Some(thread_id.as_str()),
        msg_id.as_deref(),
        &poll,
    )
    .await;
    Ok(merged.unwrap_or(poll))
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

type MessageSyncSig = (
    String,
    i64,
    bool,
    bool,
    Vec<(String, u32, bool)>,
    String,
    String,
);

fn message_sync_sig(m: &MessageDto) -> MessageSyncSig {
    (
        m.msg_id.clone(),
        m.ts,
        m.is_recalled,
        m.is_pinned,
        m.reactions
            .iter()
            .map(|r| (r.emoji.clone(), r.count, r.me))
            .collect(),
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
        let _ = upsert_incoming_message(&mut store, m);
    }
    if kind == ThreadKind::Group || thread_id.starts_with('g') {
        let _ = store.dedupe_duplicate_poll_messages_in_thread(thread_id);
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
        if before_ts.is_none()
            && (thread_id.starts_with('g')
                || store.get_thread_kind(&thread_id).ok().flatten() == Some(ThreadKind::Group))
        {
            let _ = store.dedupe_duplicate_poll_messages_in_thread(&thread_id);
        }
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
    if thread_id.starts_with('g')
        || store.get_thread_kind(&thread_id).ok().flatten() == Some(ThreadKind::Group)
    {
        let _ = store.dedupe_duplicate_poll_messages_in_thread(&thread_id);
    }
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
                        let _ = upsert_incoming_message(&mut store, &mut pm);
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
            cli_msg_id: None,
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
            cli_msg_id: None,
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

static FORWARD_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn effective_forward_attachment_name<'a>(
    src_attachment_name: Option<&'a str>,
    source_media_rec: Option<&'a MessageMediaRecord>,
) -> Option<&'a str> {
    src_attachment_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            source_media_rec
                .and_then(|r| r.original_name.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            source_media_rec
                .and_then(|r| r.sha256.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|_| "attachment.bin")
        })
}

async fn resolve_local_vault_media_by_msg(
    vault: &Arc<MediaVault>,
    store: &Arc<Mutex<MessageStore>>,
    msg_ref: Option<(&str, &str)>,
) -> Option<MediaObjectRecord> {
    let (tid, mid) = msg_ref?;
    let sha = {
        let guard = store.lock().await;
        guard
            .lookup_media_by_msg(tid, mid, "primary")
            .ok()
            .flatten()
            .map(|obj| obj.sha256)
    }?;
    try_rehydrate_local_hit(vault, store, &sha).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn message_forward(
    app: AppHandle,
    state: State<'_, AppState>,
    source_thread_id: String,
    source_msg_id: String,
    target_thread_id: String,
    fallback_text: Option<String>,
    fallback_msg_type: Option<String>,
    fallback_attachment_url: Option<String>,
    fallback_attachment_name: Option<String>,
) -> Result<MessageDto, String> {
    let target_tid = target_thread_id.trim().to_string();
    if target_tid.is_empty() {
        return Err("Mã hội thoại đích không được để trống".into());
    }
    let src_tid = source_thread_id.trim();
    let src_mid = source_msg_id.trim();

    let (stored_src, target_kind, source_media_rec) = {
        let store = state.store.lock().await;
        let src = if !src_tid.is_empty() && !src_mid.is_empty() {
            store.get_message(src_tid, src_mid).ok().flatten()
        } else {
            None
        };
        let kind = store
            .get_thread_kind(&target_tid)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let mm = if !src_tid.is_empty() && !src_mid.is_empty() {
            store
                .get_message_media_record(src_tid, src_mid, "primary")
                .ok()
                .flatten()
        } else {
            None
        };
        (src, kind, mm)
    };

    let clean_fb_text = fallback_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let clean_fb_type = fallback_msg_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let clean_fb_url = fallback_attachment_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let clean_fb_name = fallback_attachment_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let src = match stored_src {
        Some(mut m) => {
            if m.text.trim().is_empty() {
                if let Some(ft) = clean_fb_text {
                    m.text = ft;
                }
            }
            if m.msg_type.trim().is_empty() {
                if let Some(fmt) = clean_fb_type {
                    m.msg_type = fmt;
                }
            }
            if m.attachment_url
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                m.attachment_url = clean_fb_url;
            }
            if m.attachment_name
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                m.attachment_name = clean_fb_name;
            }
            m
        }
        None => MessageDto {
            msg_id: src_mid.to_string(),
            cli_msg_id: None,
            thread_id: src_tid.to_string(),
            sender_id: String::new(),
            sender_name: String::new(),
            text: clean_fb_text.unwrap_or_default(),
            msg_type: clean_fb_type.unwrap_or_else(|| "chat.text".to_string()),
            ts: 0,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: clean_fb_url,
            attachment_name: clean_fb_name,
        },
    };

    if src.is_recalled {
        return Err("Không thể chuyển tiếp tin nhắn đã thu hồi".into());
    }

    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let mt_lower = src.msg_type.trim().to_ascii_lowercase();
    let is_sticker = matches!(
        mt_lower.as_str(),
        "chat.sticker" | "sticker" | "5" | "35" | "36"
    ) || mt_lower.contains("sticker");

    // Branch A — Sticker
    if is_sticker {
        if let Some((cate_id, sticker_id)) = messaging::parse_sticker_ids_from_message(
            src.attachment_name.as_deref(),
            src.attachment_url.as_deref(),
        ) {
            let cid = u64::try_from(cate_id).unwrap_or(0);
            let sid = u64::try_from(sticker_id).unwrap_or(0);
            if sid > 0 {
                let msg = if sess_snapshot.is_authenticated() {
                    match messaging::send_sticker(
                        &sess_snapshot,
                        &target_tid,
                        target_kind,
                        sid,
                        cid,
                        1,
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
                            return Err(format!("Chuyển tiếp nhãn dán thất bại: {e}"));
                        }
                    }
                } else {
                    let now_ms = chrono_now_ms();
                    MessageDto {
                        msg_id: format!("local-{now_ms}"),
                        cli_msg_id: None,
                        thread_id: target_tid.clone(),
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
                        attachment_url: sticker_image_url(sid, cid)
                            .or_else(|| src.attachment_url.clone()),
                        attachment_name: Some(format!("sticker_{cid}_{sid}.png")),
                    }
                };
                return persist_and_emit_sent_message(&app, &state, msg).await;
            }
        }
        return Err("Không xác định được mã nhãn dán để chuyển tiếp".into());
    }

    // Branch B — Real Media / File Attachment
    let forward_att_name_for_check =
        effective_forward_attachment_name(src.attachment_name.as_deref(), source_media_rec.as_ref());
    if messaging::is_forwardable_media_message(
        &src.msg_type,
        src.attachment_url.as_deref(),
        forward_att_name_for_check,
    ) {
        let real_caption = messaging::extract_forward_caption(
            &src.msg_type,
            &src.text,
            src.attachment_name.as_deref(),
        );

        if !sess_snapshot.is_authenticated() {
            let file_name = forward_att_name_for_check.unwrap_or("attachment.bin");
            let mut demo = local_demo_attachment_message(&target_tid, file_name, &real_caption);
            if !src.msg_type.trim().is_empty() {
                demo.msg_type = src.msg_type.clone();
            }
            demo.attachment_url = src.attachment_url.clone();
            demo.attachment_name = src
                .attachment_name
                .clone()
                .or_else(|| Some(file_name.to_string()));
            return persist_and_emit_sent_message(&app, &state, demo).await;
        }

        let raw_url_opt = src
            .attachment_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if raw_url_opt.is_none() && source_media_rec.is_none() {
            return Err("Không tìm thấy tệp phương tiện gốc để chuyển tiếp".into());
        }

        let (effective_url, unwrapped_tid, unwrapped_mid) = match raw_url_opt {
            Some(raw_url) => unwrap_teza_media_url(raw_url)?,
            None => (String::new(), None, None),
        };
        let msg_ref_owned = if !src_tid.is_empty() && !src_mid.is_empty() {
            Some((src_tid.to_string(), src_mid.to_string()))
        } else {
            match (unwrapped_tid, unwrapped_mid) {
                (Some(t), Some(m)) => Some((t, m)),
                _ => None,
            }
        };

        let raw_att_name = forward_att_name_for_check.unwrap_or("");
        let kind_hint = {
            let from_att = infer_media_kind_for_attachment(raw_att_name, &src.msg_type);
            if from_att != "file" || effective_url.is_empty() {
                from_att
            } else {
                infer_media_kind_for_download(raw_att_name, &effective_url)
            }
        };

        let vault = state.media_vault.read().await.clone();
        let msg_ref_borrowed = msg_ref_owned
            .as_ref()
            .map(|(tid, mid)| (tid.as_str(), mid.as_str()));
        let obj = if let Some(local_obj) =
            resolve_local_vault_media_by_msg(&vault, &state.store, msg_ref_borrowed).await
        {
            local_obj
        } else if effective_url.is_empty() {
            let candidate =
                lookup_cloud_candidate(&state.store, "", kind_hint, msg_ref_borrowed).await;
            let relay_enabled = state.relay_cfg.lock().await.enabled;
            if let Some(cand) = candidate {
                if !relay_enabled && state.gdrive.is_connected_and_unlocked().await {
                    rehydrate_cloud_object(
                        Some(&app),
                        &vault,
                        &state.store,
                        &state.gdrive,
                        &state.relay_cfg,
                        &cand,
                        "",
                        kind_hint,
                        msg_ref_borrowed,
                    )
                    .await
                    .map_err(|e| {
                        format!("Không tải được phương tiện từ kho đám mây để chuyển tiếp: {e}")
                    })?
                } else {
                    return Err("Không tìm thấy tệp phương tiện gốc để chuyển tiếp".into());
                }
            } else {
                return Err("Không tìm thấy tệp phương tiện gốc để chuyển tiếp".into());
            }
        } else {
            resolve_media_with_tier2_gdrive(
                Some(&app),
                &vault,
                &sess_snapshot,
                &state.store,
                &state.gdrive,
                &state.relay_cfg,
                &effective_url,
                kind_hint,
                msg_ref_borrowed,
                |_, _| {},
            )
            .await
            .map_err(|e| format!("Không tải được phương tiện để chuyển tiếp: {e}"))?
        };
        messaging::validate_attachment_size(obj.size_bytes).map_err(|e| e.to_string())?;

        let cd_name = if let Some(name) = source_media_rec
            .as_ref()
            .and_then(|r| r.original_name.clone())
            .filter(|s| !s.trim().is_empty())
        {
            Some(name)
        } else {
            let store = state.store.lock().await;
            let from_msg = msg_ref_owned.as_ref().and_then(|(tid, mid)| {
                store
                    .get_message_media_record(tid, mid, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
            });
            from_msg.or_else(|| {
                if effective_url.is_empty() {
                    return None;
                }
                let url_hash = MediaVault::sha256_url(&effective_url);
                store
                    .get_message_media_record("__url__", &url_hash, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        store
                            .find_media_original_name_by_hash(&url_hash)
                            .ok()
                            .flatten()
                    })
            })
        };

        let effective_mime = if zalo_core::media_vault::is_generic_mime(&obj.mime) {
            vault
                .sniff_cached_mime(&obj)
                .await
                .map(str::to_string)
                .unwrap_or_else(|| obj.mime.clone())
        } else {
            obj.mime.clone()
        };
        let probe_bytes = read_vault_probe_bytes(&vault, &obj).await;
        let suggested_name = src
            .attachment_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(match kind_hint {
                "image" => "image",
                "video" => "video",
                "audio" => "voice",
                _ => "attachment",
            });
        let (resolved_name, _resolved_ext) = file_dialog::resolve_download_filename_and_ext(
            suggested_name,
            cd_name.as_deref(),
            &effective_url,
            &effective_url,
            &effective_mime,
            &probe_bytes,
        );

        let tmp_dir = state.paths.root.join("tmp_forward");
        zalo_core::data_dir::ensure_private_dir(&tmp_dir)
            .map_err(|e| format!("Không tạo được thư mục tạm chuyển tiếp: {e}"))?;
        let seq = FORWARD_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = tmp_dir.join(format!(
            "fwd-{}-{}-{seq}.bin",
            std::process::id(),
            chrono_now_ms()
        ));
        let tmp_guard = TempFileGuard::new(tmp_path);

        vault
            .export_to_path(&obj, tmp_guard.path())
            .await
            .map_err(|e| format!("Không xuất được phương tiện từ kho cục bộ: {e}"))?;

        let is_photo_or_gif = matches!(
            mt_lower.as_str(),
            "chat.photo" | "photo" | "2" | "chat.gif" | "gif"
        );
        let force_image = if is_photo_or_gif && messaging::is_image_extension(&resolved_name) {
            Some(true)
        } else {
            None
        };

        let msg = match messaging::send_attachment_file_named(
            &sess_snapshot,
            &target_tid,
            target_kind,
            tmp_guard.path(),
            &resolved_name,
            &real_caption,
            force_image,
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
                return Err(format!("Chuyển tiếp tệp thất bại: {e}"));
            }
        };

        let _ = tokio::fs::remove_file(tmp_guard.path()).await;
        drop(tmp_guard);

        let saved = persist_and_emit_sent_message(&app, &state, msg).await?;
        {
            let store = state.store.lock().await;
            let url_hash = saved
                .attachment_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(MediaVault::sha256_url)
                .unwrap_or_default();
            let _ = store.link_message_media(&MessageMediaRecord {
                thread_id: saved.thread_id.clone(),
                msg_id: saved.msg_id.clone(),
                variant: "primary".to_string(),
                url_hash,
                sha256: Some(obj.sha256.clone()),
                original_name: Some(resolved_name),
                saved_user_path: None,
                cdn_status: MEDIA_CDN_STATUS_OK,
                updated_ts: chrono_now_ms(),
            });
        }
        if obj.cloud_file_id.is_none() {
            state.gdrive.notify_sync();
        }
        return Ok(saved);
    }

    // Branch C — Text / Links / Cards (never append CDN thumbnail URLs)
    let forward_text = src.text.trim();
    if forward_text.is_empty() {
        return Err("Tin nhắn không có nội dung để chuyển tiếp".into());
    }

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_text(&sess_snapshot, &target_tid, target_kind, forward_text).await {
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
                return Err(format!("Chuyển tiếp tin nhắn thất bại: {e}"));
            }
        }
    } else {
        let now_ms = chrono_now_ms();
        MessageDto {
            msg_id: format!("local-{now_ms}"),
            cli_msg_id: None,
            thread_id: target_tid,
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: forward_text.to_string(),
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
async fn message_react(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    msg_id: String,
    emoji: String,
) -> Result<MessageDto, String> {
    let canonical_emoji = zalo_core::chat::parser::canonicalize_reaction_emoji(&emoji)
        .unwrap_or_else(|| emoji.trim().to_string());
    if canonical_emoji.is_empty() {
        return Err("Biểu tượng cảm xúc không hợp lệ".to_string());
    }
    let (kind, msg) = {
        let store = state.store.lock().await;
        let msg = store
            .get_message_by_ids(&thread_id, &msg_id, None)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Không tìm thấy tin nhắn".to_string())?;
        let kind = store
            .get_thread_kind(&msg.thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        (kind, msg)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let remove = msg.reactions.iter().any(|r| {
        r.me && (r.emoji == canonical_emoji
            || zalo_core::chat::parser::canonicalize_reaction_emoji(&r.emoji).as_deref()
                == Some(canonical_emoji.as_str()))
    });
    if sess_snapshot.is_authenticated() {
        if let Err(e) = messaging::send_reaction(
            &sess_snapshot,
            &msg.thread_id,
            kind,
            &msg,
            &canonical_emoji,
            remove,
        )
        .await
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
        .apply_reaction_event(
            &msg.thread_id,
            &msg.msg_id,
            msg.cli_msg_id.as_deref(),
            None,
            Some(&canonical_emoji),
            true,
            remove,
        )
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
    let (updated, orphaned_paths) = {
        let mut store = state.store.lock().await;
        let orphans = store
            .cleanup_orphaned_media_for_msg(&thread_id, &msg_id)
            .unwrap_or_default();
        let updated = store
            .mark_recalled(&thread_id, &msg_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Không cập nhật được trạng thái thu hồi".to_string())?;
        (updated, orphans)
    };
    if !orphaned_paths.is_empty() {
        let vault = state.media_vault.read().await.clone();
        vault.unlink_rel_paths(&orphaned_paths).await;
    }
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
    let orphaned_paths = {
        let mut store = state.store.lock().await;
        let orphans = store
            .cleanup_orphaned_media_for_msg(&thread_id, &msg_id)
            .unwrap_or_default();
        store
            .delete_message(&thread_id, &msg_id)
            .map_err(|e| e.to_string())?;
        orphans
    };
    if !orphaned_paths.is_empty() {
        let vault = state.media_vault.read().await.clone();
        vault.unlink_rel_paths(&orphaned_paths).await;
    }
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
            cli_msg_id: None,
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
        cli_msg_id: None,
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
async fn pick_attachment_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<AttachmentMetaDto>, String> {
    let owner_hwnd = main_window_hwnd(&app);

    let selected = tokio::task::spawn_blocking(move || file_dialog::pick_single_file(owner_hwnd))
        .await
        .map_err(|e| format!("Lỗi tác vụ chọn tệp: {e}"))??;
    let Some(path) = selected else {
        return Ok(None);
    };
    let meta = attachment_meta_from_path(&path)?;
    {
        let mut approved = state
            .approved_attachment_paths
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        record_approved_attachment_path(&mut approved, &path);
    }
    Ok(Some(meta))
}

fn main_window_hwnd(app: &AppHandle) -> Option<usize> {
    #[cfg(target_os = "windows")]
    {
        app.get_window("main")
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
const MAX_APPROVED_ATTACHMENT_PATHS: usize = 200;

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

fn record_approved_attachment_path(list: &mut VecDeque<PathBuf>, path: &Path) -> PathBuf {
    let canonical = normalize_saved_lookup_path(path);
    if let Some(pos) = list.iter().position(|p| p == &canonical) {
        list.remove(pos);
    }
    list.push_back(canonical.clone());
    while list.len() > MAX_APPROVED_ATTACHMENT_PATHS {
        list.pop_front();
    }
    canonical
}

fn validate_approved_attachment_path(
    list: &VecDeque<PathBuf>,
    candidate: &Path,
) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(candidate).map_err(|_| {
        "Tệp không nằm trong danh sách tệp đã chọn hoặc kéo thả trong phiên này".to_string()
    })?;
    list.iter()
        .find(|p| *p == &canonical)
        .cloned()
        .ok_or_else(|| {
            "Tệp không nằm trong danh sách tệp đã chọn hoặc kéo thả trong phiên này".to_string()
        })
}

#[cfg(target_os = "windows")]
fn mark_of_the_web_ads_path(path: &Path) -> PathBuf {
    let mut ads = path.as_os_str().to_os_string();
    ads.push(":Zone.Identifier");
    PathBuf::from(ads)
}

#[tauri::command]
async fn save_remote_file(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    suggested_name: String,
    download_id: Option<String>,
    thread_id: Option<String>,
    msg_id: Option<String>,
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
    let mut msg_ref_owned = match (
        thread_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        msg_id.as_deref().map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(tid), Some(mid)) => Some((tid.to_string(), mid.to_string())),
        _ => None,
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

    enum SaveSource {
        InlineData(Vec<u8>),
        VaultObject {
            vault: Arc<MediaVault>,
            obj: Box<MediaObjectRecord>,
        },
    }

    let trimmed = url.trim_start();
    let is_data_url = trimmed
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("data:"));

    let (source, bytes_len, resolved_name, resolved_ext) = if is_data_url {
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
        let (resolved_name, resolved_ext) = file_dialog::resolve_download_filename_and_ext(
            &suggested_name,
            None,
            "",
            "",
            &mime,
            &data,
        );
        (
            SaveSource::InlineData(data),
            total,
            resolved_name,
            resolved_ext,
        )
    } else {
        let (effective_url, unwrapped_tid, unwrapped_mid) = match unwrap_teza_media_url(trimmed) {
            Ok(v) => v,
            Err(msg) => {
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
        if msg_ref_owned.is_none() {
            if let (Some(tid), Some(mid)) = (unwrapped_tid, unwrapped_mid) {
                msg_ref_owned = Some((tid, mid));
            }
        }
        let vault = state.media_vault.read().await.clone();
        let sess_snapshot = {
            let sess = state.session.lock().await;
            sess.clone_shallow()
        };
        let kind_hint = infer_media_kind_for_download(&suggested_name, &effective_url);
        let app_prog = app.clone();
        let dl_id_prog = dl_id.clone();
        let initial_name_prog = initial_name.clone();
        let msg_ref_borrowed = msg_ref_owned
            .as_ref()
            .map(|(tid, mid)| (tid.as_str(), mid.as_str()));

        let obj = match resolve_media_with_tier2_gdrive(
            Some(&app),
            &vault,
            &sess_snapshot,
            &state.store,
            &state.gdrive,
            &state.relay_cfg,
            &effective_url,
            kind_hint,
            msg_ref_borrowed,
            move |bytes, total| {
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
            },
        )
        .await
        {
            Ok(obj) => obj,
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

        let total = obj.size_bytes;
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

        let cd_name = {
            let store = state.store.lock().await;
            let from_msg = msg_ref_owned.as_ref().and_then(|(tid, mid)| {
                store
                    .get_message_media_record(tid, mid, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
            });
            from_msg.or_else(|| {
                let url_hash = MediaVault::sha256_url(&effective_url);
                store
                    .get_message_media_record("__url__", &url_hash, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        store
                            .find_media_original_name_by_hash(&url_hash)
                            .ok()
                            .flatten()
                    })
            })
        };

        let probe_bytes = read_vault_probe_bytes(&vault, &obj).await;
        let (resolved_name, resolved_ext) = file_dialog::resolve_download_filename_and_ext(
            &suggested_name,
            cd_name.as_deref(),
            &effective_url,
            &effective_url,
            &obj.mime,
            &probe_bytes,
        );
        (
            SaveSource::VaultObject {
                vault,
                obj: Box::new(obj),
            },
            total,
            resolved_name,
            resolved_ext,
        )
    };

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

    let write_res: Result<(), String> = match source {
        SaveSource::InlineData(bytes) => tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| format!("Không ghi được tệp: {e}")),
        SaveSource::VaultObject { vault, obj } => vault
            .export_to_path(&obj, &path)
            .await
            .map(|_| ())
            .map_err(|e| format!("Không xuất được tệp từ kho cục bộ: {e}")),
    };
    if let Err(msg) = write_res {
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

    #[cfg(target_os = "windows")]
    {
        let ads_path = mark_of_the_web_ads_path(&path);
        let _ = tokio::fs::write(&ads_path, b"[ZoneTransfer]\r\nZoneId=3\r\n").await;
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

    if let Some((tid, mid)) = msg_ref_owned {
        let store = state.store.lock().await;
        let _ = store.set_message_saved_path(&tid, &mid, &path_str);
    }

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

const MAX_PASSTHROUGH_DATA_URL_BYTES: usize = 15 * 1024 * 1024;
const MAX_SEARCH_GLOBAL_LIMIT: usize = 200;
const MAX_TRANSLATE_BATCH_ITEMS: usize = 100;

fn validate_media_data_url_passthrough(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if !trimmed
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("data:"))
    {
        return Ok(None);
    }
    if trimmed.len() > MAX_PASSTHROUGH_DATA_URL_BYTES {
        return Err("Dữ liệu media quá lớn (tối đa 15 MB)".into());
    }
    let Some((meta, _payload)) = trimmed[5..].split_once(',') else {
        return Err("Data URL không hợp lệ".into());
    };
    if meta.chars().any(char::is_control) {
        return Err("Data URL không hợp lệ".into());
    }
    let mime = meta
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime == "image/svg+xml" || mime.contains("svg") {
        return Err("Không hỗ trợ định dạng SVG trong data URL".into());
    }
    let allowed = (mime.starts_with("image/")
        || mime.starts_with("audio/")
        || mime.starts_with("video/"))
        && mime.split_once('/').is_some_and(|(_, sub)| !sub.is_empty());
    if !allowed {
        return Err("Chỉ hỗ trợ data URL hình ảnh, âm thanh hoặc video".into());
    }
    Ok(Some(trimmed.to_string()))
}

fn clamp_search_global_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(30).clamp(1, MAX_SEARCH_GLOBAL_LIMIT)
}

fn validate_translate_batch_len(len: usize) -> Result<(), String> {
    if len > MAX_TRANSLATE_BATCH_ITEMS {
        return Err(format!(
            "Số lượng tin nhắn cần dịch vượt quá giới hạn tối đa ({MAX_TRANSLATE_BATCH_ITEMS} tin nhắn mỗi lượt)"
        ));
    }
    Ok(())
}

#[tauri::command]
async fn fetch_image_data_url(
    state: State<'_, AppState>,
    url: String,
) -> Result<String, String> {
    if let Some(passthrough) = validate_media_data_url_passthrough(&url)? {
        return Ok(passthrough);
    }
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };
    let data_url = sess_snapshot
        .download_media_data_url(&url, 25 * 1024 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    if data_url
        .get(..19)
        .is_some_and(|p| p.eq_ignore_ascii_case("data:image/svg+xml"))
    {
        return Err("Không hỗ trợ định dạng SVG".into());
    }
    Ok(data_url)
}

#[tauri::command]
async fn inspect_attachment_path(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<AttachmentMetaDto, String> {
    if file_path.trim().is_empty() {
        return Err("Đường dẫn tệp trống".into());
    }
    let canonical = {
        let approved = state
            .approved_attachment_paths
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        validate_approved_attachment_path(&approved, Path::new(&file_path))?
    };
    let mut meta = attachment_meta_from_path(&canonical)?;
    let display = file_dialog::normalize_os_display_path(&canonical);
    if display.to_string_lossy().len() < 240 {
        meta.path = display.to_string_lossy().into_owned();
    }
    Ok(meta)
}

#[tauri::command]
async fn message_send_attachment_path(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    file_path: String,
    caption: String,
) -> Result<MessageDto, String> {
    if file_path.trim().is_empty() {
        return Err("Đường dẫn tệp trống".into());
    }
    let path = {
        let approved = state
            .approved_attachment_paths
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        validate_approved_attachment_path(&approved, Path::new(&file_path))?
    };
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

    let saved = persist_and_emit_sent_message(&app, &state, msg).await?;

    let vault = state.media_vault.read().await.clone();
    let store_arc = state.store.clone();
    let gdrive_arc = state.gdrive.clone();
    let src_path = path;
    let kind = infer_media_kind_for_attachment(&meta.file_name, &saved.msg_type);
    let mime = infer_mime_from_filename_and_bytes(&meta.file_name, None);
    let url_opt = saved.attachment_url.clone();
    let tid = saved.thread_id.clone();
    let mid = saved.msg_id.clone();
    let orig_name = saved
        .attachment_name
        .clone()
        .or_else(|| Some(meta.file_name.clone()));
    tauri::async_runtime::spawn(async move {
        if let Ok(obj) = vault
            .ingest_local_file(
                &store_arc,
                &src_path,
                kind,
                mime,
                url_opt.as_deref(),
                Some((tid.as_str(), mid.as_str())),
                orig_name.as_deref(),
            )
            .await
        {
            if obj.cloud_file_id.is_none() {
                gdrive_arc.notify_sync();
            }
        }
    });

    Ok(saved)
}

static STAGED_UPLOAD_SEQ: AtomicU64 = AtomicU64::new(1);

fn sanitize_staged_upload_filename(raw: &str) -> String {
    let seg = raw
        .trim()
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .unwrap_or("");
    let cleaned: String = seg
        .chars()
        .filter(|c| !c.is_control() && *c != '\0')
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "clipboard.png".to_string()
    } else {
        trimmed.to_string()
    }
}

fn prune_staged_raw_uploads(
    map: &mut HashMap<String, StagedRawUpload>,
    now_ms: i64,
    reserve_slot: bool,
) -> Vec<PathBuf> {
    let mut evicted = Vec::new();
    let expired_keys: Vec<String> = map
        .iter()
        .filter(|(_, entry)| now_ms.saturating_sub(entry.created_ms) > STAGED_RAW_UPLOAD_TTL_MS)
        .map(|(k, _)| k.clone())
        .collect();
    for key in expired_keys {
        if let Some(removed) = map.remove(&key) {
            evicted.push(removed.path);
        }
    }
    let limit = if reserve_slot {
        MAX_STAGED_RAW_UPLOADS.saturating_sub(1)
    } else {
        MAX_STAGED_RAW_UPLOADS
    };
    while map.len() > limit {
        let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, entry)| entry.created_ms)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        if let Some(removed) = map.remove(&oldest_key) {
            evicted.push(removed.path);
        }
    }
    evicted
}

async fn send_raw_bytes_attachment_inner(
    app: &AppHandle,
    state: &State<'_, AppState>,
    thread_id: String,
    file_name: String,
    caption: String,
    bytes: Vec<u8>,
) -> Result<MessageDto, String> {
    let tid_trimmed = thread_id.trim().to_string();
    if tid_trimmed.is_empty() {
        return Err("Thiếu mã hội thoại".into());
    }
    if bytes.is_empty() {
        return Err("Dữ liệu tệp đính kèm trống".into());
    }
    messaging::validate_attachment_size(bytes.len() as u64).map_err(|e| e.to_string())?;
    let safe_name = sanitize_staged_upload_filename(&file_name);

    let kind = {
        let store = state.store.lock().await;
        store
            .get_thread_kind(&tid_trimmed)
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
            &tid_trimmed,
            kind,
            &safe_name,
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
                        app,
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
        local_demo_attachment_message(&tid_trimmed, &safe_name, &caption)
    };

    let saved = persist_and_emit_sent_message(app, state, msg).await?;

    let vault = state.media_vault.read().await.clone();
    let store_arc = state.store.clone();
    let gdrive_arc = state.gdrive.clone();
    let media_kind = infer_media_kind_for_attachment(&safe_name, &saved.msg_type);
    let mime = infer_mime_from_filename_and_bytes(&safe_name, Some(&bytes));
    let url_opt = saved.attachment_url.clone();
    let tid = saved.thread_id.clone();
    let mid = saved.msg_id.clone();
    let orig_name = saved
        .attachment_name
        .clone()
        .or_else(|| Some(safe_name.clone()));
    tauri::async_runtime::spawn(async move {
        if let Ok(obj) = vault
            .ingest_bytes(
                &store_arc,
                &bytes,
                media_kind,
                mime,
                url_opt.as_deref(),
                Some((tid.as_str(), mid.as_str())),
                orig_name.as_deref(),
            )
            .await
        {
            if obj.cloud_file_id.is_none() {
                gdrive_arc.notify_sync();
            }
        }
    });

    Ok(saved)
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

    let bytes =
        extract_invoke_body_bytes(request.body(), messaging::MAX_ATTACHMENT_BYTES as usize)?
            .into_owned();
    send_raw_bytes_attachment_inner(&app, &state, thread_id, file_name, caption, bytes).await
}

#[tauri::command]
async fn message_send_base64_attachment(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
    file_name: String,
    caption: String,
    data_base64: String,
) -> Result<MessageDto, String> {
    let trimmed = data_base64.trim();
    if trimmed.is_empty() {
        return Err("Dữ liệu tệp đính kèm trống".into());
    }
    let max_b64_len = (messaging::MAX_ATTACHMENT_BYTES as usize)
        .div_ceil(3)
        .saturating_mul(4);
    if trimmed.len() > max_b64_len {
        return Err("Tệp vượt quá giới hạn kích thước cho phép".into());
    }
    let bytes = B64_STANDARD
        .decode(trimmed)
        .map_err(|e| format!("Dữ liệu tệp base64 không hợp lệ: {e}"))?;
    if bytes.is_empty() {
        return Err("Dữ liệu tệp đính kèm trống".into());
    }
    send_raw_bytes_attachment_inner(&app, &state, thread_id, file_name, caption, bytes).await
}

#[tauri::command]
async fn raw_attachment_stage_begin(
    state: State<'_, AppState>,
    file_name: String,
) -> Result<String, String> {
    let safe_name = sanitize_staged_upload_filename(&file_name);
    let tmp_dir = state.paths.root.join("tmp_uploads");
    zalo_core::data_dir::ensure_private_dir(&tmp_dir)
        .map_err(|e| format!("Không tạo được thư mục tải tệp tạm: {e}"))?;

    let now_ms = chrono_now_ms();
    let seq = STAGED_UPLOAD_SEQ.fetch_add(1, Ordering::Relaxed);
    let upload_id = format!("up-{now_ms}-{}-{seq}", std::process::id());
    let tmp_path = tmp_dir.join(format!("{upload_id}.part"));

    tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .await
        .map_err(|e| format!("Không tạo được tệp tạm tải lên: {e}"))?;

    let evicted_paths = {
        let mut map = state
            .staged_raw_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let evicted = prune_staged_raw_uploads(&mut map, now_ms, true);
        map.insert(
            upload_id.clone(),
            StagedRawUpload {
                path: tmp_path,
                file_name: safe_name,
                bytes_written: 0,
                created_ms: now_ms,
            },
        );
        evicted
    };

    for path in evicted_paths {
        let _ = tokio::fs::remove_file(path).await;
    }

    Ok(upload_id)
}

#[tauri::command]
async fn raw_attachment_stage_chunk(
    state: State<'_, AppState>,
    upload_id: String,
    chunk_base64: String,
) -> Result<u64, String> {
    let clean_id = upload_id.trim();
    if clean_id.is_empty() {
        return Err("Thiếu mã phiên tải tệp tạm".into());
    }
    let trimmed = chunk_base64.trim();
    let max_b64_len = (messaging::MAX_ATTACHMENT_BYTES as usize)
        .div_ceil(3)
        .saturating_mul(4);
    if trimmed.len() > max_b64_len {
        return Err("Phần dữ liệu tệp vượt quá giới hạn kích thước cho phép".into());
    }
    let chunk = B64_STANDARD
        .decode(trimmed)
        .map_err(|e| format!("Phần dữ liệu base64 không hợp lệ: {e}"))?;

    let (staged_path, current_bytes) = {
        let map = state
            .staged_raw_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = map
            .get(clean_id)
            .ok_or_else(|| "Không tìm thấy phiên tải tệp tạm".to_string())?;
        (entry.path.clone(), entry.bytes_written)
    };

    let chunk_len = chunk.len() as u64;
    let projected = current_bytes
        .checked_add(chunk_len)
        .ok_or_else(|| "Kích thước tệp vượt quá giới hạn cho phép".to_string())?;
    if projected > messaging::MAX_ATTACHMENT_BYTES {
        return Err("Tệp vượt quá giới hạn kích thước cho phép".into());
    }

    if chunk.is_empty() {
        return Ok(current_bytes);
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(&staged_path)
        .await
        .map_err(|e| format!("Không mở được tệp tạm để ghi dữ liệu: {e}"))?;
    file.write_all(&chunk)
        .await
        .map_err(|e| format!("Không ghi được dữ liệu tệp tạm: {e}"))?;
    file.flush()
        .await
        .map_err(|e| format!("Không đồng bộ được dữ liệu tệp tạm: {e}"))?;
    drop(file);

    let (updated_total, overflow_path) = {
        let mut map = state
            .staged_raw_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get_mut(clean_id) else {
            return Err("Phiên tải tệp tạm đã kết thúc hoặc bị hủy".into());
        };
        let next = entry.bytes_written.saturating_add(chunk_len);
        if next > messaging::MAX_ATTACHMENT_BYTES {
            let removed = map.remove(clean_id).map(|s| s.path);
            (0, removed)
        } else {
            entry.bytes_written = next;
            (next, None)
        }
    };

    if let Some(path) = overflow_path {
        let _ = tokio::fs::remove_file(path).await;
        return Err("Tệp vượt quá giới hạn kích thước cho phép".into());
    }

    Ok(updated_total)
}

#[tauri::command]
async fn raw_attachment_stage_send(
    app: AppHandle,
    state: State<'_, AppState>,
    upload_id: String,
    thread_id: String,
    caption: String,
) -> Result<MessageDto, String> {
    let tid_trimmed = thread_id.trim().to_string();
    if tid_trimmed.is_empty() {
        return Err("Thiếu mã hội thoại".into());
    }
    let clean_id = upload_id.trim();
    if clean_id.is_empty() {
        return Err("Thiếu mã phiên tải tệp tạm".into());
    }

    let staged = {
        let mut map = state
            .staged_raw_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.remove(clean_id)
            .ok_or_else(|| "Không tìm thấy phiên tải tệp tạm".to_string())?
    };
    let tmp_guard = TempFileGuard::new(staged.path);

    if staged.bytes_written == 0 {
        return Err("Dữ liệu tệp đính kèm trống".into());
    }
    messaging::validate_attachment_size(staged.bytes_written).map_err(|e| e.to_string())?;

    let kind = {
        let store = state.store.lock().await;
        store
            .get_thread_kind(&tid_trimmed)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };
    let sess_snapshot = {
        let sess = state.session.lock().await;
        sess.clone_shallow()
    };

    let msg = if sess_snapshot.is_authenticated() {
        match messaging::send_attachment_file_named(
            &sess_snapshot,
            &tid_trimmed,
            kind,
            tmp_guard.path(),
            &staged.file_name,
            &caption,
            None,
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
        local_demo_attachment_message(&tid_trimmed, &staged.file_name, &caption)
    };

    let saved = persist_and_emit_sent_message(&app, &state, msg).await?;

    let head_probe = {
        let mut buf = [0u8; 64];
        match tokio::fs::File::open(tmp_guard.path()).await {
            Ok(mut f) => {
                use tokio::io::AsyncReadExt;
                let n = f.read(&mut buf).await.unwrap_or(0);
                buf[..n].to_vec()
            }
            Err(_) => Vec::new(),
        }
    };
    let vault = state.media_vault.read().await.clone();
    let media_kind = infer_media_kind_for_attachment(&staged.file_name, &saved.msg_type);
    let mime = infer_mime_from_filename_and_bytes(
        &staged.file_name,
        (!head_probe.is_empty()).then_some(head_probe.as_slice()),
    );
    let url_opt = saved.attachment_url.clone();
    let orig_name = saved
        .attachment_name
        .clone()
        .or_else(|| Some(staged.file_name.clone()));
    if let Ok(obj) = vault
        .ingest_local_file(
            &state.store,
            tmp_guard.path(),
            media_kind,
            mime,
            url_opt.as_deref(),
            Some((saved.thread_id.as_str(), saved.msg_id.as_str())),
            orig_name.as_deref(),
        )
        .await
    {
        if obj.cloud_file_id.is_none() {
            state.gdrive.notify_sync();
        }
    }

    let _ = tokio::fs::remove_file(tmp_guard.path()).await;
    drop(tmp_guard);

    Ok(saved)
}

#[tauri::command]
async fn raw_attachment_stage_abort(
    state: State<'_, AppState>,
    upload_id: String,
) -> Result<(), String> {
    let clean_id = upload_id.trim();
    if clean_id.is_empty() {
        return Ok(());
    }
    let removed = {
        let mut map = state
            .staged_raw_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.remove(clean_id)
    };
    if let Some(staged) = removed {
        let _ = tokio::fs::remove_file(staged.path).await;
    }
    Ok(())
}

#[tauri::command]
async fn search_global(
    state: State<'_, AppState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<MessageDto>, String> {
    let capped_limit = clamp_search_global_limit(limit);
    let store = state.store.lock().await;
    store
        .search_messages(&query, capped_limit)
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
    let mut staged = cfg.clone();
    staged.apply_update(update);
    staged
        .save_to_file(&state.paths.translate_file())
        .map_err(|e| format!("Không lưu được cấu hình dịch: {e}"))?;
    *cfg = staged;
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
    validate_translate_batch_len(items.len())?;
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
            state.media_vault.clone(),
            state.ws_task.clone(),
            state.active_thread_id.clone(),
            state.call_mgr.clone(),
            state.paths.clone(),
        )
        .await;
    }
    if summary.enabled || !is_authed {
        state
            .call_mgr
            .teardown_for_session(Some(&app), &state.session)
            .await;
    } else {
        let _ = state
            .call_mgr
            .restart_from_session(&app, &state.session, &state.store)
            .await;
    }
    if summary.enabled {
        state
            .gdrive
            .pause_for_relay_and_emit(&app, &state.store)
            .await;
    } else {
        state
            .gdrive
            .resume_after_relay_and_emit(&app, &state.store)
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
async fn call_get_state(state: State<'_, AppState>) -> Result<call::CallStateDto, String> {
    let (authed, relay_enabled) = {
        let sess = state.session.lock().await;
        (sess.is_authenticated(), sess.relay.enabled)
    };
    Ok(state
        .call_mgr
        .get_state(authed, relay_enabled, &state.store)
        .await)
}

#[tauri::command]
async fn call_list_devices(state: State<'_, AppState>) -> Result<call::CallDevicesDto, String> {
    state.call_mgr.list_devices().await
}

#[tauri::command]
async fn call_set_devices(
    state: State<'_, AppState>,
    input_device: Option<String>,
    output_device: Option<String>,
) -> Result<call::CallDevicesDto, String> {
    state
        .call_mgr
        .set_devices(input_device, output_device)
        .await
}

#[tauri::command]
async fn call_test_audio(state: State<'_, AppState>) -> Result<call::CallAudioTestResultDto, String> {
    state.call_mgr.test_audio().await
}

#[tauri::command]
async fn call_start_voice(
    app: AppHandle,
    state: State<'_, AppState>,
    partner_id: String,
    partner_name: Option<String>,
) -> Result<call::CallStateDto, String> {
    state
        .call_mgr
        .start_outgoing_call_for_session(
            &app,
            &state.session,
            &state.store,
            partner_id,
            partner_name,
            false,
        )
        .await
}

#[tauri::command]
async fn call_start_video(
    app: AppHandle,
    state: State<'_, AppState>,
    partner_id: String,
    partner_name: Option<String>,
) -> Result<call::CallStateDto, String> {
    state
        .call_mgr
        .start_outgoing_call_for_session(
            &app,
            &state.session,
            &state.store,
            partner_id,
            partner_name,
            true,
        )
        .await
}

#[tauri::command]
async fn call_answer(
    state: State<'_, AppState>,
    call_id: Option<i64>,
) -> Result<call::CallStateDto, String> {
    {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        if sess.relay.enabled {
            return Err(teza_call::CallError::RelayUnsupported.to_string());
        }
    }
    let eng = state
        .call_mgr
        .active_engine()
        .await
        .ok_or_else(|| "Không có cuộc gọi đến nào đang hoạt động".to_string())?;
    eng.answer_call_by_id(call_id, true)
        .await
        .map_err(|e| e.to_string())?;
    Ok(state
        .call_mgr
        .get_state(true, false, &state.store)
        .await)
}

#[tauri::command]
async fn call_decline(
    state: State<'_, AppState>,
    call_id: Option<i64>,
) -> Result<call::CallStateDto, String> {
    {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        if sess.relay.enabled {
            return Err(teza_call::CallError::RelayUnsupported.to_string());
        }
    }
    if let Some(eng) = state.call_mgr.active_engine().await {
        eng.answer_call_by_id(call_id, false)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(state
        .call_mgr
        .get_state(true, false, &state.store)
        .await)
}

#[tauri::command]
async fn call_hangup(app: AppHandle, state: State<'_, AppState>) -> Result<call::CallStateDto, String> {
    let (authed, relay_enabled) = {
        let sess = state.session.lock().await;
        (sess.is_authenticated(), sess.relay.enabled)
    };
    if relay_enabled || !authed {
        state
            .call_mgr
            .teardown_for_session(Some(&app), &state.session)
            .await;
        let (now_authed, now_relay) = {
            let sess = state.session.lock().await;
            (sess.is_authenticated(), sess.relay.enabled)
        };
        return Ok(state
            .call_mgr
            .get_state(now_authed, now_relay, &state.store)
            .await);
    }
    if let Some(eng) = state.call_mgr.active_engine().await {
        eng.end_call().await.map_err(|e| e.to_string())?;
    }
    Ok(state
        .call_mgr
        .get_state(true, false, &state.store)
        .await)
}

#[tauri::command]
async fn call_set_mute(
    state: State<'_, AppState>,
    muted: bool,
) -> Result<call::CallStateDto, String> {
    {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        if sess.relay.enabled {
            return Err(teza_call::CallError::RelayUnsupported.to_string());
        }
    }
    let eng = state
        .call_mgr
        .active_engine()
        .await
        .ok_or_else(|| "Không có cuộc gọi nào đang hoạt động".to_string())?;
    eng.set_mic_muted(muted).await.map_err(|e| e.to_string())?;
    Ok(state
        .call_mgr
        .get_state(true, false, &state.store)
        .await)
}

#[tauri::command]
async fn call_set_camera(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<call::CallStateDto, String> {
    {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() {
            return Err("Chưa đăng nhập".into());
        }
        if sess.relay.enabled {
            return Err(teza_call::CallError::RelayUnsupported.to_string());
        }
    }
    let eng = state
        .call_mgr
        .active_engine()
        .await
        .ok_or_else(|| "Không có cuộc gọi nào đang hoạt động".to_string())?;
    eng.set_cam_enabled(enabled)
        .await
        .map_err(|e| e.to_string())?;
    Ok(state
        .call_mgr
        .get_state(true, false, &state.store)
        .await)
}

#[tauri::command]
async fn call_push_video_frame(
    state: State<'_, AppState>,
    call_id: i64,
    data_b64: String,
) -> Result<bool, String> {
    state
        .call_mgr
        .push_outbound_video_frame_for_session(&state.session, call_id, &data_b64)
        .await
}

#[tauri::command]
async fn call_request_video_keyframe(
    state: State<'_, AppState>,
    call_id: Option<i64>,
) -> Result<bool, String> {
    {
        let sess = state.session.lock().await;
        if !sess.is_authenticated() || sess.relay.enabled {
            return Ok(false);
        }
    }
    Ok(state
        .call_mgr
        .request_remote_video_keyframe(call_id)
        .await)
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
        InvokeBody::Json(serde_json::Value::Object(map)) => {
            if map.is_empty() {
                return Err("Dữ liệu nhị phân IPC không hợp lệ".into());
            }
            if map.len() > max_bytes {
                return Err("Dữ liệu vượt quá giới hạn kích thước cho phép".into());
            }
            let mut bytes = Vec::with_capacity(map.len());
            for idx in 0..map.len() {
                let key = idx.to_string();
                let b = map
                    .get(&key)
                    .and_then(|v| v.as_u64())
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| "Dữ liệu nhị phân IPC không hợp lệ".to_string())?;
                bytes.push(b);
            }
            Ok(std::borrow::Cow::Owned(bytes))
        }
        InvokeBody::Json(serde_json::Value::String(b64)) => {
            let trimmed = b64.trim();
            if trimmed.is_empty() {
                return Err("Dữ liệu nhị phân IPC không hợp lệ".into());
            }
            let max_b64_len = max_bytes.div_ceil(3).saturating_mul(4);
            if trimmed.len() > max_b64_len {
                return Err("Dữ liệu vượt quá giới hạn kích thước cho phép".into());
            }
            let bytes = B64_STANDARD
                .decode(trimmed)
                .map_err(|_| "Dữ liệu nhị phân IPC không hợp lệ".to_string())?;
            if bytes.len() > max_bytes {
                return Err("Dữ liệu vượt quá giới hạn kích thước cho phép".into());
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
async fn clipboard_copy_rgba_image_b64(
    width: u32,
    height: u32,
    rgba_base64: String,
) -> Result<(), String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .filter(|&n| n > 0 && n <= 128 * 1024 * 1024)
        .ok_or_else(|| "Kích thước ảnh vượt giới hạn".to_string())?;
    let trimmed = rgba_base64.trim();
    let max_b64_len = expected.div_ceil(3).saturating_mul(4);
    if trimmed.is_empty() || trimmed.len() > max_b64_len {
        return Err("Dữ liệu ảnh base64 không hợp lệ hoặc vượt giới hạn".into());
    }
    let bytes = B64_STANDARD
        .decode(trimmed)
        .map_err(|e| format!("Dữ liệu ảnh base64 không hợp lệ: {e}"))?;
    if bytes.len() != expected {
        return Err("Dữ liệu pixel RGBA không khớp kích thước ảnh".into());
    }
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
    let main_win = app.get_window("main");
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
const WEBAPP_POPUP_COOLDOWN_MS: u64 = 1500;

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn webapp_label(id: &str) -> String {
    let raw = id.trim();
    let hash = fnv1a_64(raw.as_bytes());
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let trimmed = clean.trim_matches('-');
    if trimmed.is_empty() {
        format!("{WEBAPP_LABEL_PREFIX}app-{hash:016x}")
    } else {
        format!("{WEBAPP_LABEL_PREFIX}{trimmed}-{hash:016x}")
    }
}

fn try_acquire_popup_slot(last_popup_ms: &AtomicU64, now_ms: u64, cooldown_ms: u64) -> bool {
    let clamped_now = now_ms.max(1);
    let mut prev = last_popup_ms.load(Ordering::Acquire);
    loop {
        if prev != 0 && clamped_now.saturating_sub(prev) < cooldown_ms {
            return false;
        }
        match last_popup_ms.compare_exchange_weak(
            prev,
            clamped_now,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => prev = actual,
        }
    }
}

fn validate_web_url(raw: &str) -> Result<tauri::Url, String> {
    webapp_accounts::validate_web_url(raw)
}

fn hide_all_webapp_children(app: &AppHandle) {
    if let Some(st) = app.try_state::<AppState>() {
        st.webapp_epoch.fetch_add(1, Ordering::SeqCst);
    }
    for (label, wv) in app.webviews() {
        if label.starts_with(WEBAPP_LABEL_PREFIX) {
            let _ = wv.hide();
        }
    }
}

fn validate_webapp_bounds(x: f64, y: f64, width: f64, height: f64) -> Result<(f64, f64), String> {
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return Err("Tọa độ hoặc kích thước khung Web App không hợp lệ".into());
    }
    Ok((width.max(120.0), height.max(120.0)))
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
    let (safe_w, safe_h) = validate_webapp_bounds(x, y, width, height)?;
    let label = webapp_label(&id);
    let parsed_url_str = parsed.as_str().to_string();

    let my_epoch = state.webapp_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let _op = state.webapp_op_lock.lock().await;

    if state.webapp_epoch.load(Ordering::SeqCst) != my_epoch {
        return Ok(());
    }

    for (existing_label, wv) in app.webviews() {
        if existing_label.starts_with(WEBAPP_LABEL_PREFIX) && existing_label != label {
            let _ = wv.hide();
        }
    }

    if let Some(wv) = app.get_webview(&label) {
        let url_changed = {
            let mut urls = state.webapp_base_urls.lock().await;
            let changed = urls.get(&label).map(String::as_str) != Some(parsed_url_str.as_str());
            if changed {
                urls.insert(label.clone(), parsed_url_str);
            }
            changed
        };
        if url_changed {
            wv.navigate(parsed).map_err(|e| e.to_string())?;
        }
        wv.set_position(LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        wv.set_size(LogicalSize::new(safe_w, safe_h))
            .map_err(|e| e.to_string())?;
        if state.webapp_epoch.load(Ordering::SeqCst) != my_epoch {
            let _ = wv.hide();
            return Ok(());
        }
        wv.show().map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Use `get_window("main")` instead of `get_webview_window("main")`:
    // in Tauri v2, `get_webview_window` returns `None` as soon as a window owns
    // any child webview (`is_webview_window()` requires all webviews to share the window label).
    let parent_window = app
        .get_window("main")
        .ok_or_else(|| "Không tìm thấy cửa sổ chính".to_string())?;
    let app_for_new_win = app.clone();
    let label_for_new_win = label.clone();
    let popup_ts_for_new_win = state.webapp_last_popup_ms.clone();
    let app_for_load = app.clone();
    let label_for_load = label.clone();
    let app_id_for_load = id.trim().to_string();
    let accounts_for_load = state.webapp_accounts.clone();
    let builder = WebviewBuilder::new(&label, WebviewUrl::External(parsed))
        .on_navigation(|target_url| {
            matches!(target_url.scheme(), "http" | "https")
                && !webapp_accounts::is_forbidden_internal_webview_url(target_url)
        })
        .on_new_window(move |target_url, features| {
            if !matches!(target_url.scheme(), "http" | "https")
                || webapp_accounts::is_forbidden_internal_webview_url(&target_url)
            {
                return tauri::webview::NewWindowResponse::Deny;
            }
            // Keep sized popups (e.g., OAuth dialogs that rely on window.opener) working normally,
            // while rate-limiting native popups so malicious pages cannot popup-bomb.
            if features.size().is_some() {
                let now = chrono_now_ms().max(1) as u64;
                if try_acquire_popup_slot(&popup_ts_for_new_win, now, WEBAPP_POPUP_COOLDOWN_MS) {
                    return tauri::webview::NewWindowResponse::Allow;
                }
                return tauri::webview::NewWindowResponse::Deny;
            }
            // Navigate standard `target="_blank"` / unsized `window.open` links inside this Web App.
            let nav_app = app_for_new_win.clone();
            let nav_label = label_for_new_win.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(wv) = nav_app.get_webview(&nav_label) {
                    let _ = wv.navigate(target_url);
                }
            });
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load(move |_wv, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            if webapp_accounts::is_forbidden_internal_webview_url(payload.url()) {
                return;
            }
            let Ok(loaded_origin) = webapp_accounts::canonical_origin_from_url(payload.url())
            else {
                return;
            };
            let nav_app = app_for_load.clone();
            let nav_label = label_for_load.clone();
            let nav_app_id = app_id_for_load.clone();
            let accounts = accounts_for_load.clone();
            tauri::async_runtime::spawn(async move {
                let creds = {
                    let store = accounts.lock().await;
                    match store.resolve_autofill_for_page(&nav_app_id, &loaded_origin) {
                        Ok(Some(c)) => c,
                        _ => return,
                    }
                };
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let Some(wv) = nav_app.get_webview(&nav_label) else {
                    return;
                };
                let Ok(current_url) = wv.url() else {
                    return;
                };
                if webapp_accounts::verify_origin_matches(&creds.origin, &current_url).is_err() {
                    return;
                }
                if let Ok(script) = webapp_accounts::build_page_load_autofill_script(
                    &creds.username,
                    &creds.password,
                    &creds.origin,
                ) {
                    let _ = wv.eval(&script);
                }
            });
        });
    let child_wv = parent_window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(safe_w, safe_h),
        )
        .map_err(|e| format!("Không thể nhúng Web App: {e}"))?;

    state
        .webapp_base_urls
        .lock()
        .await
        .insert(label, parsed_url_str);

    #[cfg(windows)]
    webapp_accounts::enable_webview2_password_autosave(&child_wv);

    if state.webapp_epoch.load(Ordering::SeqCst) != my_epoch {
        let _ = child_wv.hide();
    }
    Ok(())
}

#[tauri::command]
async fn webapp_accounts_list(
    state: State<'_, AppState>,
    app_id: Option<String>,
    origin: Option<String>,
) -> Result<Vec<webapp_accounts::WebAppAccountDto>, String> {
    let store = state.webapp_accounts.lock().await;
    Ok(store.list(app_id.as_deref(), origin.as_deref()))
}

#[tauri::command]
async fn webapp_accounts_save(
    state: State<'_, AppState>,
    input: webapp_accounts::WebAppAccountInput,
) -> Result<webapp_accounts::WebAppAccountDto, String> {
    let mut store = state.webapp_accounts.lock().await;
    store.save(input)
}

#[tauri::command]
async fn webapp_accounts_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut store = state.webapp_accounts.lock().await;
    store.delete(&id)
}

#[tauri::command]
async fn webapp_account_fill(
    app: AppHandle,
    state: State<'_, AppState>,
    app_id: String,
    account_id: String,
    submit: Option<bool>,
) -> Result<(), String> {
    let creds = {
        let store = state.webapp_accounts.lock().await;
        store.resolve_credentials(&account_id)?
    };
    if creds.app_id != app_id.trim() {
        return Err("Tài khoản đã lưu không thuộc Web App hiện tại".into());
    }
    let label = webapp_label(&app_id);
    let wv = app
        .get_webview(&label)
        .ok_or_else(|| "Web App chưa được mở để điền tài khoản".to_string())?;
    let current_url = wv
        .url()
        .map_err(|e| format!("Không đọc được URL hiện tại của Web App: {e}"))?;
    webapp_accounts::verify_origin_matches(&creds.origin, &current_url)?;
    let script = webapp_accounts::build_autofill_script(
        &creds.username,
        &creds.password,
        &creds.origin,
        submit.unwrap_or(false),
    )?;
    wv.eval(&script)
        .map_err(|e| format!("Không thể điền thông tin đăng nhập: {e}"))?;
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
    let (safe_w, safe_h) = validate_webapp_bounds(x, y, width, height)?;
    let label = webapp_label(&id);
    if let Some(wv) = app.get_webview(&label) {
        wv.set_position(LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        wv.set_size(LogicalSize::new(safe_w, safe_h))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn webapp_hide_all(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.webapp_epoch.fetch_add(1, Ordering::SeqCst);
    let _op = state.webapp_op_lock.lock().await;
    for (label, wv) in app.webviews() {
        if label.starts_with(WEBAPP_LABEL_PREFIX) {
            let _ = wv.hide();
        }
    }
    Ok(())
}

#[tauri::command]
async fn webapp_close(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let _op = state.webapp_op_lock.lock().await;
    let label = webapp_label(&id);
    state.webapp_base_urls.lock().await.remove(&label);
    if let Some(wv) = app.get_webview(&label) {
        state.webapp_epoch.fetch_add(1, Ordering::SeqCst);
        wv.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn webapp_navigate(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    action: String,
    url: Option<String>,
) -> Result<(), String> {
    let _op = state.webapp_op_lock.lock().await;
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
    tokio::task::spawn_blocking(move || file_dialog::open_url_with_shell(&target))
        .await
        .map_err(|e| format!("Không mở được trình duyệt: {e}"))?
}

const MEDIA_PROTOCOL_RANGE_WINDOW_BYTES: u64 = 2 * 1024 * 1024;
const MEDIA_PROTOCOL_MAX_EXPLICIT_RANGE_BYTES: u64 = 8 * 1024 * 1024;
const MEDIA_PROTOCOL_DIRECT_FULL_MAX_BYTES: u64 = 8 * 1024 * 1024;
const MEDIA_PROTOCOL_IMAGE_FULL_MAX_BYTES: u64 = 64 * 1024 * 1024;
const VAULT_PROBE_WINDOW_BYTES: u64 = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaVaultConfigDto {
    pub custom_vault_dir: Option<String>,
    pub active_vault_dir: String,
    pub quota_mb: u64,
    pub encrypt_at_rest: bool,
    pub auto_cache_images: bool,
    pub auto_cache_voice: bool,
    pub auto_cache_video_files: bool,
}

fn media_vault_config_to_dto(cfg: &MediaVaultConfig, active_dir: &Path) -> MediaVaultConfigDto {
    let display = file_dialog::normalize_os_display_path(active_dir);
    MediaVaultConfigDto {
        custom_vault_dir: cfg.custom_vault_dir.clone(),
        active_vault_dir: display.to_string_lossy().into_owned(),
        quota_mb: cfg.quota_mb,
        encrypt_at_rest: cfg.encrypt_at_rest,
        auto_cache_images: cfg.auto_cache_images,
        auto_cache_voice: cfg.auto_cache_voice,
        auto_cache_video_files: cfg.auto_cache_video_files,
    }
}

fn validate_custom_vault_dir(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(trimmed) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
        return Err("Đường dẫn thư mục lưu trữ chứa ký tự không hợp lệ".into());
    }
    let path = PathBuf::from(&trimmed);
    if !path.is_absolute() {
        return Err(
            "Đường dẫn thư mục lưu trữ phải là đường dẫn tuyệt đối (ví dụ: E:\\TezaMedia)".into(),
        );
    }
    if path.exists() {
        if !path.is_dir() {
            return Err("Đường dẫn đã chọn là tệp tin, không phải thư mục".into());
        }
    } else {
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("Không tạo được thư mục lưu trữ: {e}"))?;
    }
    let display = file_dialog::normalize_os_display_path(&path);
    Ok(Some(display.to_string_lossy().into_owned()))
}

fn open_media_vault_with_fallback(
    paths: &StoragePaths,
    uid: Option<&str>,
    cfg: MediaVaultConfig,
) -> MediaVault {
    match MediaVault::open(paths, uid, cfg.clone()) {
        Ok(vault) => vault,
        Err(first_err) => {
            if cfg.custom_vault_dir.is_some() {
                eprintln!(
                    "[teza][media_vault] Failed to open custom vault dir {:?} ({first_err}); falling back to default media directory",
                    cfg.custom_vault_dir
                );
                let mut fallback_cfg = cfg.clone();
                fallback_cfg.custom_vault_dir = None;
                if let Ok(vault) = MediaVault::open(paths, uid, fallback_cfg) {
                    return vault;
                }
            }
            let ts = chrono_now_ms().max(1);
            eprintln!(
                "[teza][media_vault] Default media vault open failed ({first_err}); falling back to temporary media directory"
            );
            let tmp_root = std::env::temp_dir().join(format!("teza_recovery_media_{ts}"));
            let fallback_paths =
                StoragePaths::from_dir(&tmp_root).unwrap_or_else(|_| paths.clone());
            let mut safe_cfg = cfg;
            safe_cfg.custom_vault_dir = None;
            MediaVault::open(&fallback_paths, uid, safe_cfg)
                .expect("Failed to open fallback media vault")
        }
    }
}

async fn rebind_media_vault_unlocked(
    media_vault_lock: &Arc<RwLock<Arc<MediaVault>>>,
    paths: &StoragePaths,
    uid: Option<&str>,
) {
    let cfg = MediaVaultConfig::load_from_file(&paths.media_config_file());
    let next = Arc::new(open_media_vault_with_fallback(paths, uid, cfg));
    let mut guard = media_vault_lock.write().await;
    *guard = next;
}

fn classify_ws_auto_cache_kind(msg_type: &str) -> Option<&'static str> {
    let lower = msg_type.trim().to_ascii_lowercase();
    match lower.as_str() {
        "chat.photo" | "photo" | "2" | "chat.gif" | "gif" | "chat.sticker" | "sticker" | "5" => {
            Some("image")
        }
        "chat.voice" | "voice" | "3" => Some("audio"),
        "chat.video" | "video" | "chat.video.msg" | "4" => Some("video"),
        "chat.file" | "file" | "19" | "share.file" => Some("file"),
        _ => None,
    }
}

fn should_auto_cache_kind(cfg: &MediaVaultConfig, kind: &str) -> bool {
    match kind {
        "image" => cfg.auto_cache_images,
        "audio" => cfg.auto_cache_voice,
        "video" | "file" => cfg.auto_cache_video_files,
        _ => false,
    }
}

fn infer_media_kind_from_ext(ext: &str) -> &'static str {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" | "heic"
        | "heif" | "avif" => "image",
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv" | "3gp" => "video",
        "mp3" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "flac" | "wma" | "amr" => "audio",
        _ => "file",
    }
}

fn infer_media_kind_for_download(suggested_name: &str, url: &str) -> &'static str {
    if let Some(ext) = file_dialog::extract_extension_from_name(suggested_name)
        .or_else(|| file_dialog::extract_extension_from_url(url))
    {
        return infer_media_kind_from_ext(&ext);
    }
    "file"
}

fn infer_media_kind_for_attachment(file_name: &str, msg_type: &str) -> &'static str {
    if let Some(kind) = classify_ws_auto_cache_kind(msg_type) {
        if kind != "file" {
            return kind;
        }
    }
    if let Some(ext) = file_dialog::extract_extension_from_name(file_name) {
        return infer_media_kind_from_ext(&ext);
    }
    "file"
}

fn infer_mime_from_filename_and_bytes(file_name: &str, bytes_opt: Option<&[u8]>) -> &'static str {
    let ext = bytes_opt
        .and_then(file_dialog::sniff_extension_from_bytes)
        .map(str::to_string)
        .or_else(|| file_dialog::extract_extension_from_name(file_name));
    match ext.as_deref().unwrap_or("") {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "amr" => "audio/amr",
        "pdf" => "application/pdf",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn read_vault_probe_bytes(vault: &MediaVault, obj: &MediaObjectRecord) -> Vec<u8> {
    let total = obj.size_bytes;
    if total == 0 {
        return Vec::new();
    }
    if total <= VAULT_PROBE_WINDOW_BYTES * 2 {
        return vault.read_range(obj, 0, total - 1).await.unwrap_or_default();
    }
    let mut probe = vault
        .read_range(obj, 0, VAULT_PROBE_WINDOW_BYTES - 1)
        .await
        .unwrap_or_default();
    if let Ok(tail) = vault
        .read_range(obj, total - VAULT_PROBE_WINDOW_BYTES, total - 1)
        .await
    {
        probe.extend_from_slice(&tail);
    }
    probe
}

fn decode_query_component(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
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

fn validate_teza_media_remote_url(raw: &str) -> Result<String, String> {
    if raw.chars().any(char::is_control) {
        return Err("URL media chứa ký tự điều khiển".into());
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 8192 {
        return Err("URL media không hợp lệ".into());
    }
    let parsed = tauri::Url::parse(trimmed).map_err(|_| "URL media không hợp lệ".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Chỉ cho phép URL http/https cho teza-media".into());
    }
    let Some(host) = parsed.host_str().filter(|h| !h.is_empty()) else {
        return Err("URL media thiếu tên miền".into());
    };
    let lower_host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if lower_host == "teza-media.localhost"
        || lower_host.ends_with(".teza-media.localhost")
        || webapp_accounts::is_forbidden_internal_webview_url(&parsed)
    {
        return Err("Không cho phép địa chỉ nội bộ trong teza-media".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL media không được chứa thông tin đăng nhập".into());
    }
    Ok(parsed.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TezaMediaQueryParams {
    url: String,
    kind: String,
    token: String,
    tid: Option<String>,
    mid: Option<String>,
}

fn parse_teza_media_query(
    uri: &tauri::http::Uri,
) -> Result<TezaMediaQueryParams, (u16, &'static str)> {
    if uri.path() != "/resolve" {
        return Err((404, "Not Found"));
    }
    let mut raw_url: Option<String> = None;
    let mut raw_kind: Option<String> = None;
    let mut raw_token: Option<String> = None;
    let mut raw_tid: Option<String> = None;
    let mut raw_mid: Option<String> = None;

    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k_raw, v_raw) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode_query_component(k_raw);
            let val = decode_query_component(v_raw);
            match key.as_str() {
                "url" => raw_url = Some(val),
                "kind" => raw_kind = Some(val),
                "t" => raw_token = Some(val),
                "tid" => raw_tid = Some(val),
                "mid" => raw_mid = Some(val),
                _ => {}
            }
        }
    }

    let token = raw_token.unwrap_or_default();
    let kind = match raw_kind
        .as_deref()
        .map(str::trim)
        .unwrap_or("image")
        .to_ascii_lowercase()
        .as_str()
    {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "file" => "file",
        "thumb" => "thumb",
        _ => "image",
    }
    .to_string();

    let tid = raw_tid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let mid = raw_mid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    Ok(TezaMediaQueryParams {
        url: raw_url.unwrap_or_default(),
        kind,
        token,
        tid,
        mid,
    })
}

fn unwrap_teza_media_url(raw: &str) -> Result<(String, Option<String>, Option<String>), String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let is_vault_url = lower.starts_with("teza-media://")
        || lower.starts_with("http://teza-media.localhost/")
        || lower.starts_with("https://teza-media.localhost/");
    if !is_vault_url {
        return Ok((trimmed.to_string(), None, None));
    }
    let uri: tauri::http::Uri = trimmed
        .parse()
        .map_err(|_| "URL teza-media không hợp lệ".to_string())?;
    let parsed = parse_teza_media_query(&uri)
        .map_err(|(_, msg)| format!("URL teza-media không hợp lệ: {msg}"))?;
    let remote_url = validate_teza_media_remote_url(&parsed.url)?;
    Ok((remote_url, parsed.tid, parsed.mid))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedByteRange {
    Valid { start: u64, end_inclusive: u64 },
    Unsatisfiable,
    Ignore,
}

fn parse_http_byte_range(header: &str, total: u64) -> ParsedByteRange {
    let trimmed = header.trim();
    let Some(spec) = trimmed
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bytes="))
        .map(|_| trimmed[6..].trim())
    else {
        return ParsedByteRange::Ignore;
    };
    if spec.is_empty() || spec.contains(',') {
        return ParsedByteRange::Ignore;
    }
    let Some((start_raw, end_raw)) = spec.split_once('-') else {
        return ParsedByteRange::Ignore;
    };
    let start_str = start_raw.trim();
    let end_str = end_raw.trim();

    if total == 0 {
        if start_str.is_empty() && end_str.is_empty() {
            return ParsedByteRange::Ignore;
        }
        return ParsedByteRange::Unsatisfiable;
    }

    if start_str.is_empty() {
        let Ok(suffix_len) = end_str.parse::<u64>() else {
            return ParsedByteRange::Ignore;
        };
        if suffix_len == 0 {
            return ParsedByteRange::Unsatisfiable;
        }
        let len = suffix_len
            .min(total)
            .min(MEDIA_PROTOCOL_MAX_EXPLICIT_RANGE_BYTES);
        let start = total - len;
        return ParsedByteRange::Valid {
            start,
            end_inclusive: total - 1,
        };
    }

    let Ok(start) = start_str.parse::<u64>() else {
        return ParsedByteRange::Ignore;
    };
    if end_str.is_empty() {
        if start >= total {
            return ParsedByteRange::Unsatisfiable;
        }
        let end_inclusive = start
            .saturating_add(MEDIA_PROTOCOL_RANGE_WINDOW_BYTES - 1)
            .min(total - 1);
        return ParsedByteRange::Valid {
            start,
            end_inclusive,
        };
    }

    let Ok(end) = end_str.parse::<u64>() else {
        return ParsedByteRange::Ignore;
    };
    if start > end {
        return ParsedByteRange::Ignore;
    }
    if start >= total {
        return ParsedByteRange::Unsatisfiable;
    }
    let end_inclusive = end
        .min(total - 1)
        .min(start.saturating_add(MEDIA_PROTOCOL_MAX_EXPLICIT_RANGE_BYTES - 1));
    ParsedByteRange::Valid {
        start,
        end_inclusive,
    }
}

fn allowed_media_cors_origin(headers: &tauri::http::HeaderMap) -> Option<String> {
    let raw = headers.get("origin")?.to_str().ok()?.trim();
    match raw {
        "http://tauri.localhost"
        | "https://tauri.localhost"
        | "tauri://localhost"
        | "http://localhost:1420"
        | "http://127.0.0.1:1420" => Some(raw.to_string()),
        _ => None,
    }
}

fn sanitize_media_content_type(mime: &str) -> String {
    let trimmed = mime.trim();
    if trimmed.is_empty()
        || trimmed.len() > 200
        || trimmed.chars().any(char::is_control)
        || tauri::http::HeaderValue::from_str(trimmed).is_err()
    {
        return "application/octet-stream".to_string();
    }
    trimmed.to_string()
}

fn map_media_protocol_error(err: &anyhow::Error) -> u16 {
    if let Some(dl_err) = err.downcast_ref::<MediaDownloadError>() {
        return match dl_err {
            MediaDownloadError::ForbiddenSvg => 415,
            MediaDownloadError::UntrustedUrl { .. } => 403,
            MediaDownloadError::CdnExpired { .. } => 404,
            MediaDownloadError::HttpStatus { status } if *status == 404 || *status == 410 => 404,
            MediaDownloadError::HttpStatus { .. } => 502,
        };
    }
    502
}

fn build_media_error_response(
    status: u16,
    message: &str,
    cors_origin: Option<&str>,
    content_range: Option<&str>,
) -> tauri::http::Response<Vec<u8>> {
    let mut builder = tauri::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .header("cache-control", "no-store");
    if let Some(cr) = content_range {
        builder = builder.header("content-range", cr);
    }
    if let Some(origin) = cors_origin {
        builder = builder
            .header("access-control-allow-origin", origin)
            .header("vary", "Origin");
    }
    builder.body(message.as_bytes().to_vec()).unwrap_or_else(|_| {
        tauri::http::Response::builder()
            .status(500)
            .body(Vec::new())
            .expect("static fallback HTTP response")
    })
}

fn should_serve_full_without_range(is_head: bool, kind: &str, total: u64) -> bool {
    is_head
        || total <= MEDIA_PROTOCOL_DIRECT_FULL_MAX_BYTES
        || (kind == "image" && total <= MEDIA_PROTOCOL_IMAGE_FULL_MAX_BYTES)
}

#[allow(clippy::too_many_arguments)]
fn build_media_success_response(
    status: u16,
    content_type: &str,
    content_length: u64,
    content_range: Option<String>,
    content_disposition: Option<&str>,
    cors_origin: Option<&str>,
    body: Vec<u8>,
) -> tauri::http::Response<Vec<u8>> {
    let safe_ct = sanitize_media_content_type(content_type);
    let mut builder = tauri::http::Response::builder()
        .status(status)
        .header("content-type", safe_ct)
        .header("accept-ranges", "bytes")
        .header("content-length", content_length.to_string())
        .header("x-content-type-options", "nosniff")
        .header("cache-control", "private, max-age=86400");
    if let Some(cr) = content_range {
        builder = builder.header("content-range", cr);
    }
    if let Some(cd) = content_disposition {
        let trimmed = cd.trim();
        if !trimmed.is_empty()
            && !trimmed.chars().any(char::is_control)
            && tauri::http::HeaderValue::from_str(trimmed).is_ok()
        {
            builder = builder.header("content-disposition", trimmed);
        }
    }
    if let Some(origin) = cors_origin {
        builder = builder
            .header("access-control-allow-origin", origin)
            .header("vary", "Origin");
    }
    builder.body(body).unwrap_or_else(|_| {
        tauri::http::Response::builder()
            .status(500)
            .body(Vec::new())
            .expect("static fallback HTTP response")
    })
}

fn handle_teza_media_protocol<R: tauri::Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let cors_origin = allowed_media_cors_origin(request.headers());

    if request.method() == tauri::http::Method::OPTIONS {
        if let Some(ref origin) = cors_origin {
            let resp = tauri::http::Response::builder()
                .status(204)
                .header("access-control-allow-origin", origin.as_str())
                .header("access-control-allow-methods", "GET, HEAD")
                .header("access-control-allow-headers", "Range")
                .header("vary", "Origin")
                .header("x-content-type-options", "nosniff")
                .body(Vec::new())
                .unwrap_or_else(|_| {
                    tauri::http::Response::builder()
                        .status(204)
                        .body(Vec::new())
                        .expect("static 204 response")
                });
            responder.respond(resp);
        } else {
            responder.respond(build_media_error_response(403, "Forbidden", None, None));
        }
        return;
    }

    let is_head = request.method() == tauri::http::Method::HEAD;
    if request.method() != tauri::http::Method::GET && !is_head {
        responder.respond(build_media_error_response(
            405,
            "Method Not Allowed",
            cors_origin.as_deref(),
            None,
        ));
        return;
    }

    let params = match parse_teza_media_query(request.uri()) {
        Ok(p) => p,
        Err((status, msg)) => {
            responder.respond(build_media_error_response(
                status,
                msg,
                cors_origin.as_deref(),
                None,
            ));
            return;
        }
    };

    let state = ctx.app_handle().state::<AppState>();
    if params.token.is_empty()
        || !constant_time_eq(
            params.token.as_bytes(),
            state.media_protocol_token.as_bytes(),
        )
    {
        responder.respond(build_media_error_response(
            403,
            "Forbidden",
            cors_origin.as_deref(),
            None,
        ));
        return;
    }

    let remote_url = match validate_teza_media_remote_url(&params.url) {
        Ok(u) => u,
        Err(_) => {
            responder.respond(build_media_error_response(
                400,
                "Invalid media URL",
                cors_origin.as_deref(),
                None,
            ));
            return;
        }
    };

    let range_hdr = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let sess_arc = state.session.clone();
    let store_arc = state.store.clone();
    let media_vault_arc = state.media_vault.clone();
    let gdrive_arc = state.gdrive.clone();
    let relay_cfg_arc = state.relay_cfg.clone();
    let app_handle = ctx.app_handle().clone();
    let kind = params.kind;
    let tid = params.tid;
    let mid = params.mid;

    tauri::async_runtime::spawn(async move {
        let vault = media_vault_arc.read().await.clone();
        let sess_snap = {
            let sess = sess_arc.lock().await;
            sess.clone_shallow()
        };
        let msg_ref = match (tid.as_deref(), mid.as_deref()) {
            (Some(t), Some(m)) => Some((t, m)),
            _ => None,
        };

        let obj = match resolve_media_with_tier2_gdrive(
            Some(&app_handle),
            &vault,
            &sess_snap,
            &store_arc,
            &gdrive_arc,
            &relay_cfg_arc,
            &remote_url,
            &kind,
            msg_ref,
            |_, _| {},
        )
        .await
        {
            Ok(obj) => obj,
            Err(err) => {
                let status = map_media_protocol_error(&err);
                let label = match status {
                    415 => "Unsupported Media Type",
                    403 => "Forbidden",
                    404 => "Media Not Found",
                    _ => "Upstream Media Error",
                };
                responder.respond(build_media_error_response(
                    status,
                    label,
                    cors_origin.as_deref(),
                    None,
                ));
                return;
            }
        };

        let effective_mime: String = if zalo_core::media_vault::is_generic_mime(&obj.mime) {
            vault
                .sniff_cached_mime(&obj)
                .await
                .map(str::to_string)
                .unwrap_or_else(|| obj.mime.clone())
        } else {
            obj.mime.clone()
        };

        if is_forbidden_svg_mime(&obj.mime) || is_forbidden_svg_mime(&effective_mime) {
            responder.respond(build_media_error_response(
                415,
                "SVG media is not supported",
                cors_origin.as_deref(),
                None,
            ));
            return;
        }

        let orig_name = {
            let store = store_arc.lock().await;
            let from_msg = msg_ref.and_then(|(t, m)| {
                store
                    .get_message_media_record(t, m, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
            });
            from_msg.or_else(|| {
                let url_hash = MediaVault::sha256_url(&remote_url);
                store
                    .get_message_media_record("__url__", &url_hash, "primary")
                    .ok()
                    .flatten()
                    .and_then(|rec| rec.original_name)
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        store
                            .find_media_original_name_by_hash(&url_hash)
                            .ok()
                            .flatten()
                    })
            })
        };
        let head_probe = if obj.size_bytes > 0
            && file_dialog::extension_from_mime(&effective_mime).is_none()
        {
            read_vault_probe_bytes(&vault, &obj).await
        } else {
            Vec::new()
        };
        let (resolved_name, _resolved_ext) = file_dialog::resolve_download_filename_and_ext(
            orig_name.as_deref().unwrap_or(&kind),
            orig_name.as_deref(),
            &remote_url,
            &remote_url,
            &effective_mime,
            &head_probe,
        );
        let content_disposition = file_dialog::build_inline_content_disposition(&resolved_name);

        let total = obj.size_bytes;
        if total == 0 {
            if let Some(ref rh) = range_hdr {
                if matches!(
                    parse_http_byte_range(rh, 0),
                    ParsedByteRange::Unsatisfiable
                ) {
                    responder.respond(build_media_error_response(
                        416,
                        "Range Not Satisfiable",
                        cors_origin.as_deref(),
                        Some("bytes */0"),
                    ));
                    return;
                }
            }
            responder.respond(build_media_success_response(
                200,
                &effective_mime,
                0,
                None,
                Some(content_disposition.as_str()),
                cors_origin.as_deref(),
                Vec::new(),
            ));
            return;
        }

        if let Some(ref rh) = range_hdr {
            match parse_http_byte_range(rh, total) {
                ParsedByteRange::Valid {
                    start,
                    end_inclusive,
                } => {
                    let body = if is_head {
                        Vec::new()
                    } else {
                        match vault.read_range(&obj, start, end_inclusive).await {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                responder.respond(build_media_error_response(
                                    502,
                                    "Failed to read cached media range",
                                    cors_origin.as_deref(),
                                    None,
                                ));
                                return;
                            }
                        }
                    };
                    let slice_len = if is_head {
                        end_inclusive - start + 1
                    } else {
                        body.len() as u64
                    };
                    let actual_end = if is_head || slice_len == 0 {
                        end_inclusive
                    } else {
                        start + slice_len - 1
                    };
                    responder.respond(build_media_success_response(
                        206,
                        &effective_mime,
                        slice_len,
                        Some(format!("bytes {start}-{actual_end}/{total}")),
                        Some(content_disposition.as_str()),
                        cors_origin.as_deref(),
                        body,
                    ));
                    return;
                }
                ParsedByteRange::Unsatisfiable => {
                    let cr = format!("bytes */{total}");
                    responder.respond(build_media_error_response(
                        416,
                        "Range Not Satisfiable",
                        cors_origin.as_deref(),
                        Some(&cr),
                    ));
                    return;
                }
                ParsedByteRange::Ignore => {}
            }
        }

        if should_serve_full_without_range(is_head, &kind, total) {
            let body = if is_head {
                Vec::new()
            } else {
                match vault.read_range(&obj, 0, total - 1).await {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        responder.respond(build_media_error_response(
                            502,
                            "Failed to read cached media",
                            cors_origin.as_deref(),
                            None,
                        ));
                        return;
                    }
                }
            };
            let len = if is_head { total } else { body.len() as u64 };
            responder.respond(build_media_success_response(
                200,
                &effective_mime,
                len,
                None,
                Some(content_disposition.as_str()),
                cors_origin.as_deref(),
                body,
            ));
        } else {
            let end_inclusive = (MEDIA_PROTOCOL_RANGE_WINDOW_BYTES - 1).min(total - 1);
            let body = match vault.read_range(&obj, 0, end_inclusive).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    responder.respond(build_media_error_response(
                        502,
                        "Failed to read cached media window",
                        cors_origin.as_deref(),
                        None,
                    ));
                    return;
                }
            };
            let slice_len = body.len() as u64;
            let actual_end = if slice_len == 0 {
                end_inclusive
            } else {
                slice_len - 1
            };
            responder.respond(build_media_success_response(
                206,
                &effective_mime,
                slice_len,
                Some(format!("bytes 0-{actual_end}/{total}")),
                Some(content_disposition.as_str()),
                cors_origin.as_deref(),
                body,
            ));
        }
    });
}

#[tauri::command]
async fn media_get_protocol_token(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.media_protocol_token.clone())
}

#[tauri::command]
async fn media_get_config(state: State<'_, AppState>) -> Result<MediaVaultConfigDto, String> {
    let vault = state.media_vault.read().await.clone();
    let cfg = vault.config();
    Ok(media_vault_config_to_dto(&cfg, vault.vault_dir()))
}

#[tauri::command]
async fn media_save_config(
    state: State<'_, AppState>,
    config: MediaVaultConfigDto,
) -> Result<MediaVaultConfigDto, String> {
    let custom_dir = validate_custom_vault_dir(config.custom_vault_dir)?;
    let next_cfg = MediaVaultConfig {
        custom_vault_dir: custom_dir,
        quota_mb: config.quota_mb,
        encrypt_at_rest: config.encrypt_at_rest,
        auto_cache_images: config.auto_cache_images,
        auto_cache_voice: config.auto_cache_voice,
        auto_cache_video_files: config.auto_cache_video_files,
    };

    let uid_opt = {
        let sess = state.session.lock().await;
        let trimmed = sess.profile.uid.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    let old_vault = state.media_vault.read().await.clone();
    let old_cfg = old_vault.config();
    let dir_changed = old_cfg.custom_vault_dir != next_cfg.custom_vault_dir;
    old_vault.flush_access_touches(&state.store).await;

    let active_vault = if dir_changed {
        let rebound = MediaVault::open(&state.paths, uid_opt.as_deref(), next_cfg.clone())
            .map_err(|e| format!("Không khởi tạo được thư mục kho media: {e}"))?;
        next_cfg
            .save_to_file(&state.paths.media_config_file())
            .map_err(|e| format!("Không lưu được cấu hình kho media: {e}"))?;
        let arc = Arc::new(rebound);
        let mut guard = state.media_vault.write().await;
        *guard = arc.clone();
        arc
    } else {
        next_cfg
            .save_to_file(&state.paths.media_config_file())
            .map_err(|e| format!("Không lưu được cấu hình kho media: {e}"))?;
        old_vault.update_config(next_cfg.clone());
        old_vault
    };

    let quota_lowered =
        next_cfg.quota_mb > 0 && (old_cfg.quota_mb == 0 || next_cfg.quota_mb < old_cfg.quota_mb);
    if quota_lowered {
        let _ = active_vault.run_lru_eviction(&state.store).await;
    }

    Ok(media_vault_config_to_dto(
        &next_cfg,
        active_vault.vault_dir(),
    ))
}

#[tauri::command]
async fn media_get_stats(state: State<'_, AppState>) -> Result<MediaVaultStatsDto, String> {
    let vault = state.media_vault.read().await.clone();
    vault.flush_access_touches(&state.store).await;
    let store = state.store.lock().await;
    store.compute_media_stats().map_err(|e| e.to_string())
}

#[tauri::command]
async fn media_clear_cache(
    state: State<'_, AppState>,
    include_pinned: Option<bool>,
) -> Result<MediaVaultStatsDto, String> {
    let vault = state.media_vault.read().await.clone();
    vault
        .clear_cache(&state.store, include_pinned.unwrap_or(false))
        .await
        .map_err(|e| format!("Không thể dọn bộ nhớ đệm media: {e}"))
}

#[tauri::command]
async fn media_pick_vault_dir(app: AppHandle) -> Result<Option<String>, String> {
    let owner_hwnd = main_window_hwnd(&app);
    let picked = tokio::task::spawn_blocking(move || file_dialog::pick_directory(owner_hwnd))
        .await
        .map_err(|e| format!("Lỗi tác vụ chọn thư mục: {e}"))??;
    Ok(picked.map(|p| {
        file_dialog::normalize_os_display_path(&p)
            .to_string_lossy()
            .into_owned()
    }))
}

#[tauri::command]
async fn media_set_pin(
    state: State<'_, AppState>,
    sha256: String,
    pinned: bool,
) -> Result<(), String> {
    let clean = sha256.trim();
    if clean.is_empty() {
        return Err("Mã SHA-256 media không hợp lệ".into());
    }
    let store = state.store.lock().await;
    store
        .set_media_pin(clean, pinned)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn media_open_vault_dir(state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.media_vault.read().await.vault_dir().to_path_buf();
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Không tạo được thư mục kho media: {e}"))?;
    }
    tokio::task::spawn_blocking(move || file_dialog::open_directory(&dir))
        .await
        .map_err(|e| format!("Không thể mở thư mục kho media: {e}"))?
}

const GDRIVE_MEDIA_SYNC_BATCH_LIMIT: usize = 50;
const GDRIVE_SYNC_WAKE_COALESCE_SECS: u64 = 2;

struct TempFileGuard(PathBuf);

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn effective_gdrive_uid(uid: Option<&str>) -> &str {
    uid.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
}

fn check_gdrive_relay_gate(relay_enabled: bool) -> Result<(), String> {
    ensure_relay_disabled(relay_enabled).map_err(|e| e.to_string())
}

fn validate_gdrive_passphrase(passphrase: &str) -> Result<(), String> {
    if passphrase.trim().chars().count() < MIN_VAULT_PASSPHRASE_LEN {
        return Err(format!(
            "Mật khẩu đồng bộ E2EE phải có ít nhất {MIN_VAULT_PASSPHRASE_LEN} ký tự"
        ));
    }
    Ok(())
}

fn compute_prune_cutoff_ts_ms(now_ms: i64, older_than_days: u32) -> i64 {
    let delta_ms = i64::from(older_than_days).saturating_mul(86_400_000);
    now_ms.saturating_sub(delta_ms)
}

fn should_trigger_gdrive_rehydration(
    relay_enabled: bool,
    gdrive_ready: bool,
    primary_err: &anyhow::Error,
    cloud_candidate: Option<&MediaObjectRecord>,
) -> bool {
    if relay_enabled || !gdrive_ready {
        return false;
    }
    let Some(obj) = cloud_candidate else {
        return false;
    };
    if !obj
        .cloud_file_id
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return false;
    }
    let is_cdn_expired_or_gone = primary_err
        .downcast_ref::<MediaDownloadError>()
        .is_some_and(|dl| {
            dl.is_cdn_expired()
                || matches!(dl, MediaDownloadError::HttpStatus { status } if *status == 403 || *status == 404 || *status == 410)
        });
    is_cdn_expired_or_gone || obj.local_state == MEDIA_LOCAL_STATE_MISSING
}

struct GDriveInner {
    uid: Option<String>,
    config: GDriveStoredConfig,
    tokens: Option<GDriveTokens>,
    master_key: Option<CloudMasterKey>,
    has_remote_manifest: bool,
    syncing: bool,
    phase: String,
    progress_done: u64,
    progress_total: u64,
    last_sync_ts: Option<i64>,
    last_history_sync_ts: Option<i64>,
    last_error: Option<String>,
}

fn build_gdrive_status_snapshot(
    relay_blocked: bool,
    inner: &GDriveInner,
    breakdown: &CloudMediaBreakdown,
) -> GDriveStatusDto {
    let connected = inner.tokens.is_some();
    let vault_unlocked = connected && inner.master_key.is_some();
    let has_remote_manifest = inner.has_remote_manifest
        || inner
            .tokens
            .as_ref()
            .is_some_and(|t| t.manifest_file_id.is_some());
    let (syncing, phase, progress_done, progress_total) = if relay_blocked {
        (false, GDRIVE_PHASE_PAUSED_RELAY.to_string(), 0, 0)
    } else {
        let p = if inner.phase == GDRIVE_PHASE_PAUSED_RELAY || inner.phase.is_empty() {
            GDRIVE_PHASE_IDLE.to_string()
        } else {
            inner.phase.clone()
        };
        (inner.syncing, p, inner.progress_done, inner.progress_total)
    };
    GDriveStatusDto {
        relay_blocked,
        oauth_configured: inner.config.is_oauth_configured(),
        connected,
        account_email: inner.tokens.as_ref().and_then(|t| t.account_email.clone()),
        account_name: inner.tokens.as_ref().and_then(|t| t.account_name.clone()),
        vault_unlocked,
        has_remote_manifest,
        sync_enabled: inner.config.sync_enabled,
        sync_media: inner.config.sync_media,
        sync_history: inner.config.sync_history,
        syncing,
        phase,
        progress_done,
        progress_total,
        pending_media_count: breakdown.pending_count,
        pending_media_bytes: breakdown.pending_bytes,
        last_sync_ts: inner.last_sync_ts,
        last_history_sync_ts: inner.last_history_sync_ts,
        last_error: inner.last_error.clone(),
    }
}

pub struct GDriveRuntime {
    paths: StoragePaths,
    client: GDriveClient,
    device_id: String,
    inner: Mutex<GDriveInner>,
    sync_lock: Mutex<()>,
    connect_lock: Mutex<()>,
    sync_notify: Arc<tokio::sync::Notify>,
    cancel_tx: tokio::sync::watch::Sender<u64>,
    oauth_cancel: Arc<tokio::sync::Notify>,
    bg_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    rehydrating: std::sync::Mutex<HashMap<String, tokio::sync::watch::Receiver<bool>>>,
}

impl GDriveRuntime {
    fn new(
        paths: StoragePaths,
        uid: Option<&str>,
        last_sync_ts: Option<i64>,
        last_history_sync_ts: Option<i64>,
    ) -> Self {
        let config = GDriveStoredConfig::load_from_file(&paths.gdrive_config_file());
        let device_id = load_or_create_device_id(&paths.gdrive_device_id_file())
            .unwrap_or_else(|_| format!("dev-fallback-{}", std::process::id()));
        let client = GDriveClient::new().expect("Failed to initialize direct Google Drive client");
        let clean_uid = uid
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let (tokens, master_key) = if let Some(ref u) = clean_uid {
            let t = GDriveTokens::load_from_file(&paths.gdrive_tokens_file(Some(u)))
                .ok()
                .flatten();
            let k = if t.is_some() {
                CloudMasterKey::load_from_file(&paths.gdrive_master_key_file(Some(u)))
                    .ok()
                    .flatten()
            } else {
                None
            };
            (t, k)
        } else {
            (None, None)
        };
        let has_remote_manifest = tokens
            .as_ref()
            .is_some_and(|t| t.manifest_file_id.is_some())
            || master_key.is_some();
        let (cancel_tx, _) = tokio::sync::watch::channel(0u64);
        Self {
            paths,
            client,
            device_id,
            inner: Mutex::new(GDriveInner {
                uid: clean_uid,
                config,
                tokens,
                master_key,
                has_remote_manifest,
                syncing: false,
                phase: GDRIVE_PHASE_IDLE.to_string(),
                progress_done: 0,
                progress_total: 0,
                last_sync_ts,
                last_history_sync_ts,
                last_error: None,
            }),
            sync_lock: Mutex::new(()),
            connect_lock: Mutex::new(()),
            sync_notify: Arc::new(tokio::sync::Notify::new()),
            cancel_tx,
            oauth_cancel: Arc::new(tokio::sync::Notify::new()),
            bg_task: Mutex::new(None),
            rehydrating: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn notify_sync(&self) {
        self.sync_notify.notify_one();
    }

    fn cancel_active_operations(&self) {
        self.cancel_tx.send_modify(|v| *v = v.wrapping_add(1));
        self.oauth_cancel.notify_waiters();
    }

    fn cancel_oauth_flow(&self) {
        self.cancel_tx.send_modify(|v| *v = v.wrapping_add(1));
        self.oauth_cancel.notify_waiters();
    }

    async fn shutdown(&self) {
        self.cancel_active_operations();
        if let Some(handle) = self.bg_task.lock().await.take() {
            handle.abort();
        }
    }

    async fn auto_sync_interval_secs(&self) -> u64 {
        self.inner
            .lock()
            .await
            .config
            .auto_sync_interval_secs
            .clamp(30, 86_400)
    }

    async fn is_connected_and_unlocked(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.tokens.is_some() && guard.master_key.is_some()
    }

    async fn can_background_sync(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.tokens.is_some()
            && guard.master_key.is_some()
            && guard.config.sync_enabled
            && (guard.config.sync_media || guard.config.sync_history)
    }

    async fn snapshot_status(
        &self,
        relay_blocked: bool,
        store: &Arc<Mutex<MessageStore>>,
    ) -> GDriveStatusDto {
        let breakdown = {
            let guard = store.lock().await;
            guard.compute_cloud_media_breakdown().unwrap_or_default()
        };
        let inner = self.inner.lock().await;
        build_gdrive_status_snapshot(relay_blocked, &inner, &breakdown)
    }

    async fn emit_status<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        relay_blocked: bool,
        store: &Arc<Mutex<MessageStore>>,
    ) -> GDriveStatusDto {
        let status = self.snapshot_status(relay_blocked, store).await;
        let _ = app.emit("core://gdrive-sync-status", &status);
        status
    }

    async fn clear_for_logout_and_emit(
        &self,
        app: &AppHandle,
        relay_cfg: &Arc<Mutex<RelayConfig>>,
        store: &Arc<Mutex<MessageStore>>,
    ) {
        self.cancel_active_operations();
        let relay_enabled = relay_cfg.lock().await.enabled;
        {
            let mut inner = self.inner.lock().await;
            inner.uid = None;
            inner.tokens = None;
            inner.master_key = None;
            inner.has_remote_manifest = false;
            inner.syncing = false;
            inner.phase = if relay_enabled {
                GDRIVE_PHASE_PAUSED_RELAY.to_string()
            } else {
                GDRIVE_PHASE_IDLE.to_string()
            };
            inner.progress_done = 0;
            inner.progress_total = 0;
            inner.last_sync_ts = None;
            inner.last_history_sync_ts = None;
            inner.last_error = None;
        }
        let _ = self.emit_status(app, relay_enabled, store).await;
    }

    async fn rebind_for_uid_and_emit(
        &self,
        app: &AppHandle,
        uid: Option<&str>,
        relay_cfg: &Arc<Mutex<RelayConfig>>,
        store: &Arc<Mutex<MessageStore>>,
    ) {
        self.cancel_active_operations();
        let clean_uid = uid
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let (last_sync_ts, last_history_sync_ts) = {
            let guard = store.lock().await;
            (
                guard.get_gdrive_last_sync_ts().ok().flatten(),
                guard.get_gdrive_last_history_sync_ts().ok().flatten(),
            )
        };
        let (tokens, master_key) = if let Some(ref u) = clean_uid {
            let t = GDriveTokens::load_from_file(&self.paths.gdrive_tokens_file(Some(u)))
                .ok()
                .flatten();
            let k = if t.is_some() {
                CloudMasterKey::load_from_file(&self.paths.gdrive_master_key_file(Some(u)))
                    .ok()
                    .flatten()
            } else {
                None
            };
            (t, k)
        } else {
            (None, None)
        };
        let relay_enabled = relay_cfg.lock().await.enabled;
        let should_wake = {
            let mut inner = self.inner.lock().await;
            inner.uid = clean_uid;
            inner.has_remote_manifest = tokens
                .as_ref()
                .is_some_and(|t| t.manifest_file_id.is_some())
                || master_key.is_some();
            inner.tokens = tokens;
            inner.master_key = master_key;
            inner.syncing = false;
            inner.phase = if relay_enabled {
                GDRIVE_PHASE_PAUSED_RELAY.to_string()
            } else {
                GDRIVE_PHASE_IDLE.to_string()
            };
            inner.progress_done = 0;
            inner.progress_total = 0;
            inner.last_sync_ts = last_sync_ts;
            inner.last_history_sync_ts = last_history_sync_ts;
            inner.last_error = None;
            !relay_enabled
                && inner.config.sync_enabled
                && inner.tokens.is_some()
                && inner.master_key.is_some()
        };
        let _ = self.emit_status(app, relay_enabled, store).await;
        if should_wake {
            self.notify_sync();
        }
    }

    async fn pause_for_relay_and_emit(
        &self,
        app: &AppHandle,
        store: &Arc<Mutex<MessageStore>>,
    ) -> GDriveStatusDto {
        self.cancel_active_operations();
        {
            let mut inner = self.inner.lock().await;
            inner.syncing = false;
            inner.phase = GDRIVE_PHASE_PAUSED_RELAY.to_string();
            inner.progress_done = 0;
            inner.progress_total = 0;
        }
        self.emit_status(app, true, store).await
    }

    async fn resume_after_relay_and_emit(
        &self,
        app: &AppHandle,
        store: &Arc<Mutex<MessageStore>>,
    ) -> GDriveStatusDto {
        let should_wake = {
            let mut inner = self.inner.lock().await;
            if inner.phase == GDRIVE_PHASE_PAUSED_RELAY {
                inner.phase = GDRIVE_PHASE_IDLE.to_string();
            }
            inner.config.sync_enabled && inner.tokens.is_some() && inner.master_key.is_some()
        };
        let status = self.emit_status(app, false, store).await;
        if should_wake {
            self.notify_sync();
        }
        status
    }

    async fn ensure_fresh_tokens(
        &self,
        relay_enabled: bool,
    ) -> Result<(GDriveTokens, Option<String>), GDriveError> {
        ensure_relay_disabled(relay_enabled)?;
        let (mut tokens, oauth_cfg, uid_opt) = {
            let inner = self.inner.lock().await;
            let tokens = inner.tokens.clone().ok_or(GDriveError::NotConnected)?;
            let oauth_cfg = inner.config.resolve_oauth_config()?;
            (tokens, oauth_cfg, inner.uid.clone())
        };
        let refreshed = self
            .client
            .refresh_access_token_with_broker(
                relay_enabled,
                &oauth_cfg.client_id,
                oauth_cfg.client_secret.as_deref(),
                oauth_cfg.token_broker_url.as_deref(),
                &mut tokens,
            )
            .await?;
        if refreshed {
            let mut inner = self.inner.lock().await;
            if inner.uid == uid_opt && inner.tokens.is_some() {
                tokens.save_to_file(&self.paths.gdrive_tokens_file(uid_opt.as_deref()))?;
                inner.tokens = Some(tokens.clone());
            }
        }
        Ok((tokens, uid_opt))
    }

    async fn ensure_folders_cached(
        &self,
        relay_enabled: bool,
        tokens: &mut GDriveTokens,
        uid_opt: Option<&str>,
    ) -> Result<VaultFolderIds, GDriveError> {
        ensure_relay_disabled(relay_enabled)?;
        if let (Some(root), Some(uid_dir), Some(blobs), Some(history)) = (
            tokens.root_folder_id.clone(),
            tokens.uid_folder_id.clone(),
            tokens.blobs_folder_id.clone(),
            tokens.history_folder_id.clone(),
        ) {
            return Ok(VaultFolderIds {
                root_folder_id: root,
                uid_folder_id: uid_dir,
                blobs_folder_id: blobs,
                history_folder_id: history,
                manifest_file_id: tokens.manifest_file_id.clone(),
            });
        }
        let access_token = tokens.access_token.clone();
        let folders = self
            .client
            .ensure_vault_folders(
                relay_enabled,
                &access_token,
                effective_gdrive_uid(uid_opt),
                tokens,
            )
            .await?;
        {
            let mut inner = self.inner.lock().await;
            if inner.uid.as_deref() == uid_opt && inner.tokens.is_some() {
                let _ = tokens.save_to_file(&self.paths.gdrive_tokens_file(uid_opt));
                inner.tokens = Some(tokens.clone());
                inner.has_remote_manifest =
                    inner.has_remote_manifest || folders.manifest_file_id.is_some();
            }
        }
        Ok(folders)
    }
}

fn spawn_gdrive_background_sync_loop(
    app: AppHandle,
    gdrive: Arc<GDriveRuntime>,
    relay_cfg: Arc<Mutex<RelayConfig>>,
    store: Arc<Mutex<MessageStore>>,
    media_vault: Arc<RwLock<Arc<MediaVault>>>,
) {
    let gdrive_bg = gdrive.clone();
    let handle = tauri::async_runtime::spawn(async move {
        loop {
            let interval_secs = gdrive_bg.auto_sync_interval_secs().await;
            let woke_by_notify = tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => false,
                _ = gdrive_bg.sync_notify.notified() => true,
            };
            if woke_by_notify {
                tokio::time::sleep(std::time::Duration::from_secs(
                    GDRIVE_SYNC_WAKE_COALESCE_SECS,
                ))
                .await;
            }
            let relay_enabled = relay_cfg.lock().await.enabled;
            if relay_enabled || !gdrive_bg.can_background_sync().await {
                continue;
            }
            let _ =
                run_gdrive_sync_cycle(&app, &gdrive_bg, &relay_cfg, &store, &media_vault, false)
                    .await;
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut guard = gdrive.bg_task.lock().await;
        if let Some(prev) = guard.replace(handle) {
            prev.abort();
        }
    });
}

enum SyncCycleOutcome {
    Finished,
    Cancelled,
}

async fn run_gdrive_sync_cycle(
    app: &AppHandle,
    gdrive: &Arc<GDriveRuntime>,
    relay_cfg: &Arc<Mutex<RelayConfig>>,
    store: &Arc<Mutex<MessageStore>>,
    media_vault: &Arc<RwLock<Arc<MediaVault>>>,
    force: bool,
) -> Result<GDriveStatusDto, String> {
    let initial_relay = relay_cfg.lock().await.enabled;
    if initial_relay {
        let status = gdrive.snapshot_status(true, store).await;
        return if force {
            Err(GDriveError::RelayBlocked.to_string())
        } else {
            Ok(status)
        };
    }

    let _sync_guard = if force {
        gdrive.sync_lock.lock().await
    } else {
        match gdrive.sync_lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                let relay_now = relay_cfg.lock().await.enabled;
                return Ok(gdrive.snapshot_status(relay_now, store).await);
            }
        }
    };

    let relay_now = relay_cfg.lock().await.enabled;
    if relay_now {
        let status = gdrive.snapshot_status(true, store).await;
        return if force {
            Err(GDriveError::RelayBlocked.to_string())
        } else {
            Ok(status)
        };
    }

    enum SyncPrep {
        Ready {
            master_key: CloudMasterKey,
            uid_opt: Option<String>,
            sync_media: bool,
            sync_history: bool,
        },
        NotConnected,
        VaultLocked,
        Skip,
    }
    let prep = {
        let mut inner = gdrive.inner.lock().await;
        if inner.tokens.is_none() {
            SyncPrep::NotConnected
        } else if let Some(mk) = inner.master_key.clone() {
            if (!force && !inner.config.sync_enabled)
                || (!inner.config.sync_media && !inner.config.sync_history)
            {
                SyncPrep::Skip
            } else {
                inner.syncing = true;
                inner.last_error = None;
                inner.progress_done = 0;
                inner.progress_total = 0;
                inner.phase = if inner.config.sync_media {
                    GDRIVE_PHASE_UPLOADING_MEDIA.to_string()
                } else {
                    GDRIVE_PHASE_SYNCING_HISTORY.to_string()
                };
                SyncPrep::Ready {
                    master_key: mk,
                    uid_opt: inner.uid.clone(),
                    sync_media: inner.config.sync_media,
                    sync_history: inner.config.sync_history,
                }
            }
        } else {
            SyncPrep::VaultLocked
        }
    };
    let (master_key, uid_opt, sync_media, sync_history) = match prep {
        SyncPrep::Ready {
            master_key,
            uid_opt,
            sync_media,
            sync_history,
        } => (master_key, uid_opt, sync_media, sync_history),
        SyncPrep::NotConnected => {
            return if force {
                Err(GDriveError::NotConnected.to_string())
            } else {
                Ok(gdrive.snapshot_status(false, store).await)
            };
        }
        SyncPrep::VaultLocked => {
            return if force {
                Err(GDriveError::VaultLocked.to_string())
            } else {
                Ok(gdrive.snapshot_status(false, store).await)
            };
        }
        SyncPrep::Skip => {
            return Ok(gdrive.snapshot_status(false, store).await);
        }
    };

    let mut cancel_rx = gdrive.cancel_tx.subscribe();
    let _ = cancel_rx.borrow_and_update();
    let _ = gdrive.emit_status(app, false, store).await;

    let sync_body = async {
        let (mut tokens, _) = gdrive
            .ensure_fresh_tokens(relay_cfg.lock().await.enabled)
            .await
            .map_err(|e| e.to_string())?;
        let folders = gdrive
            .ensure_folders_cached(relay_cfg.lock().await.enabled, &mut tokens, uid_opt.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        // 1. Upload pending local media blobs first so history delta immediately carries new cloud_file_ids
        if sync_media {
            let vault = media_vault.read().await.clone();
            let initial_pending = {
                let guard = store.lock().await;
                guard
                    .compute_cloud_media_breakdown()
                    .map(|b| b.pending_count)
                    .unwrap_or(0)
            };
            if initial_pending > 0 {
                {
                    let mut inner = gdrive.inner.lock().await;
                    inner.phase = GDRIVE_PHASE_UPLOADING_MEDIA.to_string();
                    inner.progress_done = 0;
                    inner.progress_total = initial_pending;
                }
                let _ = gdrive.emit_status(app, false, store).await;

                let mut attempted = std::collections::HashSet::<String>::new();
                let mut uploaded_count: u64 = 0;
                loop {
                    let batch = {
                        let guard = store.lock().await;
                        guard
                            .list_pending_cloud_uploads(GDRIVE_MEDIA_SYNC_BATCH_LIMIT)
                            .map_err(|e| format!("SQLite pending cloud uploads failed: {e}"))?
                    };
                    let next_items: Vec<MediaObjectRecord> = batch
                        .into_iter()
                        .filter(|obj| attempted.insert(obj.sha256.clone()))
                        .collect();
                    if next_items.is_empty() {
                        break;
                    }

                    for obj in next_items {
                        let relay_step = relay_cfg.lock().await.enabled;
                        check_gdrive_relay_gate(relay_step)?;

                        let (fresh_tokens, _) = gdrive
                            .ensure_fresh_tokens(relay_step)
                            .await
                            .map_err(|e| e.to_string())?;
                        tokens = fresh_tokens;

                        let blob_name = format!("{}.tzmb", obj.sha256);
                        let existing_id = gdrive
                            .client
                            .find_file_in_folder(
                                relay_step,
                                &tokens.access_token,
                                &folders.blobs_folder_id,
                                &blob_name,
                                None,
                            )
                            .await
                            .map_err(|e| e.to_string())?
                            .map(|f| f.id);

                        let cloud_file_id = if let Some(fid) = existing_id {
                            fid
                        } else {
                            let tmp_guard =
                                TempFileGuard::new(vault.next_cloud_tmp_path("gdrive-up", "tzmb"));
                            if let Err(prep_err) = vault
                                .prepare_cloud_encrypted_temp(
                                    &obj,
                                    master_key.as_bytes(),
                                    tmp_guard.path(),
                                )
                                .await
                            {
                                let missing_on_disk = obj
                                    .local_rel_path
                                    .as_deref()
                                    .and_then(|rel| vault.resolve_blob_path(rel).ok())
                                    .is_some_and(|p| !p.exists());
                                if missing_on_disk {
                                    let guard = store.lock().await;
                                    let _ = guard.mark_media_evicted(&obj.sha256);
                                    continue;
                                }
                                return Err(format!(
                                    "Không chuẩn bị được tệp mã hóa đám mây: {prep_err}"
                                ));
                            }

                            let relay_upload = relay_cfg.lock().await.enabled;
                            check_gdrive_relay_gate(relay_upload)?;

                            let fid = gdrive
                                .client
                                .upload_file_resumable(
                                    relay_upload,
                                    &tokens.access_token,
                                    &folders.blobs_folder_id,
                                    &blob_name,
                                    "application/octet-stream",
                                    tmp_guard.path(),
                                )
                                .await
                                .map_err(|e| e.to_string())?;
                            drop(tmp_guard);
                            fid
                        };

                        let synced_ts = chrono_now_ms();
                        {
                            let guard = store.lock().await;
                            guard
                                .mark_media_cloud_synced(
                                    &obj.sha256,
                                    GDRIVE_PROVIDER_NAME,
                                    &cloud_file_id,
                                    1,
                                    synced_ts,
                                )
                                .map_err(|e| format!("SQLite mark_media_cloud_synced: {e}"))?;
                        }
                        uploaded_count = uploaded_count.saturating_add(1);
                        {
                            let mut inner = gdrive.inner.lock().await;
                            inner.progress_done = uploaded_count;
                            if inner.progress_total < uploaded_count {
                                inner.progress_total = uploaded_count;
                            }
                        }
                        let _ = gdrive.emit_status(app, false, store).await;
                    }
                }
            }
        }

        // 2. Sync encrypted delta message history (pull unseen remote deltas + push local changes)
        if sync_history {
            let relay_hist = relay_cfg.lock().await.enabled;
            check_gdrive_relay_gate(relay_hist)?;

            {
                let mut inner = gdrive.inner.lock().await;
                inner.phase = GDRIVE_PHASE_SYNCING_HISTORY.to_string();
                inner.progress_done = 0;
                inner.progress_total = 0;
            }
            let _ = gdrive.emit_status(app, false, store).await;

            let (fresh_tokens, _) = gdrive
                .ensure_fresh_tokens(relay_hist)
                .await
                .map_err(|e| e.to_string())?;
            tokens = fresh_tokens;

            let last_hist_ts = {
                let guard = store.lock().await;
                guard
                    .get_gdrive_last_history_sync_ts()
                    .ok()
                    .flatten()
                    .unwrap_or(0)
            };

            let report = gdrive
                .client
                .sync_history_deltas(
                    relay_hist,
                    &tokens.access_token,
                    &folders.history_folder_id,
                    store,
                    &master_key,
                    effective_gdrive_uid(uid_opt.as_deref()),
                    &gdrive.device_id,
                    last_hist_ts,
                )
                .await
                .map_err(|e| e.to_string())?;

            let persisted_hist_ts = {
                let guard = store.lock().await;
                guard.get_gdrive_last_history_sync_ts().ok().flatten()
            };
            {
                let mut inner = gdrive.inner.lock().await;
                if let Some(ts) = persisted_hist_ts {
                    inner.last_history_sync_ts = Some(ts);
                } else if report.new_last_history_sync_ts > 0 {
                    inner.last_history_sync_ts = Some(report.new_last_history_sync_ts);
                }
            }
            if report.downloaded_deltas > 0 || report.applied_messages > 0 {
                let _ = app.emit("core://threads-updated", ());
            }
        }

        let now_ms = chrono_now_ms();
        {
            let guard = store.lock().await;
            let _ = guard.set_gdrive_last_sync_ts(now_ms);
        }
        {
            let mut inner = gdrive.inner.lock().await;
            inner.last_sync_ts = Some(now_ms);
        }
        Ok::<SyncCycleOutcome, String>(SyncCycleOutcome::Finished)
    };

    let outcome = tokio::select! {
        biased;
        _ = cancel_rx.changed() => Ok(SyncCycleOutcome::Cancelled),
        res = sync_body => res,
    };

    let relay_after = relay_cfg.lock().await.enabled;
    match outcome {
        Ok(SyncCycleOutcome::Finished) => {
            {
                let mut inner = gdrive.inner.lock().await;
                inner.syncing = false;
                inner.phase = if relay_after {
                    GDRIVE_PHASE_PAUSED_RELAY.to_string()
                } else {
                    GDRIVE_PHASE_IDLE.to_string()
                };
                inner.progress_done = 0;
                inner.progress_total = 0;
                inner.last_error = None;
            }
            Ok(gdrive.emit_status(app, relay_after, store).await)
        }
        Ok(SyncCycleOutcome::Cancelled) => {
            {
                let mut inner = gdrive.inner.lock().await;
                inner.syncing = false;
                inner.phase = if relay_after {
                    GDRIVE_PHASE_PAUSED_RELAY.to_string()
                } else {
                    GDRIVE_PHASE_IDLE.to_string()
                };
                inner.progress_done = 0;
                inner.progress_total = 0;
            }
            let status = gdrive.emit_status(app, relay_after, store).await;
            if force && relay_after {
                Err(GDriveError::RelayBlocked.to_string())
            } else {
                Ok(status)
            }
        }
        Err(err_msg) => {
            {
                let mut inner = gdrive.inner.lock().await;
                inner.syncing = false;
                if relay_after {
                    inner.phase = GDRIVE_PHASE_PAUSED_RELAY.to_string();
                    inner.last_error = None;
                } else {
                    inner.phase = GDRIVE_PHASE_ERROR.to_string();
                    inner.last_error = Some(err_msg.clone());
                }
                inner.progress_done = 0;
                inner.progress_total = 0;
            }
            let _ = gdrive.emit_status(app, relay_after, store).await;
            Err(if relay_after {
                GDriveError::RelayBlocked.to_string()
            } else {
                err_msg
            })
        }
    }
}

struct CloudLookupResult {
    obj: MediaObjectRecord,
    original_name: Option<String>,
    cdn_expired: bool,
}

async fn lookup_cloud_candidate(
    store: &Arc<Mutex<MessageStore>>,
    url: &str,
    kind: &str,
    msg_ref: Option<(&str, &str)>,
) -> Option<CloudLookupResult> {
    let variant = if kind.eq_ignore_ascii_case("thumb") {
        "thumb"
    } else {
        "primary"
    };
    let guard = store.lock().await;
    if let Some((tid, mid)) = msg_ref {
        if let Ok(Some(obj)) = guard.lookup_media_by_msg(tid, mid, variant) {
            if obj
                .cloud_file_id
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                let mm = guard
                    .get_message_media_record(tid, mid, variant)
                    .ok()
                    .flatten();
                let original_name = mm.as_ref().and_then(|r| r.original_name.clone());
                let cdn_expired = mm
                    .as_ref()
                    .is_some_and(|r| r.cdn_status == MEDIA_CDN_STATUS_EXPIRED);
                return Some(CloudLookupResult {
                    obj,
                    original_name,
                    cdn_expired,
                });
            }
        }
    }
    if !url.trim().is_empty() {
        let url_hash = MediaVault::sha256_url(url);
        if let Ok(Some(obj)) = guard.lookup_media_by_url_hash(&url_hash) {
            if obj
                .cloud_file_id
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                let mm = guard
                    .get_message_media_record("__url__", &url_hash, variant)
                    .ok()
                    .flatten();
                let original_name = mm.as_ref().and_then(|r| r.original_name.clone());
                let cdn_expired = mm
                    .as_ref()
                    .is_some_and(|r| r.cdn_status == MEDIA_CDN_STATUS_EXPIRED);
                return Some(CloudLookupResult {
                    obj,
                    original_name,
                    cdn_expired,
                });
            }
        }
    }
    None
}

async fn try_rehydrate_local_hit(
    vault: &Arc<MediaVault>,
    store: &Arc<Mutex<MessageStore>>,
    sha256: &str,
) -> Option<MediaObjectRecord> {
    let obj = {
        let guard = store.lock().await;
        guard.get_media_object(sha256).ok().flatten()?
    };
    if obj.local_state != MEDIA_LOCAL_STATE_LOCAL {
        return None;
    }
    let rel = obj.local_rel_path.as_deref()?;
    let path = vault.resolve_blob_path(rel).ok()?;
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        Some(obj)
    } else {
        None
    }
}

struct RehydrateLeaderGuard<'a> {
    map: &'a std::sync::Mutex<HashMap<String, tokio::sync::watch::Receiver<bool>>>,
    sha: String,
    tx: Option<tokio::sync::watch::Sender<bool>>,
}

impl Drop for RehydrateLeaderGuard<'_> {
    fn drop(&mut self) {
        {
            let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(&self.sha);
        }
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(true);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn rehydrate_cloud_object<R: tauri::Runtime>(
    app: Option<&AppHandle<R>>,
    vault: &Arc<MediaVault>,
    store: &Arc<Mutex<MessageStore>>,
    gdrive: &Arc<GDriveRuntime>,
    relay_cfg: &Arc<Mutex<RelayConfig>>,
    candidate: &CloudLookupResult,
    url: &str,
    kind: &str,
    msg_ref: Option<(&str, &str)>,
) -> Result<MediaObjectRecord, GDriveError> {
    let sha = candidate.obj.sha256.clone();
    let cloud_file_id = candidate
        .obj
        .cloud_file_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or(GDriveError::NotFound)?;

    // Singleflight coordination per SHA-256 using watch::Receiver<bool> so Followers never miss Leader completion
    loop {
        enum RehydrateSlot<'a> {
            Leader(RehydrateLeaderGuard<'a>),
            Follower(tokio::sync::watch::Receiver<bool>),
        }
        let slot = {
            let mut map = gdrive
                .rehydrating
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = map.get(&sha) {
                RehydrateSlot::Follower(existing.clone())
            } else {
                let (tx, rx) = tokio::sync::watch::channel(false);
                map.insert(sha.clone(), rx);
                RehydrateSlot::Leader(RehydrateLeaderGuard {
                    map: &gdrive.rehydrating,
                    sha: sha.clone(),
                    tx: Some(tx),
                })
            }
        };
        match slot {
            RehydrateSlot::Follower(mut rx) => {
                let _ = rx.wait_for(|&done| done).await;
                if let Some(hit) = try_rehydrate_local_hit(vault, store, &sha).await {
                    return Ok(hit);
                }
                if relay_cfg.lock().await.enabled {
                    return Err(GDriveError::RelayBlocked);
                }
                let still_has_cloud = {
                    let guard = store.lock().await;
                    guard
                        .get_media_object(&sha)
                        .ok()
                        .flatten()
                        .and_then(|o| o.cloud_file_id)
                        .is_some_and(|s| !s.trim().is_empty())
                };
                if !still_has_cloud {
                    return Err(GDriveError::NotFound);
                }
                continue;
            }
            RehydrateSlot::Leader(_leader_guard) => {
                let result: Result<MediaObjectRecord, GDriveError> = async {
                    if let Some(hit) = try_rehydrate_local_hit(vault, store, &sha).await {
                        return Ok(hit);
                    }
                    let relay_now = relay_cfg.lock().await.enabled;
                    ensure_relay_disabled(relay_now)?;
                    let master_key = {
                        let inner = gdrive.inner.lock().await;
                        if inner.tokens.is_none() {
                            return Err(GDriveError::NotConnected);
                        }
                        inner.master_key.clone().ok_or(GDriveError::VaultLocked)?
                    };
                    let (tokens, _) = gdrive.ensure_fresh_tokens(relay_now).await?;

                    let set_rehydrating_phase = {
                        let mut inner = gdrive.inner.lock().await;
                        if !inner.syncing {
                            inner.phase = GDRIVE_PHASE_REHYDRATING.to_string();
                            true
                        } else {
                            false
                        }
                    };
                    if set_rehydrating_phase {
                        if let Some(app_h) = app {
                            let _ = gdrive.emit_status(app_h, false, store).await;
                        }
                    }

                    let mut cancel_rx = gdrive.cancel_tx.subscribe();
                    let _ = cancel_rx.borrow_and_update();

                    let tmp_guard =
                        TempFileGuard::new(vault.next_cloud_tmp_path("gdrive-dl", "tzmb"));
                    let dl_res = tokio::select! {
                        biased;
                        _ = cancel_rx.changed() => Err(GDriveError::RelayBlocked),
                        res = gdrive.client.download_file_to_path(
                            relay_cfg.lock().await.enabled,
                            &tokens.access_token,
                            &cloud_file_id,
                            tmp_guard.path(),
                        ) => res,
                    };

                    if set_rehydrating_phase {
                        let relay_after = relay_cfg.lock().await.enabled;
                        {
                            let mut inner = gdrive.inner.lock().await;
                            if inner.phase == GDRIVE_PHASE_REHYDRATING {
                                inner.phase = if relay_after {
                                    GDRIVE_PHASE_PAUSED_RELAY.to_string()
                                } else {
                                    GDRIVE_PHASE_IDLE.to_string()
                                };
                            }
                        }
                        if let Some(app_h) = app {
                            let _ = gdrive.emit_status(app_h, relay_after, store).await;
                        }
                    }

                    match dl_res {
                        Ok(_) => {}
                        Err(GDriveError::NotFound) => {
                            let guard = store.lock().await;
                            let _ = guard.clear_media_cloud_links(std::slice::from_ref(&sha));
                            return Err(GDriveError::NotFound);
                        }
                        Err(e) => return Err(e),
                    }

                    let eff_kind = if candidate.obj.kind.trim().is_empty() {
                        kind
                    } else {
                        candidate.obj.kind.as_str()
                    };
                    let eff_mime = if candidate.obj.mime.trim().is_empty() {
                        "application/octet-stream"
                    } else {
                        candidate.obj.mime.as_str()
                    };
                    let variant = if eff_kind.eq_ignore_ascii_case("thumb") {
                        "thumb"
                    } else {
                        "primary"
                    };
                    let msg_ref_variant = msg_ref.map(|(t, m)| (t, m, variant));

                    let obj = vault
                        .ingest_from_cloud_encrypted_temp(
                            store,
                            tmp_guard.path(),
                            master_key.as_bytes(),
                            &sha,
                            eff_kind,
                            eff_mime,
                            (!url.trim().is_empty()).then_some(url),
                            msg_ref_variant,
                            candidate.original_name.as_deref(),
                        )
                        .await
                        .map_err(|e| GDriveError::Other(e.to_string()))?;
                    drop(tmp_guard);

                    if vault.config().quota_mb > 0 {
                        let _ = vault
                            .run_lru_eviction_excluding(store, Some(&obj.sha256))
                            .await;
                    }
                    Ok(obj)
                }
                .await;

                return result;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_media_with_tier2_gdrive<R: tauri::Runtime, F>(
    app: Option<&AppHandle<R>>,
    vault: &Arc<MediaVault>,
    session: &Session,
    store: &Arc<Mutex<MessageStore>>,
    gdrive: &Arc<GDriveRuntime>,
    relay_cfg: &Arc<Mutex<RelayConfig>>,
    url: &str,
    kind: &str,
    msg_ref: Option<(&str, &str)>,
    on_progress: F,
) -> Result<MediaObjectRecord, anyhow::Error>
where
    F: FnMut(u64, Option<u64>),
{
    // Fast Tier 1 local hit check when a cloud record already knows CDN is expired
    if let Some(candidate) = lookup_cloud_candidate(store, url, kind, msg_ref).await {
        if candidate.obj.local_state == MEDIA_LOCAL_STATE_MISSING && candidate.cdn_expired {
            let relay_enabled = relay_cfg.lock().await.enabled;
            if !relay_enabled && gdrive.is_connected_and_unlocked().await {
                match rehydrate_cloud_object(
                    app, vault, store, gdrive, relay_cfg, &candidate, url, kind, msg_ref,
                )
                .await
                {
                    Ok(obj) => return Ok(obj),
                    Err(GDriveError::NotFound) => {
                        return Err(anyhow::Error::new(MediaDownloadError::HttpStatus {
                            status: 404,
                        }));
                    }
                    Err(_) => {}
                }
            }
        }
    }

    match vault
        .ensure_cached_url(session, store, url, kind, msg_ref, on_progress)
        .await
    {
        Ok(obj) => {
            if obj.cloud_file_id.is_none() {
                gdrive.notify_sync();
            }
            Ok(obj)
        }
        Err(primary_err) => {
            let candidate = lookup_cloud_candidate(store, url, kind, msg_ref).await;
            let relay_enabled = relay_cfg.lock().await.enabled;
            let gdrive_ready = if relay_enabled {
                false
            } else {
                gdrive.is_connected_and_unlocked().await
            };
            if should_trigger_gdrive_rehydration(
                relay_enabled,
                gdrive_ready,
                &primary_err,
                candidate.as_ref().map(|c| &c.obj),
            ) {
                if let Some(cand) = candidate {
                    return match rehydrate_cloud_object(
                        app, vault, store, gdrive, relay_cfg, &cand, url, kind, msg_ref,
                    )
                    .await
                    {
                        Ok(obj) => Ok(obj),
                        Err(GDriveError::NotFound) => {
                            Err(anyhow::Error::new(MediaDownloadError::HttpStatus {
                                status: 404,
                            }))
                        }
                        Err(_) => Err(primary_err),
                    };
                }
            }
            Err(primary_err)
        }
    }
}

#[tauri::command]
async fn gdrive_get_config(state: State<'_, AppState>) -> Result<GDriveConfigDto, String> {
    let inner = state.gdrive.inner.lock().await;
    Ok(inner.config.to_dto())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn gdrive_save_config(
    app: AppHandle,
    state: State<'_, AppState>,
    client_id: Option<String>,
    client_secret: Option<String>,
    token_broker_url: Option<String>,
    reset_custom_oauth: Option<bool>,
    sync_enabled: Option<bool>,
    sync_media: Option<bool>,
    sync_history: Option<bool>,
) -> Result<GDriveConfigDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    let (dto, should_wake) = {
        let mut inner = state.gdrive.inner.lock().await;
        let mut staged = inner.config.clone();
        if reset_custom_oauth.unwrap_or(false) {
            staged.reset_custom_oauth();
        } else {
            if let Some(cid) = client_id {
                staged.client_id = cid.trim().to_string();
            }
            if let Some(secret) = client_secret {
                staged
                    .set_client_secret(Some(&secret))
                    .map_err(|e| e.to_string())?;
            }
            if let Some(broker) = token_broker_url {
                staged
                    .set_token_broker_url(Some(&broker))
                    .map_err(|e| e.to_string())?;
            }
        }
        if let Some(v) = sync_enabled {
            staged.sync_enabled = v;
        }
        if let Some(v) = sync_media {
            staged.sync_media = v;
        }
        if let Some(v) = sync_history {
            staged.sync_history = v;
        }
        staged
            .save_to_file(&state.paths.gdrive_config_file())
            .map_err(|e| e.to_string())?;
        inner.config = staged;
        let dto = inner.config.to_dto();
        let wake = !relay_enabled
            && inner.config.sync_enabled
            && inner.tokens.is_some()
            && inner.master_key.is_some();
        (dto, wake)
    };
    let _ = state
        .gdrive
        .emit_status(&app, relay_enabled, &state.store)
        .await;
    if should_wake {
        state.gdrive.notify_sync();
    }
    Ok(dto)
}

#[tauri::command]
async fn gdrive_get_status(state: State<'_, AppState>) -> Result<GDriveStatusDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    Ok(state
        .gdrive
        .snapshot_status(relay_enabled, &state.store)
        .await)
}

#[tauri::command]
async fn gdrive_connect_start(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<GDriveStatusDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;

    let _connect_guard = state.gdrive.connect_lock.lock().await;
    let relay_check = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_check)?;

    let current_uid = {
        let sess = state.session.lock().await;
        let u = sess.profile.uid.trim();
        (!u.is_empty()).then(|| u.to_string())
    };
    let (oauth_cfg, uid_opt) = {
        let mut inner = state.gdrive.inner.lock().await;
        if current_uid.is_some() {
            inner.uid = current_uid;
        }
        let oauth_cfg = inner
            .config
            .resolve_oauth_config()
            .map_err(|e| e.to_string())?;
        (oauth_cfg, inner.uid.clone())
    };

    let flow = OAuthLoopbackFlow::bind(relay_check, &oauth_cfg.client_id)
        .await
        .map_err(|e| e.to_string())?;
    let auth_url = flow.auth_url.clone();

    // Subscribe to cancellation watch BEFORE launching the browser so Relay-ON or disconnect
    // during `open_url_with_shell` can never miss cancellation and leave the listener blocked.
    let mut cancel_rx = state.gdrive.cancel_tx.subscribe();
    let _ = cancel_rx.borrow_and_update();

    {
        let mut inner = state.gdrive.inner.lock().await;
        inner.phase = GDRIVE_PHASE_CONNECTING.to_string();
        inner.last_error = None;
    }
    let _ = state.gdrive.emit_status(&app, false, &state.store).await;

    let open_res = tokio::task::spawn_blocking(move || file_dialog::open_url_with_shell(&auth_url))
        .await
        .map_err(|e| format!("Không mở được trình duyệt hệ thống: {e}"))
        .and_then(|r| r);
    if let Err(err_msg) = open_res {
        let relay_now = state.relay_cfg.lock().await.enabled;
        {
            let mut inner = state.gdrive.inner.lock().await;
            inner.phase = if relay_now {
                GDRIVE_PHASE_PAUSED_RELAY.to_string()
            } else {
                GDRIVE_PHASE_ERROR.to_string()
            };
            inner.last_error = if relay_now { None } else { Some(err_msg.clone()) };
        }
        let _ = state.gdrive.emit_status(&app, relay_now, &state.store).await;
        return Err(err_msg);
    }

    let relay_before_wait = state.relay_cfg.lock().await.enabled;
    let wait_res = if relay_before_wait {
        Err(GDriveError::RelayBlocked)
    } else if cancel_rx.has_changed().unwrap_or(false) {
        Err(GDriveError::AuthCancelled)
    } else {
        tokio::select! {
            biased;
            _ = cancel_rx.changed() => {
                if state.relay_cfg.lock().await.enabled {
                    Err(GDriveError::RelayBlocked)
                } else {
                    Err(GDriveError::AuthCancelled)
                }
            }
            res = flow.wait_for_code(Some(state.gdrive.oauth_cancel.clone())) => res,
        }
    };

    let (code, code_verifier, redirect_uri) = match wait_res {
        Ok(tuple) => tuple,
        Err(e) => {
            let relay_now = state.relay_cfg.lock().await.enabled;
            let err_msg = if relay_now {
                GDriveError::RelayBlocked.to_string()
            } else {
                e.to_string()
            };
            {
                let mut inner = state.gdrive.inner.lock().await;
                inner.phase = if relay_now {
                    GDRIVE_PHASE_PAUSED_RELAY.to_string()
                } else {
                    GDRIVE_PHASE_IDLE.to_string()
                };
                inner.last_error = if relay_now
                    || matches!(e, GDriveError::AuthCancelled | GDriveError::RelayBlocked)
                {
                    None
                } else {
                    Some(err_msg.clone())
                };
            }
            let _ = state.gdrive.emit_status(&app, relay_now, &state.store).await;
            return Err(err_msg);
        }
    };

    let relay_after_cb = state.relay_cfg.lock().await.enabled;
    if let Err(err_msg) = check_gdrive_relay_gate(relay_after_cb) {
        let _ = state.gdrive.emit_status(&app, true, &state.store).await;
        return Err(err_msg);
    }

    // Bring Teza back to the foreground right after the browser OAuth callback completes.
    if let Some(main_win) = app.get_window("main") {
        let _ = main_win.unminimize();
        let _ = main_win.set_focus();
    }

    let setup_res: Result<(GDriveTokens, bool, Option<String>), GDriveError> = async {
        let mut tokens = state
            .gdrive
            .client
            .exchange_oauth_code_with_broker(
                relay_after_cb,
                &oauth_cfg.client_id,
                oauth_cfg.client_secret.as_deref(),
                oauth_cfg.token_broker_url.as_deref(),
                &code,
                &code_verifier,
                &redirect_uri,
            )
            .await?;

        let (quota, email, name) = state
            .gdrive
            .client
            .fetch_drive_quota(state.relay_cfg.lock().await.enabled, &tokens.access_token)
            .await?;
        tokens.account_email = email;
        tokens.account_name = name;

        let access_token = tokens.access_token.clone();
        let (has_manifest, mut connect_warning) = match state
            .gdrive
            .client
            .ensure_vault_folders(
                state.relay_cfg.lock().await.enabled,
                &access_token,
                effective_gdrive_uid(uid_opt.as_deref()),
                &mut tokens,
            )
            .await
        {
            Ok(folders) => (folders.manifest_file_id.is_some(), None),
            Err(GDriveError::StorageQuotaExceeded) => (
                false,
                Some(GDriveError::StorageQuotaExceeded.to_string()),
            ),
            Err(e) => return Err(e),
        };
        if connect_warning.is_none()
            && quota
                .drive_limit_bytes
                .is_some_and(|lim| lim > 0 && quota.drive_usage_bytes >= lim)
        {
            connect_warning = Some(GDriveError::StorageQuotaExceeded.to_string());
        }
        Ok((tokens, has_manifest, connect_warning))
    }
    .await;

    let (tokens, has_manifest, connect_warning) = match setup_res {
        Ok(triple) => triple,
        Err(e) => {
            let relay_now = state.relay_cfg.lock().await.enabled;
            let err_msg = if relay_now {
                GDriveError::RelayBlocked.to_string()
            } else {
                e.to_string()
            };
            {
                let mut inner = state.gdrive.inner.lock().await;
                inner.phase = if relay_now {
                    GDRIVE_PHASE_PAUSED_RELAY.to_string()
                } else {
                    GDRIVE_PHASE_ERROR.to_string()
                };
                inner.last_error = if relay_now { None } else { Some(err_msg.clone()) };
            }
            let _ = state.gdrive.emit_status(&app, relay_now, &state.store).await;
            return Err(err_msg);
        }
    };

    let relay_final = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_final)?;

    // Save DPAPI-sealed tokens and clear any previous cached master key so passphrase unlock matches the newly connected Drive
    tokens
        .save_to_file(&state.paths.gdrive_tokens_file(uid_opt.as_deref()))
        .map_err(|e| e.to_string())?;
    CloudMasterKey::delete_file(&state.paths.gdrive_master_key_file(uid_opt.as_deref()));

    {
        let mut inner = state.gdrive.inner.lock().await;
        inner.tokens = Some(tokens);
        inner.master_key = None;
        inner.has_remote_manifest = has_manifest;
        inner.syncing = false;
        inner.phase = GDRIVE_PHASE_IDLE.to_string();
        inner.progress_done = 0;
        inner.progress_total = 0;
        inner.last_error = connect_warning;
    }

    Ok(state.gdrive.emit_status(&app, false, &state.store).await)
}

#[tauri::command]
async fn gdrive_connect_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<GDriveStatusDto, String> {
    state.gdrive.cancel_oauth_flow();
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    {
        let mut inner = state.gdrive.inner.lock().await;
        if inner.phase == GDRIVE_PHASE_CONNECTING {
            inner.phase = if relay_enabled {
                GDRIVE_PHASE_PAUSED_RELAY.to_string()
            } else {
                GDRIVE_PHASE_IDLE.to_string()
            };
            inner.last_error = None;
        }
    }
    Ok(state
        .gdrive
        .emit_status(&app, relay_enabled, &state.store)
        .await)
}

#[tauri::command]
async fn gdrive_disconnect(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<GDriveStatusDto, String> {
    state.gdrive.cancel_active_operations();
    let session_uid = {
        let sess = state.session.lock().await;
        let u = sess.profile.uid.trim();
        (!u.is_empty()).then(|| u.to_string())
    };
    let stored_uid = {
        let mut inner = state.gdrive.inner.lock().await;
        let prev_uid = inner.uid.clone();
        inner.tokens = None;
        inner.master_key = None;
        inner.has_remote_manifest = false;
        inner.syncing = false;
        inner.phase = GDRIVE_PHASE_IDLE.to_string();
        inner.progress_done = 0;
        inner.progress_total = 0;
        inner.last_error = None;
        prev_uid
    };
    // Remove local DPAPI tokens & cached master key for active UID while leaving remote Drive backup intact
    GDriveTokens::delete_file(&state.paths.gdrive_tokens_file(stored_uid.as_deref()));
    CloudMasterKey::delete_file(&state.paths.gdrive_master_key_file(stored_uid.as_deref()));
    if session_uid != stored_uid {
        GDriveTokens::delete_file(&state.paths.gdrive_tokens_file(session_uid.as_deref()));
        CloudMasterKey::delete_file(&state.paths.gdrive_master_key_file(session_uid.as_deref()));
    }
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    Ok(state
        .gdrive
        .emit_status(&app, relay_enabled, &state.store)
        .await)
}

#[tauri::command]
async fn gdrive_unlock_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    passphrase: String,
) -> Result<GDriveStatusDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;
    validate_gdrive_passphrase(&passphrase)?;

    let (mut tokens, uid_opt) = state
        .gdrive
        .ensure_fresh_tokens(relay_enabled)
        .await
        .map_err(|e| e.to_string())?;

    let access_token = tokens.access_token.clone();
    let (master_key, _created_new) = state
        .gdrive
        .client
        .unlock_or_init_vault(
            state.relay_cfg.lock().await.enabled,
            &access_token,
            effective_gdrive_uid(uid_opt.as_deref()),
            &passphrase,
            &mut tokens,
        )
        .await
        .map_err(|e| e.to_string())?;

    let relay_after = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_after)?;

    master_key
        .save_to_file(&state.paths.gdrive_master_key_file(uid_opt.as_deref()))
        .map_err(|e| e.to_string())?;
    tokens
        .save_to_file(&state.paths.gdrive_tokens_file(uid_opt.as_deref()))
        .map_err(|e| e.to_string())?;

    let should_wake = {
        let mut inner = state.gdrive.inner.lock().await;
        if inner.uid != uid_opt {
            return Err("Tài khoản đã thay đổi trong lúc mở khóa kho đám mây".into());
        }
        inner.tokens = Some(tokens);
        inner.master_key = Some(master_key);
        inner.has_remote_manifest = true;
        inner.phase = GDRIVE_PHASE_IDLE.to_string();
        inner.last_error = None;
        inner.config.sync_enabled
    };

    let status = state.gdrive.emit_status(&app, false, &state.store).await;
    if should_wake {
        state.gdrive.notify_sync();
    }
    Ok(status)
}

#[tauri::command]
async fn gdrive_sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<GDriveStatusDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;
    run_gdrive_sync_cycle(
        &app,
        &state.gdrive,
        &state.relay_cfg,
        &state.store,
        &state.media_vault,
        true,
    )
    .await
}

#[tauri::command]
async fn gdrive_get_storage_overview(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<GDriveStorageOverviewDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;

    let (mut tokens, uid_opt) = state
        .gdrive
        .ensure_fresh_tokens(relay_enabled)
        .await
        .map_err(|e| e.to_string())?;

    let (quota, email, name) = state
        .gdrive
        .client
        .fetch_drive_quota(state.relay_cfg.lock().await.enabled, &tokens.access_token)
        .await
        .map_err(|e| e.to_string())?;

    if email.is_some() || name.is_some() {
        let mut changed = false;
        if email.is_some() && tokens.account_email != email {
            tokens.account_email = email;
            changed = true;
        }
        if name.is_some() && tokens.account_name != name {
            tokens.account_name = name;
            changed = true;
        }
        if changed {
            let mut inner = state.gdrive.inner.lock().await;
            if inner.uid == uid_opt && inner.tokens.is_some() {
                let _ = tokens.save_to_file(&state.paths.gdrive_tokens_file(uid_opt.as_deref()));
                inner.tokens = Some(tokens.clone());
            }
        }
    }

    let folders = state
        .gdrive
        .ensure_folders_cached(
            state.relay_cfg.lock().await.enabled,
            &mut tokens,
            uid_opt.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;

    let history_files = state
        .gdrive
        .client
        .list_folder_files(
            state.relay_cfg.lock().await.enabled,
            &tokens.access_token,
            &folders.history_folder_id,
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut history_bytes: u64 = 0;
    let mut history_file_count: u64 = 0;
    for f in history_files {
        if f.name.ends_with(".delta.enc") {
            history_bytes = history_bytes.saturating_add(f.size);
            history_file_count = history_file_count.saturating_add(1);
        }
    }

    check_gdrive_relay_gate(state.relay_cfg.lock().await.enabled)?;

    let clamped_limit = limit.unwrap_or(50).clamp(1, 500);
    let (breakdown, by_thread, largest_items) = {
        let guard = state.store.lock().await;
        let breakdown = guard
            .compute_cloud_media_breakdown()
            .map_err(|e| e.to_string())?;
        let by_thread = guard
            .list_cloud_usage_by_thread()
            .map_err(|e| e.to_string())?;
        let largest_items = guard
            .list_largest_cloud_items(clamped_limit)
            .map_err(|e| e.to_string())?;
        (breakdown, by_thread, largest_items)
    };

    Ok(GDriveStorageOverviewDto {
        quota: Some(quota),
        teza_cloud_total_bytes: breakdown.total_bytes.saturating_add(history_bytes),
        teza_cloud_object_count: breakdown.total_count,
        images_bytes: breakdown.images_bytes,
        videos_bytes: breakdown.videos_bytes,
        voice_bytes: breakdown.voice_bytes,
        files_bytes: breakdown.files_bytes,
        history_bytes,
        history_file_count,
        by_thread,
        largest_items,
    })
}

async fn execute_gdrive_prune_targets(
    app: &AppHandle,
    state: &State<'_, AppState>,
    targets: Vec<(String, String, u64)>,
) -> Result<GDrivePruneResultDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;

    let _sync_guard = state.gdrive.sync_lock.lock().await;
    check_gdrive_relay_gate(state.relay_cfg.lock().await.enabled)?;

    let (mut tokens, _) = state
        .gdrive
        .ensure_fresh_tokens(state.relay_cfg.lock().await.enabled)
        .await
        .map_err(|e| e.to_string())?;

    if targets.is_empty() {
        let _ = state.gdrive.emit_status(app, false, &state.store).await;
        return Ok(GDrivePruneResultDto::default());
    }

    let total = targets.len() as u64;
    {
        let mut inner = state.gdrive.inner.lock().await;
        inner.syncing = true;
        inner.phase = GDRIVE_PHASE_PRUNING.to_string();
        inner.progress_done = 0;
        inner.progress_total = total;
        inner.last_error = None;
    }
    let _ = state.gdrive.emit_status(app, false, &state.store).await;

    let mut deleted_shas: Vec<String> = Vec::with_capacity(targets.len());
    let mut deleted_count: u64 = 0;
    let mut freed_bytes: u64 = 0;
    let mut failed_count: u64 = 0;
    let mut relay_interrupted = false;

    for (idx, (sha, cloud_file_id, size_bytes)) in targets.into_iter().enumerate() {
        let relay_step = state.relay_cfg.lock().await.enabled;
        if relay_step {
            relay_interrupted = true;
            break;
        }
        if let Ok((fresh_tokens, _)) = state.gdrive.ensure_fresh_tokens(relay_step).await {
            tokens = fresh_tokens;
        }
        match state
            .gdrive
            .client
            .delete_drive_file(relay_step, &tokens.access_token, &cloud_file_id)
            .await
        {
            Ok(()) => {
                deleted_shas.push(sha);
                deleted_count = deleted_count.saturating_add(1);
                freed_bytes = freed_bytes.saturating_add(size_bytes);
            }
            Err(GDriveError::RelayBlocked) => {
                relay_interrupted = true;
                break;
            }
            Err(_) => {
                failed_count = failed_count.saturating_add(1);
            }
        }
        {
            let mut inner = state.gdrive.inner.lock().await;
            inner.progress_done = (idx as u64).saturating_add(1);
        }
        let _ = state.gdrive.emit_status(app, false, &state.store).await;
    }

    if !deleted_shas.is_empty() {
        let guard = state.store.lock().await;
        let _ = guard.mark_media_cloud_pruned(&deleted_shas);
    }

    let relay_after = relay_interrupted || state.relay_cfg.lock().await.enabled;
    {
        let mut inner = state.gdrive.inner.lock().await;
        inner.syncing = false;
        inner.phase = if relay_after {
            GDRIVE_PHASE_PAUSED_RELAY.to_string()
        } else {
            GDRIVE_PHASE_IDLE.to_string()
        };
        inner.progress_done = 0;
        inner.progress_total = 0;
    }
    let _ = state
        .gdrive
        .emit_status(app, relay_after, &state.store)
        .await;

    if relay_interrupted {
        return Err(GDriveError::RelayBlocked.to_string());
    }
    Ok(GDrivePruneResultDto {
        deleted_count,
        freed_bytes,
        failed_count,
    })
}

#[tauri::command]
async fn gdrive_delete_cloud_items(
    app: AppHandle,
    state: State<'_, AppState>,
    sha256s: Vec<String>,
) -> Result<GDrivePruneResultDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;
    let targets = {
        let guard = state.store.lock().await;
        guard
            .list_cloud_items_by_sha256s(&sha256s)
            .map_err(|e| e.to_string())?
    };
    execute_gdrive_prune_targets(&app, &state, targets).await
}

#[tauri::command]
async fn gdrive_delete_cloud_by_thread(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<GDrivePruneResultDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;
    let trimmed = thread_id.trim();
    if trimmed.is_empty() {
        return Err("Mã cuộc trò chuyện không hợp lệ".into());
    }
    let targets = {
        let guard = state.store.lock().await;
        guard
            .list_cloud_items_exclusive_to_thread(trimmed)
            .map_err(|e| e.to_string())?
    };
    execute_gdrive_prune_targets(&app, &state, targets).await
}

#[tauri::command]
async fn gdrive_prune_cloud_older_than(
    app: AppHandle,
    state: State<'_, AppState>,
    older_than_days: u32,
) -> Result<GDrivePruneResultDto, String> {
    let relay_enabled = state.relay_cfg.lock().await.enabled;
    check_gdrive_relay_gate(relay_enabled)?;
    let cutoff_ms = compute_prune_cutoff_ts_ms(chrono_now_ms(), older_than_days);
    let targets = {
        let guard = state.store.lock().await;
        guard
            .list_cloud_items_older_than(cutoff_ms)
            .map_err(|e| e.to_string())?
    };
    execute_gdrive_prune_targets(&app, &state, targets).await
}

static AI_MSG_SEQ: AtomicU64 = AtomicU64::new(1);

fn normalize_ai_session_id(session_id: Option<&str>) -> String {
    match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => "default".to_string(),
    }
}

async fn translate_fallback_gemini_key(state: &AppState) -> Option<String> {
    let cfg = state.translate_cfg.lock().await;
    let trimmed = cfg.gemini_key.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[tauri::command]
async fn ai_get_config(state: State<'_, AppState>) -> Result<AiConfigPublic, String> {
    let fallback_key = translate_fallback_gemini_key(&state).await;
    let cfg = state.ai_config.lock().await;
    Ok(cfg.to_public(fallback_key.as_deref()))
}

#[tauri::command]
async fn ai_update_config(
    state: State<'_, AppState>,
    update: AiConfigUpdate,
) -> Result<AiConfigPublic, String> {
    let staged = {
        let cfg = state.ai_config.lock().await;
        cfg.with_update(update).map_err(|e| e.to_string())?
    };
    let path = ai_config_file(&state.paths.root);
    staged
        .save_to_file(&path)
        .map_err(|e| format!("Không lưu được cấu hình Trợ lý AI: {e}"))?;
    {
        let mut cfg = state.ai_config.lock().await;
        *cfg = staged.clone();
    }
    let fallback_key = translate_fallback_gemini_key(&state).await;
    Ok(staged.to_public(fallback_key.as_deref()))
}

#[tauri::command]
async fn ai_probe_cli(
    state: State<'_, AppState>,
    cli_path: Option<String>,
) -> Result<AiCliProbeDto, String> {
    let target = match cli_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(p) => p.to_string(),
        None => state.ai_config.lock().await.cli_path.clone(),
    };
    Ok(probe_gemini_cli(&target).await)
}

#[tauri::command]
async fn ai_list_messages(
    state: State<'_, AppState>,
    session_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<AiStoredMessageDto>, String> {
    let sid = normalize_ai_session_id(session_id.as_deref());
    let capped = limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.lock().await;
    store
        .list_ai_messages(&sid, capped)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_clear_messages(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<usize, String> {
    let sid = normalize_ai_session_id(session_id.as_deref());
    let store = state.store.lock().await;
    store.clear_ai_messages(&sid).map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    prompt: String,
    session_id: Option<String>,
) -> Result<AiStoredMessageDto, String> {
    let prompt_clean = prompt.trim();
    if prompt_clean.is_empty() {
        return Err("Nội dung câu hỏi cho Trợ lý AI không được để trống".into());
    }
    let cfg = state.ai_config.lock().await.clone();
    if !cfg.enabled {
        return Err(
            "Trợ lý AI đang tắt. Hãy bật Trợ lý AI trong Cài đặt trước khi sử dụng.".into(),
        );
    }

    let sid = normalize_ai_session_id(session_id.as_deref());
    let relay = state.relay_cfg.lock().await.clone();
    let fallback_key = translate_fallback_gemini_key(&state).await;

    let user_ts = chrono_now_ms();
    let user_seq = AI_MSG_SEQ.fetch_add(1, Ordering::Relaxed);
    let user_msg = AiStoredMessageDto {
        id: format!("ai-u-{user_ts}-{user_seq}"),
        session_id: sid.clone(),
        role: "user".to_string(),
        content: prompt_clean.to_string(),
        model: String::new(),
        backend: String::new(),
        ts: user_ts,
    };

    let history: Vec<AiChatTurn> = {
        let store = state.store.lock().await;
        let prior = store
            .list_ai_messages(&sid, MAX_CHAT_HISTORY_TURNS)
            .map_err(|e| e.to_string())?;
        store
            .insert_ai_message(&user_msg)
            .map_err(|e| e.to_string())?;
        prior
            .into_iter()
            .map(|m| AiChatTurn {
                role: m.role,
                content: m.content,
            })
            .collect()
    };

    let sid_for_emit = sid.clone();
    let reply = ai_chat_stream_with_relay(
        &cfg,
        &relay,
        fallback_key.as_deref(),
        &history,
        prompt_clean,
        |delta| {
            let _ = app.emit(
                "core://ai-stream-delta",
                serde_json::json!({
                    "session_id": sid_for_emit,
                    "delta": delta,
                }),
            );
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let model_ts = chrono_now_ms().max(user_ts.saturating_add(1));
    let model_seq = AI_MSG_SEQ.fetch_add(1, Ordering::Relaxed);
    let model_msg = AiStoredMessageDto {
        id: format!("ai-m-{model_ts}-{model_seq}"),
        session_id: sid,
        role: "model".to_string(),
        content: reply.text,
        model: reply.model,
        backend: reply.backend.as_str().to_string(),
        ts: model_ts,
    };
    {
        let store = state.store.lock().await;
        store
            .insert_ai_message(&model_msg)
            .map_err(|e| e.to_string())?;
    }
    Ok(model_msg)
}

#[tauri::command]
async fn ai_summarize_unread(
    app: AppHandle,
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<AiSummaryDto, String> {
    let tid = thread_id.trim().to_string();
    if tid.is_empty() {
        return Err("Mã cuộc trò chuyện không được để trống".into());
    }
    let cfg = state.ai_config.lock().await.clone();
    if !cfg.enabled {
        return Err(
            "Trợ lý AI đang tắt. Hãy bật Trợ lý AI trong Cài đặt trước khi sử dụng.".into(),
        );
    }
    let relay = state.relay_cfg.lock().await.clone();
    let fallback_key = translate_fallback_gemini_key(&state).await;

    let (thread_title, is_group, unread_msgs) = {
        let store = state.store.lock().await;
        let thread_opt = store
            .list_threads(FolderFilter::All)
            .ok()
            .and_then(|list| list.into_iter().find(|t| t.id == tid));
        let (title, is_grp) = match thread_opt {
            Some(t) => (
                if t.title.trim().is_empty() {
                    tid.clone()
                } else {
                    t.title
                },
                t.kind == ThreadKind::Group,
            ),
            None => {
                let is_grp = store
                    .get_thread_kind(&tid)
                    .ok()
                    .flatten()
                    .is_some_and(|k| k == ThreadKind::Group);
                (tid.clone(), is_grp)
            }
        };
        let msgs = store
            .list_unread_messages_for_summary(&tid, cfg.max_summary_messages)
            .map_err(|e| e.to_string())?;
        (title, is_grp, msgs)
    };

    let input_msgs: Vec<AiSummaryInputMsg> =
        unread_msgs.iter().map(AiSummaryInputMsg::from).collect();
    let tid_for_emit = tid.clone();
    ai_summarize_unread_with_relay(
        &cfg,
        &relay,
        fallback_key.as_deref(),
        &thread_title,
        is_group,
        &input_msgs,
        |delta| {
            let _ = app.emit(
                "core://ai-summary-delta",
                serde_json::json!({
                    "thread_id": tid_for_emit,
                    "delta": delta,
                }),
            );
        },
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ai_translate_text(
    state: State<'_, AppState>,
    text: String,
    target_lang: String,
) -> Result<String, String> {
    let cfg = state.ai_config.lock().await.clone();
    if !cfg.enabled {
        return Err(
            "Trợ lý AI đang tắt. Hãy bật Trợ lý AI trong Cài đặt trước khi sử dụng.".into(),
        );
    }
    let relay = state.relay_cfg.lock().await.clone();
    let fallback_key = translate_fallback_gemini_key(&state).await;
    ai_translate_text_with_relay(&cfg, &relay, fallback_key.as_deref(), &text, &target_lang)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn schedule_list(
    state: State<'_, AppState>,
    status_filter: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<ScheduledMessageDto>, String> {
    let capped = limit.unwrap_or(200).clamp(1, 1000);
    let store = state.store.lock().await;
    store
        .list_scheduled_messages(status_filter.as_deref(), capped)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn schedule_create(
    app: AppHandle,
    state: State<'_, AppState>,
    mut input: NewScheduledMessageInput,
) -> Result<ScheduledMessageDto, String> {
    if input.thread_id.trim().is_empty() {
        return Err("Mã cuộc trò chuyện không được để trống".into());
    }
    if input.text.trim().is_empty() && input.attachment_path.trim().is_empty() {
        return Err("Cần nhập nội dung tin nhắn hoặc chọn tệp đính kèm".into());
    }
    if !input.attachment_path.trim().is_empty() {
        let canonical = {
            let approved = state
                .approved_attachment_paths
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            validate_approved_attachment_path(&approved, Path::new(input.attachment_path.trim()))?
        };
        input.attachment_path = canonical.to_string_lossy().into_owned();
    }
    let created = {
        let store = state.store.lock().await;
        store
            .create_scheduled_message(&input)
            .map_err(|e| e.to_string())?
    };
    state.scheduler.notify_changed();
    let _ = app.emit("core://schedule-updated", &created);
    Ok(created)
}

#[tauri::command]
async fn schedule_update(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    mut input: UpdateScheduledMessageInput,
) -> Result<Option<ScheduledMessageDto>, String> {
    if let Some(ref att_raw) = input.attachment_path {
        let trimmed = att_raw.trim();
        if !trimmed.is_empty() {
            let unchanged = {
                let store = state.store.lock().await;
                store
                    .get_scheduled_message(&id)
                    .ok()
                    .flatten()
                    .is_some_and(|cur| cur.attachment_path.trim() == trimmed)
            };
            if !unchanged {
                let canonical = {
                    let approved = state
                        .approved_attachment_paths
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    validate_approved_attachment_path(&approved, Path::new(trimmed))?
                };
                input.attachment_path = Some(canonical.to_string_lossy().into_owned());
            }
        }
    }
    let updated = {
        let store = state.store.lock().await;
        store
            .update_scheduled_message(&id, &input)
            .map_err(|e| e.to_string())?
    };
    if let Some(ref dto) = updated {
        state.scheduler.notify_changed();
        let _ = app.emit("core://schedule-updated", dto);
    }
    Ok(updated)
}

#[tauri::command]
async fn schedule_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let (cancelled, updated) = {
        let store = state.store.lock().await;
        let ok = store
            .cancel_scheduled_message(&id)
            .map_err(|e| e.to_string())?;
        let dto = if ok {
            store.get_scheduled_message(&id).ok().flatten()
        } else {
            None
        };
        (ok, dto)
    };
    if cancelled {
        state.scheduler.notify_changed();
        if let Some(ref dto) = updated {
            let _ = app.emit("core://schedule-updated", dto);
        } else {
            let _ = app.emit("core://schedule-updated", serde_json::Value::Null);
        }
    }
    Ok(cancelled)
}

#[tauri::command]
async fn schedule_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let deleted = {
        let store = state.store.lock().await;
        store
            .delete_scheduled_message(&id)
            .map_err(|e| e.to_string())?
    };
    if deleted {
        state.scheduler.notify_changed();
        let _ = app.emit("core://schedule-updated", serde_json::Value::Null);
    }
    Ok(deleted)
}

#[tauri::command]
async fn schedule_send_now(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<ScheduledMessageDto>, String> {
    let updated = {
        let store = state.store.lock().await;
        store
            .reschedule_now(&id, chrono_now_ms())
            .map_err(|e| e.to_string())?
    };
    if let Some(ref dto) = updated {
        state.scheduler.notify_changed();
        let _ = app.emit("core://schedule-updated", dto);
    }
    Ok(updated)
}

#[tauri::command]
async fn local_api_get_config(
    state: State<'_, AppState>,
) -> Result<local_api::LocalApiConfig, String> {
    Ok(state.local_api.config().await)
}

#[tauri::command]
async fn local_api_get_status(
    state: State<'_, AppState>,
) -> Result<local_api::LocalApiStatusDto, String> {
    Ok(state.local_api.status(&state.store).await)
}

#[tauri::command]
async fn local_api_update_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: local_api::LocalApiConfig,
) -> Result<local_api::LocalApiStatusDto, String> {
    state
        .local_api
        .apply_config(
            config,
            app,
            state.session.clone(),
            state.store.clone(),
            state.scheduler.clone(),
        )
        .await
}

#[tauri::command]
async fn local_api_kill_switch(
    state: State<'_, AppState>,
) -> Result<local_api::LocalApiStatusDto, String> {
    state.local_api.kill_switch(&state.store).await
}

#[tauri::command]
async fn local_api_list_clients(state: State<'_, AppState>) -> Result<Vec<ApiClientDto>, String> {
    let store = state.store.lock().await;
    store.list_api_clients().map_err(|e| e.to_string())
}

#[tauri::command]
async fn local_api_create_client(
    state: State<'_, AppState>,
    name: String,
    scopes: Vec<String>,
    allowed_thread_ids: Vec<String>,
    rate_limit_per_min: u32,
) -> Result<CreatedApiClientDto, String> {
    let store = state.store.lock().await;
    store
        .create_api_client(&name, &scopes, &allowed_thread_ids, rate_limit_per_min)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn local_api_update_client(
    state: State<'_, AppState>,
    id: String,
    enabled: Option<bool>,
    scopes: Option<Vec<String>>,
    allowed_thread_ids: Option<Vec<String>>,
    rate_limit_per_min: Option<u32>,
) -> Result<Option<ApiClientDto>, String> {
    let store = state.store.lock().await;
    store
        .update_api_client(
            &id,
            enabled,
            scopes.as_deref(),
            allowed_thread_ids.as_deref(),
            rate_limit_per_min,
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn local_api_delete_client(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let store = state.store.lock().await;
    store.delete_api_client(&id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn local_api_list_audit_logs(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<ApiAuditLogDto>, String> {
    let capped = limit.unwrap_or(100).clamp(1, 500);
    let store = state.store.lock().await;
    store
        .list_api_audit_logs(capped)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn local_api_clear_audit_logs(state: State<'_, AppState>) -> Result<usize, String> {
    let store = state.store.lock().await;
    store.clear_api_audit_logs().map_err(|e| e.to_string())
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn init_session_with_relay_fallback(
    session_path: &Path,
    relay_cfg: RelayConfig,
) -> (Session, RelayConfig) {
    match Session::load_from_file_with_relay(session_path, relay_cfg.clone()) {
        Ok(sess) => (sess, relay_cfg),
        Err(load_err) => match Session::new_with_relay(relay_cfg.clone()) {
            Ok(sess) => (sess, relay_cfg),
            Err(init_err) => {
                eprintln!(
                    "[teza][startup] Failed to initialize session with saved relay config (load={load_err}; init={init_err}); falling back to default direct relay config"
                );
                let default_relay = RelayConfig::default();
                let sess = Session::load_from_file_with_relay(session_path, default_relay.clone())
                    .or_else(|_| Session::new_with_relay(default_relay.clone()))
                    .expect("Failed to initialize session with default relay config");
                (sess, default_relay)
            }
        },
    }
}

fn append_os_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn quarantine_corrupt_store_files(db_path: &Path, ts: i64) {
    let candidates = [
        db_path.to_path_buf(),
        append_os_suffix(db_path, ".key"),
        append_os_suffix(db_path, "-wal"),
        append_os_suffix(db_path, "-shm"),
        append_os_suffix(db_path, "-journal"),
    ];
    let suffix = format!(".corrupt.{ts}");
    for src in candidates {
        if src.exists() {
            let dst = append_os_suffix(&src, &suffix);
            if let Err(rename_err) = std::fs::rename(&src, &dst) {
                eprintln!(
                    "[teza][startup] Failed to quarantine {} -> {}: {rename_err}; removing broken file",
                    src.display(),
                    dst.display()
                );
                let _ = std::fs::remove_file(&src);
            } else {
                eprintln!(
                    "[teza][startup] Quarantined corrupt history file {} -> {}",
                    src.display(),
                    dst.display()
                );
            }
        }
    }
}

fn open_message_store_with_recovery(db_path: &Path) -> MessageStore {
    match MessageStore::open(db_path) {
        Ok(store) => store,
        Err(first_err) => {
            eprintln!(
                "[teza][startup] Failed to open history database {} ({first_err}); quarantining broken DB/key files and opening a fresh store",
                db_path.display()
            );
            let ts = chrono_now_ms().max(1);
            quarantine_corrupt_store_files(db_path, ts);
            MessageStore::open(db_path).unwrap_or_else(|second_err| {
                eprintln!(
                    "[teza][startup] Fresh history database {} also failed ({second_err}); falling back to temporary store",
                    db_path.display()
                );
                let fallback_path =
                    std::env::temp_dir().join(format!("teza_recovery_history_{ts}.db"));
                MessageStore::open(&fallback_path)
                    .expect("Failed to open fallback SQLite history database")
            })
        }
    }
}

pub fn run() {
    let paths = StoragePaths::resolve_default(None).expect("Failed to resolve storage directory");
    let loaded_relay_cfg = RelayConfig::load_from_file(&paths.relay_file());
    let session_path = paths.session_file();
    let (session, relay_cfg) = init_session_with_relay_fallback(&session_path, loaded_relay_cfg);
    let uid = if session.profile.uid.is_empty() {
        None
    } else {
        Some(session.profile.uid.as_str())
    };
    let db_path = paths.history_db(uid);
    let store = open_message_store_with_recovery(&db_path);
    let gdrive_last_sync_ts = store.get_gdrive_last_sync_ts().ok().flatten();
    let gdrive_last_history_sync_ts = store.get_gdrive_last_history_sync_ts().ok().flatten();
    let media_cfg = MediaVaultConfig::load_from_file(&paths.media_config_file());
    let media_vault = open_media_vault_with_fallback(&paths, uid, media_cfg);
    let media_protocol_token =
        generate_protocol_token().expect("Failed to generate CSPRNG media protocol token");
    let gdrive = Arc::new(GDriveRuntime::new(
        paths.clone(),
        uid,
        gdrive_last_sync_ts,
        gdrive_last_history_sync_ts,
    ));

    let translate_cfg = TranslateConfig::load_from_file(&paths.translate_file());
    let ai_cfg = AiConfig::load_from_file(&ai_config_file(&paths.root));
    let scheduler = scheduler::SchedulerHandle::new();
    let local_api = Arc::new(local_api::LocalApiManager::new(&paths));
    let sess_arc = Arc::new(Mutex::new(session));
    let store_arc = Arc::new(Mutex::new(store));
    let media_vault_arc = Arc::new(RwLock::new(Arc::new(media_vault)));
    let relay_cfg_arc = Arc::new(Mutex::new(relay_cfg));
    let ws_task_arc = Arc::new(Mutex::new(None));
    let qr_task_arc = Arc::new(Mutex::new(None));

    let close_to_tray = Arc::new(AtomicBool::new(true));
    let allow_exit = Arc::new(AtomicBool::new(false));
    let call_mgr = Arc::new(call::CallManager::new(paths.clone()));
    let webapp_accounts =
        webapp_accounts::WebAppAccountStore::load(paths.root.join("webapp_accounts.json"));
    let gdrive_for_setup = gdrive.clone();
    let relay_for_setup = relay_cfg_arc.clone();
    let store_for_setup = store_arc.clone();
    let media_vault_for_setup = media_vault_arc.clone();
    let session_for_setup = sess_arc.clone();
    let store_for_automation = store_arc.clone();
    let scheduler_for_setup = scheduler.clone();
    let local_api_for_setup = local_api.clone();
    let state = AppState {
        paths,
        session: sess_arc,
        store: store_arc,
        media_vault: media_vault_arc,
        media_protocol_token,
        gdrive,
        ws_task: ws_task_arc,
        qr_task: qr_task_arc,
        active_thread_id: Arc::new(Mutex::new(None)),
        translate_cfg: Arc::new(Mutex::new(translate_cfg)),
        relay_cfg: relay_cfg_arc,
        pending_update: Arc::new(Mutex::new(None)),
        close_to_tray: close_to_tray.clone(),
        allow_exit: allow_exit.clone(),
        notify_popup: notify_popup::NotifyPopupController::new(),
        webapp_op_lock: Arc::new(Mutex::new(())),
        webapp_epoch: Arc::new(AtomicU64::new(0)),
        webapp_base_urls: Arc::new(Mutex::new(HashMap::new())),
        webapp_last_popup_ms: Arc::new(AtomicU64::new(0)),
        webapp_accounts: Arc::new(Mutex::new(webapp_accounts)),
        recent_saved_files: Arc::new(Mutex::new(VecDeque::new())),
        approved_attachment_paths: Arc::new(std::sync::Mutex::new(VecDeque::new())),
        staged_raw_uploads: Arc::new(std::sync::Mutex::new(HashMap::new())),
        call_mgr,
        ai_config: Arc::new(Mutex::new(ai_cfg)),
        scheduler,
        local_api,
    };

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }));

    builder
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .register_asynchronous_uri_scheme_protocol("teza-media", handle_teza_media_protocol)
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            auth_get_status,
            auth_start_qr,
            auth_logout,
            threads_sync,
            threads_list,
            thread_ensure_direct,
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
            friend_remove,
            friend_send_request,
            friend_accept_request,
            friend_reject_request,
            friend_undo_request,
            friend_list_requests,
            friend_block_user,
            friend_unblock_user,
            group_add_members,
            group_leave,
            group_kick_members,
            reminder_create,
            poll_create,
            poll_get_detail,
            poll_vote,
            poll_add_options,
            poll_lock,
            messages_load,
            messages_load_around,
            messages_list_pinned,
            message_get_by_id,
            message_send_text,
            message_send_quote,
            message_forward,
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
            message_send_base64_attachment,
            raw_attachment_stage_begin,
            raw_attachment_stage_chunk,
            raw_attachment_stage_send,
            raw_attachment_stage_abort,
            search_global,
            translate_get_config,
            translate_save_config,
            translate_messages,
            translate_clear_cache,
            relay_get_config,
            relay_save_config,
            relay_test_connection,
            call_get_state,
            call_list_devices,
            call_set_devices,
            call_test_audio,
            call_start_voice,
            call_start_video,
            call_answer,
            call_decline,
            call_hangup,
            call_set_mute,
            call_set_camera,
            call_push_video_frame,
            call_request_video_keyframe,
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
            clipboard_copy_rgba_image_b64,
            capture_screen,
            webapp_open,
            webapp_set_bounds,
            webapp_hide_all,
            webapp_close,
            webapp_navigate,
            webapp_accounts_list,
            webapp_accounts_save,
            webapp_accounts_delete,
            webapp_account_fill,
            open_external_url,
            media_get_protocol_token,
            media_get_config,
            media_save_config,
            media_get_stats,
            media_clear_cache,
            media_pick_vault_dir,
            media_set_pin,
            media_open_vault_dir,
            gdrive_get_config,
            gdrive_save_config,
            gdrive_get_status,
            gdrive_connect_start,
            gdrive_connect_cancel,
            gdrive_disconnect,
            gdrive_unlock_vault,
            gdrive_sync_now,
            gdrive_get_storage_overview,
            gdrive_delete_cloud_items,
            gdrive_delete_cloud_by_thread,
            gdrive_prune_cloud_older_than,
            ai_get_config,
            ai_update_config,
            ai_probe_cli,
            ai_list_messages,
            ai_clear_messages,
            ai_chat_send,
            ai_summarize_unread,
            ai_translate_text,
            schedule_list,
            schedule_create,
            schedule_update,
            schedule_cancel,
            schedule_delete,
            schedule_send_now,
            local_api_get_config,
            local_api_get_status,
            local_api_update_config,
            local_api_kill_switch,
            local_api_list_clients,
            local_api_create_client,
            local_api_update_client,
            local_api_delete_client,
            local_api_list_audit_logs,
            local_api_clear_audit_logs,
        ])
        .setup(move |app| {
            let _ = app.get_window("main");
            spawn_gdrive_background_sync_loop(
                app.handle().clone(),
                gdrive_for_setup,
                relay_for_setup,
                store_for_setup,
                media_vault_for_setup,
            );
            {
                let app_sched = app.handle().clone();
                let sess_sched = session_for_setup.clone();
                let store_sched = store_for_automation.clone();
                let notify_sched = scheduler_for_setup.notify_arc();
                tauri::async_runtime::spawn(async move {
                    std::mem::drop(scheduler::spawn_scheduler(
                        app_sched,
                        sess_sched,
                        store_sched,
                        notify_sched,
                    ));
                });
            }
            {
                let app_local_api = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = local_api_for_setup
                        .start_if_enabled(
                            app_local_api,
                            session_for_setup,
                            store_for_automation,
                            scheduler_for_setup,
                        )
                        .await
                    {
                        eprintln!("[teza][local-api] failed to start on boot: {err}");
                    }
                });
            }
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
                        if let Some(w) = app_handle.get_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    } else if event.id() == "quit" {
                        exit_flag.store(true, Ordering::SeqCst);
                        let quit_app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Some(st) = quit_app.try_state::<AppState>() {
                                notify_popup::force_hide_or_destroy(&quit_app, &st.notify_popup)
                                    .await;
                                let _ = st.local_api.stop().await;
                                st.gdrive.shutdown().await;
                                let vault = st.media_vault.read().await.clone();
                                vault.flush_access_touches(&st.store).await;
                                st.call_mgr.teardown_for_session(None, &st.session).await;
                            }
                            quit_app.exit(0);
                        });
                    }
                })
                .on_tray_icon_event(|tray_icon, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray_icon.app_handle().get_window("main") {
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
                match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        let st = window.state::<AppState>();
                        if st.close_to_tray.load(Ordering::SeqCst)
                            && !st.allow_exit.load(Ordering::SeqCst)
                        {
                            api.prevent_close();
                            hide_all_webapp_children(window.app_handle());
                            let _ = window.hide();
                        } else {
                            notify_popup::force_destroy_sync(window.app_handle(), &st.notify_popup);
                        }
                    }
                    WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) => {
                        let st = window.state::<AppState>();
                        let mut approved = st
                            .approved_attachment_paths
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        for p in paths {
                            record_approved_attachment_path(&mut approved, p);
                        }
                    }
                    _ => {}
                }
            }
        })
        .on_webview_event(|webview, event| {
            if webview.label() == "main" {
                if let tauri::WebviewEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) =
                    event
                {
                    let st = webview.state::<AppState>();
                    let mut approved = st
                        .approved_attachment_paths
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    for p in paths {
                        record_approved_attachment_path(&mut approved, p);
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
    fn validate_web_url_rejects_non_http_schemes_and_defaults_lan_to_http() {
        assert!(super::validate_web_url("https://calendar.google.com").is_ok());
        assert_eq!(
            super::validate_web_url("notion.so").unwrap().as_str(),
            "https://notion.so/"
        );
        assert_eq!(
            super::validate_web_url("http://10.7.12.233/index.html")
                .unwrap()
                .as_str(),
            "http://10.7.12.233/index.html"
        );
        assert_eq!(
            super::validate_web_url("http://10.7.12.233:5093/summary_error.html")
                .unwrap()
                .as_str(),
            "http://10.7.12.233:5093/summary_error.html"
        );
        assert_eq!(
            super::validate_web_url("10.7.12.233/index.html")
                .unwrap()
                .as_str(),
            "http://10.7.12.233/index.html"
        );
        assert_eq!(
            super::validate_web_url("10.7.12.233:5093/summary_error.html")
                .unwrap()
                .as_str(),
            "http://10.7.12.233:5093/summary_error.html"
        );
        assert_eq!(
            super::validate_web_url("https://10.7.12.233/secure")
                .unwrap()
                .as_str(),
            "https://10.7.12.233/secure"
        );
        assert_eq!(
            super::validate_web_url("localhost:3000/dashboard")
                .unwrap()
                .as_str(),
            "http://localhost:3000/dashboard"
        );
        assert_eq!(
            super::validate_web_url("192.168.1.20:8080")
                .unwrap()
                .as_str(),
            "http://192.168.1.20:8080/"
        );
        assert_eq!(
            super::validate_web_url("172.16.5.10/status")
                .unwrap()
                .as_str(),
            "http://172.16.5.10/status"
        );
        assert_eq!(
            super::validate_web_url("100.95.41.126:9000")
                .unwrap()
                .as_str(),
            "http://100.95.41.126:9000/"
        );
        assert_eq!(
            super::validate_web_url("nas.local/files").unwrap().as_str(),
            "http://nas.local/files"
        );
        assert_eq!(
            super::validate_web_url("[::1]:8080/api").unwrap().as_str(),
            "http://[::1]:8080/api"
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

    #[test]
    fn webapp_label_is_collision_free_and_deterministic() {
        let a = super::webapp_label("erp/1");
        let b = super::webapp_label("erp.1");
        let c = super::webapp_label("erp-1");
        let d = super::webapp_label("erp/1");
        assert_eq!(a, d);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert!(a.starts_with(super::WEBAPP_LABEL_PREFIX));
        assert!(b.starts_with(super::WEBAPP_LABEL_PREFIX));
        assert!(a
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));

        let empty = super::webapp_label("   ");
        let dashes = super::webapp_label("---");
        assert_ne!(empty, dashes);
        assert!(empty.starts_with(super::WEBAPP_LABEL_PREFIX));
    }

    #[test]
    fn popup_rate_limiter_enforces_cooldown() {
        use std::sync::atomic::AtomicU64;

        let last = AtomicU64::new(0);
        assert!(super::try_acquire_popup_slot(&last, 10_000, 1500));
        assert!(!super::try_acquire_popup_slot(&last, 10_500, 1500));
        assert!(!super::try_acquire_popup_slot(&last, 11_499, 1500));
        assert!(super::try_acquire_popup_slot(&last, 11_500, 1500));
        assert!(!super::try_acquire_popup_slot(&last, 12_000, 1500));
    }

    #[test]
    fn startup_recovery_handles_invalid_relay_and_corrupt_history_db() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::relay::RelayConfig;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_startup_recovery_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. Invalid saved relay config must not panic and must fall back to default relay
        let broken_relay = RelayConfig {
            enabled: true,
            host: "invalid host with spaces".into(),
            port: 0,
            ..RelayConfig::default()
        };
        let session_path = dir.join("session.json");
        let (sess, effective_relay) =
            super::init_session_with_relay_fallback(&session_path, broken_relay);
        assert!(!effective_relay.enabled);
        assert!(!sess.relay.enabled);

        // 2. Corrupt SQLite DB + corrupt .key must be quarantined and reopened cleanly
        let db_path = dir.join("history.db");
        let key_path = super::append_os_suffix(&db_path, ".key");
        std::fs::write(&db_path, b"not-a-sqlite-database").unwrap();
        std::fs::write(&key_path, b"dpapi:v1:corrupted-base64!!!").unwrap();

        let store = super::open_message_store_with_recovery(&db_path);
        assert!(store
            .list_threads(zalo_core::chat::FolderFilter::All)
            .unwrap()
            .is_empty());

        let entries: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().any(|n| n.starts_with("history.db.corrupt.")),
            "expected quarantined db file in {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|n| n.starts_with("history.db.key.corrupt.")),
            "expected quarantined key file in {entries:?}"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn mark_of_the_web_ads_path_appends_zone_identifier() {
        use std::path::Path;

        let p = Path::new(r"C:\Users\Demo\Downloads\report.pdf");
        let ads = super::mark_of_the_web_ads_path(p);
        assert_eq!(
            ads.to_string_lossy(),
            r"C:\Users\Demo\Downloads\report.pdf:Zone.Identifier"
        );
    }

    #[test]
    fn fetch_image_data_url_passthrough_rejects_svg_non_media_and_oversized_payloads() {
        // Non-data URLs return Ok(None) so caller proceeds to remote HTTP fetch
        assert_eq!(
            super::validate_media_data_url_passthrough("https://photo.zdn.vn/a.jpg").unwrap(),
            None
        );

        // Valid raster image, audio, and video data URLs pass through
        for ok in [
            "data:image/png;base64,iVBORw0KGgo=",
            "  DATA:IMAGE/JPEG;base64,/9j/4AAQ  ",
            "data:audio/mpeg;base64,SUQz",
            "data:video/mp4;base64,AAAA",
        ] {
            let out = super::validate_media_data_url_passthrough(ok).unwrap();
            assert_eq!(out.as_deref(), Some(ok.trim()));
        }

        // SVG (case-insensitive) and non-media data URLs must be rejected
        for blocked in [
            "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=",
            "DATA:IMAGE/SVG+XML,<svg onload=alert(1)>",
            "data:image/svg+xml;charset=utf-8,%3Csvg%3E",
            "data:image/svg ,foo",
            "data:text/html,<script>alert(1)</script>",
            "data:application/javascript,alert(1)",
            "data:image/,empty-subtype",
            "data:image/png;base64",
            "data:image/png\n;base64,AAAA",
        ] {
            assert!(
                super::validate_media_data_url_passthrough(blocked).is_err(),
                "expected blocked data URL: {blocked}"
            );
        }

        // Oversized data URL (> 15 MiB) must be rejected
        let oversized = format!(
            "data:image/png;base64,{}",
            "A".repeat(super::MAX_PASSTHROUGH_DATA_URL_BYTES)
        );
        assert!(super::validate_media_data_url_passthrough(&oversized).is_err());
    }

    #[test]
    fn search_global_limit_translate_batch_and_webapp_bounds_are_enforced() {
        assert_eq!(super::clamp_search_global_limit(None), 30);
        assert_eq!(super::clamp_search_global_limit(Some(0)), 1);
        assert_eq!(super::clamp_search_global_limit(Some(1)), 1);
        assert_eq!(super::clamp_search_global_limit(Some(45)), 45);
        assert_eq!(super::clamp_search_global_limit(Some(200)), 200);
        assert_eq!(super::clamp_search_global_limit(Some(99_999)), 200);

        assert!(super::validate_translate_batch_len(0).is_ok());
        assert!(super::validate_translate_batch_len(100).is_ok());
        assert!(super::validate_translate_batch_len(101).is_err());

        assert_eq!(
            super::validate_webapp_bounds(10.0, 20.0, 800.0, 600.0).unwrap(),
            (800.0, 600.0)
        );
        assert_eq!(
            super::validate_webapp_bounds(0.0, 0.0, 10.0, -5.0).unwrap(),
            (120.0, 120.0)
        );
        assert!(super::validate_webapp_bounds(f64::NAN, 0.0, 400.0, 300.0).is_err());
        assert!(super::validate_webapp_bounds(0.0, f64::INFINITY, 400.0, 300.0).is_err());
        assert!(super::validate_webapp_bounds(0.0, 0.0, f64::NEG_INFINITY, 300.0).is_err());
        assert!(super::validate_webapp_bounds(0.0, 0.0, 400.0, f64::NAN).is_err());
    }

    #[test]
    fn approved_attachment_paths_allowlist_validates_dedups_and_enforces_cap() {
        use std::collections::VecDeque;
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_approved_attach_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();

        let approved_file = dir.join("picked.png");
        std::fs::write(&approved_file, b"png").unwrap();
        let unapproved_file = dir.join("secret.txt");
        std::fs::write(&unapproved_file, b"secret").unwrap();

        let mut list = VecDeque::new();
        assert!(super::validate_approved_attachment_path(&list, &approved_file).is_err());
        assert!(super::validate_approved_attachment_path(&list, &unapproved_file).is_err());

        super::record_approved_attachment_path(&mut list, &approved_file);
        super::record_approved_attachment_path(&mut list, &approved_file);
        assert_eq!(list.len(), 1);
        assert!(super::validate_approved_attachment_path(&list, &approved_file).is_ok());
        assert!(super::validate_approved_attachment_path(&list, &unapproved_file).is_err());

        for idx in 0..(super::MAX_APPROVED_ATTACHMENT_PATHS + 5) {
            super::record_approved_attachment_path(&mut list, &dir.join(format!("f_{idx}.bin")));
        }
        assert_eq!(list.len(), super::MAX_APPROVED_ATTACHMENT_PATHS);
        assert!(super::validate_approved_attachment_path(&list, &approved_file).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_ws_message_unread_respects_window_focus_and_direction() {
        // Outgoing messages (including self-echo from phone) always clear unread regardless of focus.
        assert_eq!(
            super::classify_ws_message_unread(true, None, "peer-1", false),
            (false, true)
        );
        assert_eq!(
            super::classify_ws_message_unread(true, Some("peer-1"), "peer-1", false),
            (false, true)
        );
        assert_eq!(
            super::classify_ws_message_unread(true, Some("peer-1"), "peer-1", true),
            (false, true)
        );

        // Incoming message on active thread is only marked read when main window is visible & focused.
        assert_eq!(
            super::classify_ws_message_unread(false, Some("peer-1"), "peer-1", true),
            (false, true)
        );
        // Unfocused / minimized / hidden active thread must honestly increment SQLite unread.
        assert_eq!(
            super::classify_ws_message_unread(false, Some("peer-1"), "peer-1", false),
            (true, false)
        );
        // Incoming message on another thread or when no thread is active increments unread.
        assert_eq!(
            super::classify_ws_message_unread(false, Some("peer-2"), "peer-1", true),
            (true, false)
        );
        assert_eq!(
            super::classify_ws_message_unread(false, None, "peer-1", true),
            (true, false)
        );
    }

    #[test]
    fn thread_read_synced_payload_matches_frontend_contract() {
        assert_eq!(
            super::build_thread_read_synced_payload("12345", false),
            serde_json::json!({ "thread_id": "12345", "is_unread": false })
        );
        assert_eq!(
            super::build_thread_read_synced_payload("grp-9", true),
            serde_json::json!({ "thread_id": "grp-9", "is_unread": true })
        );
    }

    #[test]
    fn pick_latest_incoming_for_seen_selects_newest_server_incoming_with_msg_type() {
        let mk = |msg_id: &str, sender_id: &str, msg_type: &str, ts: i64, is_outgoing: bool| {
            zalo_core::chat::MessageDto {
                msg_id: msg_id.into(),
                cli_msg_id: None,
                thread_id: "peer-1".into(),
                sender_id: sender_id.into(),
                sender_name: sender_id.into(),
                text: String::new(),
                msg_type: msg_type.into(),
                ts,
                is_outgoing,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            }
        };

        assert_eq!(super::pick_latest_incoming_for_seen(&[]), None);
        assert_eq!(
            super::pick_latest_incoming_for_seen(&[mk("100", "me", "chat.text", 1_000, true)]),
            None
        );

        let msgs = vec![
            mk("101", "u1", "chat.text", 1_000, false),
            mk("local-999", "u1", "chat.text", 9_000, false),
            mk("202", "u2", "chat.photo", 3_000, false),
            mk("203", "u3", "share.file", 3_000, false),
            mk("300", "me", "chat.text", 5_000, true),
        ];
        assert_eq!(
            super::pick_latest_incoming_for_seen(&msgs),
            Some(("203".into(), "u3".into(), "share.file".into()))
        );
    }

    #[test]
    fn should_dispatch_ws_event_accepts_live_and_pulled_reaction_cmds() {
        let mk_ev = |cmd: u16| zalo_core::socket::WsEvent {
            cmd,
            sub_cmd: 0,
            version: 1,
            payload: serde_json::json!({}),
        };

        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::chat::parser::CMD_REACTION_PUSH
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::chat::parser::CMD_REACTION_PULL_1_1
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::chat::parser::CMD_REACTION_PULL_GROUP
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PUSH_MSG_1_1
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_CLEAR_UNREAD_1_1
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_CLEAR_UNREAD_GROUP
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PULL_ACK_GROUP
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_E2EE_DELIVER_SEEN_1_1
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_E2EE_DELIVER_SEEN_GROUP
        )));
        assert!(!super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PING
        )));
    }

    #[test]
    fn read_sync_watermark_suppresses_late_offline_msgs_without_hiding_newer_ones() {
        let mut map = std::collections::HashMap::new();
        super::record_read_sync_watermark(&mut map, "u1", false, Some("7451000"));
        assert!(super::is_msg_cleared_by_read_sync(&map, "u1", "7450999"));
        assert!(super::is_msg_cleared_by_read_sync(&map, "u1", "7451000"));
        assert!(!super::is_msg_cleared_by_read_sync(&map, "u1", "7451001"));
        assert!(!super::is_msg_cleared_by_read_sync(&map, "u2", "7451000"));

        // Older clear watermark must not regress a newer watermark
        super::record_read_sync_watermark(&mut map, "u1", false, Some("7450500"));
        assert!(super::is_msg_cleared_by_read_sync(&map, "u1", "7451000"));

        // Explicit mark-unread clears watermark
        super::record_read_sync_watermark(&mut map, "u1", true, None);
        assert!(!super::is_msg_cleared_by_read_sync(&map, "u1", "7451000"));
    }

    #[test]
    fn message_sync_sig_detects_reaction_emoji_count_and_me_changes() {
        let base = zalo_core::chat::MessageDto {
            msg_id: "m1".into(),
            cli_msg_id: Some("c1".into()),
            thread_id: "t1".into(),
            sender_id: "u1".into(),
            sender_name: "Alice".into(),
            text: "hi".into(),
            msg_type: "chat.text".into(),
            ts: 1_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: vec![zalo_core::chat::ReactionSummary {
                emoji: "👍".into(),
                count: 1,
                me: false,
            }],
            attachment_url: None,
            attachment_name: None,
        };
        let mut switched = base.clone();
        switched.reactions[0].emoji = "❤️".into();
        assert_ne!(
            super::message_sync_sig(&base),
            super::message_sync_sig(&switched),
            "switching reaction emoji with same len must change sync sig"
        );

        let mut self_flagged = base.clone();
        self_flagged.reactions[0].me = true;
        assert_ne!(
            super::message_sync_sig(&base),
            super::message_sync_sig(&self_flagged),
            "changing me flag with same len must change sync sig"
        );
    }

    #[test]
    fn teza_media_query_and_remote_url_validation_enforce_security_boundaries() {
        let uri: tauri::http::Uri =
            "http://teza-media.localhost/resolve?url=https%3A%2F%2Fphoto.zdn.vn%2Fa%2Bb%20c.jpg&kind=video&t=deadbeef&tid=t1&mid=m2"
                .parse()
                .unwrap();
        let parsed = super::parse_teza_media_query(&uri).unwrap();
        assert_eq!(parsed.url, "https://photo.zdn.vn/a+b c.jpg");
        assert_eq!(parsed.kind, "video");
        assert_eq!(parsed.token, "deadbeef");
        assert_eq!(parsed.tid.as_deref(), Some("t1"));
        assert_eq!(parsed.mid.as_deref(), Some("m2"));

        // Default kind is "image" when omitted or unknown
        let uri_default: tauri::http::Uri =
            "teza-media://localhost/resolve?url=https%3A%2F%2Fphoto.zdn.vn%2Fx.png&kind=weird&t=tok"
                .parse()
                .unwrap();
        let parsed_def = super::parse_teza_media_query(&uri_default).unwrap();
        assert_eq!(parsed_def.kind, "image");
        assert_eq!(parsed_def.tid, None);
        assert_eq!(parsed_def.mid, None);

        // Wrong path must return 404
        let bad_path: tauri::http::Uri =
            "http://teza-media.localhost/etc/passwd?t=tok".parse().unwrap();
        assert_eq!(super::parse_teza_media_query(&bad_path).unwrap_err().0, 404);

        // Valid remote URLs pass
        assert!(super::validate_teza_media_remote_url("https://photo.zdn.vn/pic.jpg").is_ok());
        assert!(super::validate_teza_media_remote_url("http://cdn.zdn.vn/v.mp4").is_ok());

        // Local filesystem paths, non-http schemes, internal hosts, and credentials must fail
        for bad in [
            "",
            "   ",
            r"C:\Windows\win.ini",
            r"\\server\share\secret.txt",
            "/etc/passwd",
            "file:///C:/Windows/win.ini",
            "data:image/png;base64,AAAA",
            "javascript:alert(1)",
            "teza-media://localhost/resolve?url=https://a.b",
            "http://teza-media.localhost/resolve?url=https://a.b",
            "http://tauri.localhost/index.html",
            "http://ipc.localhost/cmd",
            "https://user:pass@photo.zdn.vn/a.jpg",
            "https://photo.zdn.vn/a.jpg\r\nX-Inject: 1",
        ] {
            assert!(
                super::validate_teza_media_remote_url(bad).is_err(),
                "expected validate_teza_media_remote_url to reject: {bad:?}"
            );
        }
    }

    #[test]
    fn http_byte_range_parser_handles_video_streaming_windows_and_edge_cases() {
        use super::{
            parse_http_byte_range, ParsedByteRange, MEDIA_PROTOCOL_MAX_EXPLICIT_RANGE_BYTES,
            MEDIA_PROTOCOL_RANGE_WINDOW_BYTES,
        };

        // Open-ended range on a 50 MiB video caps to 2 MiB slice for O(1) RAM streaming
        let total = 50 * 1024 * 1024;
        assert_eq!(
            parse_http_byte_range("bytes=0-", total),
            ParsedByteRange::Valid {
                start: 0,
                end_inclusive: MEDIA_PROTOCOL_RANGE_WINDOW_BYTES - 1,
            }
        );
        // Second window near EOF clamps to total - 1
        assert_eq!(
            parse_http_byte_range(&format!("bytes={}-", total - 100), total),
            ParsedByteRange::Valid {
                start: total - 100,
                end_inclusive: total - 1,
            }
        );
        // Closed range within limit
        assert_eq!(
            parse_http_byte_range("bytes=1024-4095", total),
            ParsedByteRange::Valid {
                start: 1024,
                end_inclusive: 4095,
            }
        );
        // Oversized explicit closed range is capped to max window
        assert_eq!(
            parse_http_byte_range("bytes=0-40000000", total),
            ParsedByteRange::Valid {
                start: 0,
                end_inclusive: MEDIA_PROTOCOL_MAX_EXPLICIT_RANGE_BYTES - 1,
            }
        );
        // Suffix range (e.g. MP4 moov atom at tail)
        assert_eq!(
            parse_http_byte_range("bytes=-4096", total),
            ParsedByteRange::Valid {
                start: total - 4096,
                end_inclusive: total - 1,
            }
        );
        // Unsatisfiable ranges
        assert_eq!(
            parse_http_byte_range(&format!("bytes={total}-"), total),
            ParsedByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_http_byte_range("bytes=-0", total),
            ParsedByteRange::Unsatisfiable
        );
        assert_eq!(
            parse_http_byte_range("bytes=0-0", 0),
            ParsedByteRange::Unsatisfiable
        );
        // Ignored / malformed / multi-range
        assert_eq!(
            parse_http_byte_range("items=0-5", total),
            ParsedByteRange::Ignore
        );
        assert_eq!(
            parse_http_byte_range("bytes=0-10, 20-30", total),
            ParsedByteRange::Ignore
        );
        assert_eq!(
            parse_http_byte_range("bytes=200-100", total),
            ParsedByteRange::Ignore
        );
    }

    #[test]
    fn media_protocol_responses_set_security_and_range_headers() {
        let mut hdrs = tauri::http::HeaderMap::new();
        hdrs.insert(
            "origin",
            tauri::http::HeaderValue::from_static("http://tauri.localhost"),
        );
        assert_eq!(
            super::allowed_media_cors_origin(&hdrs).as_deref(),
            Some("http://tauri.localhost")
        );
        hdrs.insert(
            "origin",
            tauri::http::HeaderValue::from_static("https://evil.example"),
        );
        assert_eq!(super::allowed_media_cors_origin(&hdrs), None);

        let cd = super::file_dialog::build_inline_content_disposition("clip.mp4");
        let resp = super::build_media_success_response(
            206,
            "video/mp4",
            1024,
            Some("bytes 0-1023/4096".into()),
            Some(cd.as_str()),
            Some("http://tauri.localhost"),
            vec![0u8; 1024],
        );
        assert_eq!(resp.status().as_u16(), 206);
        assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
        assert_eq!(
            resp.headers().get("content-range").unwrap(),
            "bytes 0-1023/4096"
        );
        assert_eq!(
            resp.headers().get("content-disposition").unwrap(),
            cd.as_str()
        );
        assert_eq!(resp.headers().get("content-length").unwrap(), "1024");
        assert_eq!(resp.headers().get("content-type").unwrap(), "video/mp4");
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            resp.headers().get("cache-control").unwrap(),
            "private, max-age=86400"
        );
        assert_eq!(
            resp.headers().get("access-control-allow-origin").unwrap(),
            "http://tauri.localhost"
        );

        let err_resp = super::build_media_error_response(415, "SVG blocked", None, None);
        assert_eq!(err_resp.status().as_u16(), 415);
        assert_eq!(
            err_resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert!(err_resp
            .headers()
            .get("access-control-allow-origin")
            .is_none());

        // Non-Range HEAD always returns 200 OK with full Content-Length (0 RAM body),
        // even for >8 MiB videos/files; non-Range GET still caps large non-image payloads.
        let half_gib = 512 * 1024 * 1024;
        assert!(super::should_serve_full_without_range(
            true, "video", half_gib
        ));
        assert!(super::should_serve_full_without_range(
            true, "file", half_gib
        ));
        assert!(!super::should_serve_full_without_range(
            false, "video", half_gib
        ));
        assert!(super::should_serve_full_without_range(
            false,
            "video",
            super::MEDIA_PROTOCOL_DIRECT_FULL_MAX_BYTES
        ));
        assert!(super::should_serve_full_without_range(
            false,
            "image",
            32 * 1024 * 1024
        ));
    }

    #[test]
    fn custom_vault_dir_and_ws_auto_cache_classification_are_strict() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::media_vault::MediaVaultConfig;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("teza_vault_dir_val_{stamp}"));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("not_a_dir.txt");
        std::fs::write(&file, b"hi").unwrap();
        let new_sub = base.join("custom_media_vault");

        assert_eq!(super::validate_custom_vault_dir(None).unwrap(), None);
        assert_eq!(
            super::validate_custom_vault_dir(Some("   ".into())).unwrap(),
            None
        );
        assert!(super::validate_custom_vault_dir(Some("relative/path".into())).is_err());
        assert!(super::validate_custom_vault_dir(Some("C:\\bad\0dir".into())).is_err());
        assert!(super::validate_custom_vault_dir(Some(
            file.to_string_lossy().into_owned()
        ))
        .is_err());
        let created = super::validate_custom_vault_dir(Some(
            new_sub.to_string_lossy().into_owned(),
        ))
        .unwrap();
        assert!(created.is_some());
        assert!(new_sub.is_dir());

        // WS auto-cache classification & config gating
        let cfg = MediaVaultConfig::default();
        assert_eq!(super::classify_ws_auto_cache_kind("chat.photo"), Some("image"));
        assert_eq!(super::classify_ws_auto_cache_kind("chat.sticker"), Some("image"));
        assert_eq!(super::classify_ws_auto_cache_kind("chat.voice"), Some("audio"));
        assert_eq!(super::classify_ws_auto_cache_kind("chat.video"), Some("video"));
        assert_eq!(super::classify_ws_auto_cache_kind("share.file"), Some("file"));
        assert_eq!(super::classify_ws_auto_cache_kind("chat.link"), None);
        assert_eq!(super::classify_ws_auto_cache_kind("chat.text"), None);

        assert!(super::should_auto_cache_kind(&cfg, "image"));
        assert!(super::should_auto_cache_kind(&cfg, "audio"));
        assert!(!super::should_auto_cache_kind(&cfg, "video"));
        assert!(!super::should_auto_cache_kind(&cfg, "file"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn media_vault_probe_and_streaming_export_roundtrip_encrypted_large_blob() {
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::sync::Mutex;
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::media_vault::{MediaVault, MediaVaultConfig};
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_vault_rt_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();

        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(Mutex::new(
            MessageStore::open(&paths.history_db(Some("u1"))).unwrap(),
        ));
        let vault = MediaVault::open(&paths, Some("u1"), MediaVaultConfig::default()).unwrap();

        // Build >160 KiB synthetic ZIP/XLSX payload so probe reads head + tail in O(1) RAM
        let mut payload = b"PK\x03\x04\x14\x00\x00\x00\x00\x00".to_vec();
        payload.resize(170_000, 0x42);
        payload.extend_from_slice(b"xl/workbook.xml\x00PK\x05\x06");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let obj = vault
                .ingest_bytes(
                    &store,
                    &payload,
                    "file",
                    "application/octet-stream",
                    Some("https://file-stal-9.dlfl.vn/gr/abc/blob"),
                    Some(("t1", "m1")),
                    Some("report"),
                )
                .await
                .unwrap();
            assert!(obj.is_encrypted);

            let probe = super::read_vault_probe_bytes(&vault, &obj).await;
            assert!(probe.len() <= (super::VAULT_PROBE_WINDOW_BYTES * 2) as usize);
            let (resolved_name, resolved_ext) = super::file_dialog::resolve_download_filename_and_ext(
                "download",
                None,
                "https://file-stal-9.dlfl.vn/gr/abc/blob",
                "https://file-stal-9.dlfl.vn/gr/abc/blob",
                &obj.mime,
                &probe,
            );
            assert_eq!(resolved_name, "download.xlsx");
            assert_eq!(resolved_ext.as_deref(), Some("xlsx"));

            let export_path = dir.join("exported.xlsx");
            let written = vault.export_to_path(&obj, &export_path).await.unwrap();
            assert_eq!(written, payload.len() as u64);
            assert_eq!(std::fs::read(&export_path).unwrap(), payload);
        });

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gdrive_relay_exclusion_gate_and_status_snapshot_formatting() {
        use zalo_core::gdrive::{
            CloudMasterKey, GDriveStoredConfig, GDriveTokens, GDRIVE_PHASE_IDLE,
            GDRIVE_PHASE_PAUSED_RELAY, GDRIVE_PHASE_UPLOADING_MEDIA,
        };
        use zalo_core::store::CloudMediaBreakdown;

        // 1. Strict Relay exclusion helper
        assert!(super::check_gdrive_relay_gate(false).is_ok());
        let blocked_err = super::check_gdrive_relay_gate(true).unwrap_err();
        assert!(
            blocked_err.to_ascii_lowercase().contains("relay"),
            "unexpected error message: {blocked_err}"
        );

        // 2. Status snapshot formatting when Relay is ON vs OFF
        let inner = super::GDriveInner {
            uid: Some("u42".into()),
            config: GDriveStoredConfig {
                client_id: "test-client-id.apps.googleusercontent.com".into(),
                client_secret_dpapi_b64: None,
                token_broker_url: String::new(),
                sync_enabled: true,
                sync_media: true,
                sync_history: true,
                auto_sync_interval_secs: 300,
            },
            tokens: Some(GDriveTokens {
                access_token: "ya29.secret".into(),
                refresh_token: "1//refresh".into(),
                expires_at_epoch_secs: 9_999_999_999,
                account_email: Some("alice@example.com".into()),
                account_name: Some("Alice".into()),
                root_folder_id: Some("r1".into()),
                uid_folder_id: Some("u1".into()),
                blobs_folder_id: Some("b1".into()),
                history_folder_id: Some("h1".into()),
                manifest_file_id: Some("m1".into()),
            }),
            master_key: Some(CloudMasterKey::from_bytes([7u8; 32])),
            has_remote_manifest: true,
            syncing: true,
            phase: GDRIVE_PHASE_UPLOADING_MEDIA.to_string(),
            progress_done: 3,
            progress_total: 10,
            last_sync_ts: Some(1_700_000_000_000),
            last_history_sync_ts: Some(1_700_000_000_000),
            last_error: None,
        };
        let breakdown = CloudMediaBreakdown {
            total_bytes: 2048,
            total_count: 2,
            images_bytes: 2048,
            videos_bytes: 0,
            voice_bytes: 0,
            files_bytes: 0,
            pending_count: 4,
            pending_bytes: 8192,
        };

        let paused = super::build_gdrive_status_snapshot(true, &inner, &breakdown);
        assert!(paused.relay_blocked);
        assert_eq!(paused.phase, GDRIVE_PHASE_PAUSED_RELAY);
        assert!(!paused.syncing);
        assert_eq!(paused.progress_done, 0);
        assert_eq!(paused.progress_total, 0);
        assert!(paused.connected);
        assert!(paused.vault_unlocked);
        assert_eq!(paused.account_email.as_deref(), Some("alice@example.com"));

        let active = super::build_gdrive_status_snapshot(false, &inner, &breakdown);
        assert!(!active.relay_blocked);
        assert_eq!(active.phase, GDRIVE_PHASE_UPLOADING_MEDIA);
        assert!(active.syncing);
        assert_eq!(active.progress_done, 3);
        assert_eq!(active.progress_total, 10);
        assert_eq!(active.pending_media_count, 4);
        assert_eq!(active.pending_media_bytes, 8192);

        // Stale paused_relay phase normalizes back to idle once Relay is disabled
        let mut paused_inner = inner;
        paused_inner.syncing = false;
        paused_inner.phase = GDRIVE_PHASE_PAUSED_RELAY.to_string();
        let resumed = super::build_gdrive_status_snapshot(false, &paused_inner, &breakdown);
        assert!(!resumed.relay_blocked);
        assert_eq!(resumed.phase, GDRIVE_PHASE_IDLE);
        assert!(!resumed.syncing);
    }

    #[test]
    fn gdrive_passphrase_validation_and_prune_cutoff_calculation() {
        // Passphrase validation (< 8 chars or whitespace-only rejected; >= 8 accepted)
        assert!(super::validate_gdrive_passphrase("").is_err());
        assert!(super::validate_gdrive_passphrase("short77").is_err());
        assert!(super::validate_gdrive_passphrase("        ").is_err());
        assert!(super::validate_gdrive_passphrase("   1234567   ").is_err());
        assert!(super::validate_gdrive_passphrase("12345678").is_ok());
        assert!(super::validate_gdrive_passphrase("mật khẩu mạnh").is_ok());

        // Prune age cutoff calculation
        let now_ms: i64 = 1_750_000_000_000;
        assert_eq!(super::compute_prune_cutoff_ts_ms(now_ms, 0), now_ms);
        assert_eq!(
            super::compute_prune_cutoff_ts_ms(now_ms, 30),
            now_ms - 30 * 86_400_000
        );
        assert_eq!(
            super::compute_prune_cutoff_ts_ms(now_ms, 90),
            now_ms - 90 * 86_400_000
        );
        assert_eq!(
            super::compute_prune_cutoff_ts_ms(now_ms, 365),
            now_ms - 365 * 86_400_000
        );
        // Must never panic on extreme day counts
        let _ = super::compute_prune_cutoff_ts_ms(now_ms, u32::MAX);
    }

    #[test]
    fn gdrive_tier2_rehydration_gating_respects_relay_lock_and_expiry() {
        use zalo_core::session::MediaDownloadError;
        use zalo_core::store::{
            MediaObjectRecord, MEDIA_LOCAL_STATE_LOCAL, MEDIA_LOCAL_STATE_MISSING,
        };

        let make_obj = |local_state: i32, cloud_file_id: Option<&str>| MediaObjectRecord {
            sha256: "a".repeat(64),
            kind: "video".into(),
            mime: "video/mp4".into(),
            size_bytes: 123_456,
            thumb_sha256: None,
            width: None,
            height: None,
            duration_ms: None,
            local_state,
            local_rel_path: None,
            is_encrypted: true,
            pin_count: 0,
            created_ts: 1,
            last_accessed_ts: 1,
            cloud_provider: cloud_file_id.map(|_| "gdrive".into()),
            cloud_file_id: cloud_file_id.map(str::to_string),
            cloud_encrypted: true,
            cloud_key_ver: 1,
            cloud_synced_ts: Some(10),
        };

        let expired_err = anyhow::Error::new(MediaDownloadError::CdnExpired { status: 403 });
        let transient_err = anyhow::anyhow!("network timeout");
        let evicted_cloud = make_obj(MEDIA_LOCAL_STATE_MISSING, Some("gd-file-1"));
        let local_cloud = make_obj(MEDIA_LOCAL_STATE_LOCAL, Some("gd-file-1"));
        let no_cloud = make_obj(MEDIA_LOCAL_STATE_MISSING, None);

        // Relay active MUST ALWAYS block Tier 2 rehydration
        assert!(!super::should_trigger_gdrive_rehydration(
            true,
            true,
            &expired_err,
            Some(&evicted_cloud)
        ));
        // Locked/disconnected Drive MUST block Tier 2 rehydration
        assert!(!super::should_trigger_gdrive_rehydration(
            false,
            false,
            &expired_err,
            Some(&evicted_cloud)
        ));
        // No cloud_file_id -> cannot rehydrate
        assert!(!super::should_trigger_gdrive_rehydration(
            false,
            true,
            &expired_err,
            Some(&no_cloud)
        ));
        // CDN expired + cloud_file_id + !relay + unlocked -> triggers rehydration
        assert!(super::should_trigger_gdrive_rehydration(
            false,
            true,
            &expired_err,
            Some(&local_cloud)
        ));
        // Evicted local blob (local_state == 0) + cloud_file_id + !relay + unlocked -> triggers rehydration
        assert!(super::should_trigger_gdrive_rehydration(
            false,
            true,
            &transient_err,
            Some(&evicted_cloud)
        ));
        // Present local state + non-expired transient error -> does not trigger Drive rehydration
        assert!(!super::should_trigger_gdrive_rehydration(
            false,
            true,
            &transient_err,
            Some(&local_cloud)
        ));
    }

    #[test]
    fn gdrive_rehydrate_singleflight_guard_never_loses_follower_wakeup() {
        use std::collections::HashMap;
        use std::sync::Mutex as StdMutex;

        let map: StdMutex<HashMap<String, tokio::sync::watch::Receiver<bool>>> =
            StdMutex::new(HashMap::new());
        let sha = "b".repeat(64);
        let (tx, rx) = tokio::sync::watch::channel(false);
        map.lock().unwrap().insert(sha.clone(), rx);
        let leader_guard = super::RehydrateLeaderGuard {
            map: &map,
            sha: sha.clone(),
            tx: Some(tx),
        };
        // Follower clones receiver while Leader is active
        let mut follower_rx = map.lock().unwrap().get(&sha).unwrap().clone();
        // Leader finishes and drops BEFORE Follower begins awaiting `wait_for`
        drop(leader_guard);
        assert!(
            !map.lock().unwrap().contains_key(&sha),
            "leader drop must remove singleflight key"
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let res = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                follower_rx.wait_for(|&done| done),
            )
            .await
            .expect("follower must wake immediately without lost-wakeup deadlock");
            assert!(res.is_ok());
            assert!(*res.unwrap());
        });
    }

    #[test]
    fn social_ws_event_dto_serializes_and_deserializes_exact_contract() {
        let ev = super::SocialWsEventDto {
            kind: "group_left".to_string(),
            thread_id: Some("987654321".to_string()),
            user_id: None,
            self_removed: true,
        };
        let v = serde_json::to_value(&ev).expect("serialize SocialWsEventDto");
        assert_eq!(v["kind"], "group_left");
        assert_eq!(v["thread_id"], "987654321");
        assert!(v["user_id"].is_null());
        assert_eq!(v["self_removed"], true);
        let roundtrip: super::SocialWsEventDto =
            serde_json::from_value(v).expect("deserialize SocialWsEventDto");
        assert_eq!(roundtrip, ev);

        let friend_ev = super::SocialWsEventDto {
            kind: "friend_added".to_string(),
            thread_id: None,
            user_id: Some("123456789".to_string()),
            self_removed: false,
        };
        let fv = serde_json::to_value(&friend_ev).expect("serialize friend event");
        assert_eq!(fv["kind"], "friend_added");
        assert!(fv["thread_id"].is_null());
        assert_eq!(fv["user_id"], "123456789");
        assert_eq!(fv["self_removed"], false);
        let roundtrip_friend: super::SocialWsEventDto =
            serde_json::from_value(fv).expect("deserialize friend event");
        assert_eq!(roundtrip_friend, friend_ev);
    }

    #[test]
    fn social_input_validation_helpers_clean_and_reject_invalid_inputs() {
        // User ID validation
        assert_eq!(
            super::validate_social_user_id("  123456_0 ").unwrap(),
            "123456"
        );
        assert_eq!(super::validate_social_user_id("998877").unwrap(), "998877");
        assert!(super::validate_social_user_id("").is_err());
        assert!(super::validate_social_user_id("   ").is_err());
        assert!(super::validate_social_user_id("_0").is_err());

        // Group ID validation
        assert_eq!(
            super::validate_social_group_id(" g123456789 ").unwrap(),
            "123456789"
        );
        assert_eq!(
            super::validate_social_group_id("group_555666").unwrap(),
            "555666"
        );
        assert_eq!(
            super::validate_social_group_id("G777888").unwrap(),
            "777888"
        );
        assert!(super::validate_social_group_id("").is_err());
        assert!(super::validate_social_group_id("   ").is_err());
        assert!(super::validate_social_group_id("group_").is_err());
        assert!(super::validate_social_group_id("g").is_err());

        // Member IDs normalization, dedup, and self-kick guard
        let cleaned = super::normalize_social_member_ids(
            &[
                " 111_0 ".to_string(),
                "".to_string(),
                "222".to_string(),
                "111".to_string(),
                "   ".to_string(),
            ],
            Some("999_0"),
        )
        .unwrap();
        assert_eq!(cleaned, vec!["111".to_string(), "222".to_string()]);
        assert!(super::normalize_social_member_ids(&[], Some("999")).is_err());
        assert!(super::normalize_social_member_ids(&["   ".to_string()], Some("999")).is_err());
        assert!(
            super::normalize_social_member_ids(
                &["111".to_string(), "999_0".to_string()],
                Some("999")
            )
            .is_err()
        );

        // Friend request message normalization and UTF-8 safe truncation
        assert_eq!(super::normalize_friend_request_message(None), None);
        assert_eq!(super::normalize_friend_request_message(Some("   \t\n ")), None);
        assert_eq!(
            super::normalize_friend_request_message(Some("  Xin chào bạn!  ")),
            Some("Xin chào bạn!".to_string())
        );
        let long_vi = "Chào ".repeat(50); // 250 Unicode chars with diacritics
        let truncated = super::normalize_friend_request_message(Some(&long_vi)).unwrap();
        assert_eq!(truncated.chars().count(), 150);
    }

    #[test]
    fn build_media_success_response_emits_and_sanitizes_content_disposition() {
        let cd = super::file_dialog::build_inline_content_disposition("Báo cáo quý 1.png");
        let resp = super::build_media_success_response(
            200,
            "image/png",
            4,
            None,
            Some(cd.as_str()),
            None,
            vec![137, 80, 78, 71],
        );
        assert_eq!(resp.status().as_u16(), 200);
        let hdr = resp
            .headers()
            .get("content-disposition")
            .expect("content-disposition header should be emitted")
            .to_str()
            .unwrap();
        assert!(hdr.starts_with("inline; filename=\""), "unexpected header: {hdr}");
        assert!(hdr.contains(".png\""), "expected .png ASCII fallback in {hdr}");
        assert!(hdr.contains("filename*=UTF-8''"), "expected RFC 5987 UTF-8 param in {hdr}");

        // None omits content-disposition
        let no_cd = super::build_media_success_response(
            200,
            "image/jpeg",
            2,
            None,
            None,
            None,
            vec![0xFF, 0xD8],
        );
        assert!(no_cd.headers().get("content-disposition").is_none());

        // Control characters (CRLF header injection attempt) must be suppressed cleanly
        let injected = super::build_media_success_response(
            200,
            "image/png",
            2,
            None,
            Some("inline; filename=\"a.png\"\r\nX-Evil: 1"),
            None,
            vec![1, 2],
        );
        assert_eq!(injected.status().as_u16(), 200);
        assert!(injected.headers().get("content-disposition").is_none());
        assert!(injected.headers().get("x-evil").is_none());
    }

    #[test]
    fn unwrap_teza_media_url_extracts_remote_url_and_msg_ref_safely() {
        // Direct HTTPS URL passes through untouched
        let (u, tid, mid) =
            super::unwrap_teza_media_url("https://photo.zdn.vn/original/pic.webp").unwrap();
        assert_eq!(u, "https://photo.zdn.vn/original/pic.webp");
        assert_eq!(tid, None);
        assert_eq!(mid, None);

        // Windows WebView2 loopback protocol URL unwraps inner URL + tid/mid
        let (u2, tid2, mid2) = super::unwrap_teza_media_url(
            "http://teza-media.localhost/resolve?url=https%3A%2F%2Fphoto.zdn.vn%2Fimg_123&kind=image&t=secret&tid=thread-9&mid=msg-42",
        )
        .unwrap();
        assert_eq!(u2, "https://photo.zdn.vn/img_123");
        assert_eq!(tid2.as_deref(), Some("thread-9"));
        assert_eq!(mid2.as_deref(), Some("msg-42"));

        // HTTPS teza-media.localhost and custom teza-media:// scheme also unwrap cleanly
        let (u3, tid3, mid3) = super::unwrap_teza_media_url(
            "https://teza-media.localhost/resolve?url=https%3A%2F%2Fcdn.zdn.vn%2Fvoice.aac&kind=audio&t=tok",
        )
        .unwrap();
        assert_eq!(u3, "https://cdn.zdn.vn/voice.aac");
        assert_eq!(tid3, None);
        assert_eq!(mid3, None);

        let (u4, tid4, mid4) = super::unwrap_teza_media_url(
            "teza-media://localhost/resolve?url=https%3A%2F%2Fcdn.zdn.vn%2Fdoc.pdf&tid=t1&mid=m2&t=tok",
        )
        .unwrap();
        assert_eq!(u4, "https://cdn.zdn.vn/doc.pdf");
        assert_eq!(tid4.as_deref(), Some("t1"));
        assert_eq!(mid4.as_deref(), Some("m2"));

        // Unsafe inner targets or wrong paths must be rejected
        for bad in [
            "http://teza-media.localhost/resolve?url=file%3A%2F%2F%2FC%3A%2FWindows%2Fwin.ini&t=tok",
            "http://teza-media.localhost/resolve?url=http%3A%2F%2Ftauri.localhost%2Findex.html&t=tok",
            "teza-media://localhost/etc/passwd?url=https%3A%2F%2Fphoto.zdn.vn%2Fa.jpg&t=tok",
            "http://teza-media.localhost/resolve?t=tok",
        ] {
            assert!(
                super::unwrap_teza_media_url(bad).is_err(),
                "expected unwrap_teza_media_url to reject: {bad}"
            );
        }

        assert_eq!(super::normalize_ai_session_id(None), "default");
        assert_eq!(super::normalize_ai_session_id(Some("   ")), "default");
        assert_eq!(super::normalize_ai_session_id(Some(" work ")), "work");
    }

    #[test]
    fn resolve_phone_country_code_defaults_to_vietnam_and_trims_override() {
        assert_eq!(super::resolve_phone_country_code(None), "84");
        assert_eq!(super::resolve_phone_country_code(Some("")), "84");
        assert_eq!(super::resolve_phone_country_code(Some("   \t ")), "84");
        assert_eq!(super::resolve_phone_country_code(Some("82")), "82");
        assert_eq!(super::resolve_phone_country_code(Some(" +82 ")), "+82");
    }

    #[test]
    fn upsert_incoming_message_dedupes_optimistic_poll_and_merges_sparse_payload() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::chat::{MessageDto, PollDto, PollOptionDto};
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_poll_dedupe_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let mut store = MessageStore::open(&paths.history_db(Some("u-poll"))).unwrap();

        let optimistic_poll = PollDto {
            poll_id: 777,
            question: "Trưa nay ăn gì?".into(),
            options: vec![
                PollOptionDto {
                    id: 11,
                    content: "Phở".into(),
                    votes: 0,
                    voted: false,
                },
                PollOptionDto {
                    id: 12,
                    content: "Bún chả".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: false,
            expired_time: 1_900_000_000_000,
            closed: false,
        };
        let optimistic_msg = MessageDto {
            msg_id: "poll-777".into(),
            cli_msg_id: None,
            thread_id: "grp-100".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "Bình chọn: Trưa nay ăn gì?".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_000_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&optimistic_poll).unwrap()
            )),
        };
        assert_eq!(super::real_poll_id_for_dedupe(&optimistic_msg), None);
        store.upsert_message(&optimistic_msg).unwrap();

        // Real server message arrives with sparse poll payload (empty options + generic title)
        let sparse_server_poll = PollDto {
            poll_id: 777,
            question: "Bình chọn".into(),
            options: Vec::new(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let mut real_msg = MessageDto {
            msg_id: "90012345".into(),
            cli_msg_id: Some("cli-1".into()),
            thread_id: "grp-100".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "Bình chọn".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_001_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&sparse_server_poll).unwrap()
            )),
        };
        assert_eq!(super::real_poll_id_for_dedupe(&real_msg), Some(777));
        super::upsert_incoming_message(&mut store, &mut real_msg).unwrap();

        let stored = store.load_messages("grp-100", None, 20).unwrap();
        assert_eq!(stored.len(), 1, "optimistic poll card must be deduplicated");
        assert_eq!(stored[0].msg_id, "90012345");
        assert_eq!(stored[0].text, "Bình chọn: Trưa nay ăn gì?");
        let stored_poll = super::parse_message_poll_dto(&stored[0]).unwrap();
        assert_eq!(stored_poll.question, "Trưa nay ăn gì?");
        assert_eq!(stored_poll.options.len(), 2);
        assert!(stored_poll.allow_multi);
        assert!(stored_poll.allow_add_option);

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_poll_update_to_store_finds_by_poll_id_merges_partial_and_locks() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::chat::{MessageDto, PollDto, PollOptionDto};
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_poll_update_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let mut store = MessageStore::open(&paths.history_db(Some("u-poll-2"))).unwrap();

        let initial_poll = PollDto {
            poll_id: 888,
            question: "Họp lúc mấy giờ?".into(),
            options: vec![
                PollOptionDto {
                    id: 1,
                    content: "9h".into(),
                    votes: 1,
                    voted: true,
                },
                PollOptionDto {
                    id: 2,
                    content: "14h".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: true,
            expired_time: 1_950_000_000_000,
            closed: false,
        };
        let msg = MessageDto {
            msg_id: "srv-88801".into(),
            cli_msg_id: None,
            thread_id: "grp-200".into(),
            sender_id: "u1".into(),
            sender_name: "An".into(),
            text: "Bình chọn: Họp lúc mấy giờ?".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_010_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&initial_poll).unwrap()
            )),
        };
        store.upsert_message(&msg).unwrap();

        // Partial vote/add-option response without question/flags and without msg_id
        let partial_update = PollDto {
            poll_id: 888,
            question: "Bình chọn".into(),
            options: vec![
                PollOptionDto {
                    id: 1,
                    content: "9h".into(),
                    votes: 2,
                    voted: true,
                },
                PollOptionDto {
                    id: 2,
                    content: "14h".into(),
                    votes: 1,
                    voted: false,
                },
                PollOptionDto {
                    id: 3,
                    content: "16h".into(),
                    votes: 1,
                    voted: true,
                },
            ],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let (updated_msg, merged) =
            super::apply_poll_update_to_store(&mut store, Some("grp-200"), None, &partial_update)
                .expect("should locate poll message by (thread_id, poll_id) without msg_id");
        assert_eq!(updated_msg.msg_id, "srv-88801");
        assert_eq!(merged.question, "Họp lúc mấy giờ?");
        assert_eq!(merged.options.len(), 3);
        assert!(merged.allow_multi);
        assert!(merged.allow_add_option);
        assert!(merged.is_anonymous);
        assert!(!merged.closed);

        // Lock update without msg_id marks closed = true while preserving metadata
        let lock_update = PollDto {
            closed: true,
            ..partial_update
        };
        let (_, locked) =
            super::apply_poll_update_to_store(&mut store, Some("grp-200"), None, &lock_update)
                .expect("lock update should succeed");
        assert!(locked.closed);
        assert_eq!(locked.question, "Họp lúc mấy giờ?");

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_create_reverse_order_race_merges_into_existing_real_ws_row_without_duplicate() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::chat::{MessageDto, PollDto, PollOptionDto};
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_poll_reverse_race_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let mut store = MessageStore::open(&paths.history_db(Some("u-poll-race"))).unwrap();

        // 1. Real WS poll message arrives and is persisted BEFORE `poll_create` finishes.
        let sparse_ws_poll = PollDto {
            poll_id: 901,
            question: "Bình chọn".into(),
            options: Vec::new(),
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let mut ws_real_msg = MessageDto {
            msg_id: "99001122".into(),
            cli_msg_id: Some("cli-race-1".into()),
            thread_id: "300400".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "Bình chọn".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_020_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&sparse_ws_poll).unwrap()
            )),
        };
        super::upsert_incoming_message(&mut store, &mut ws_real_msg).unwrap();

        // 2. `poll_create` finishes later with synthetic `poll-901` carrying full metadata.
        let created_poll = PollDto {
            poll_id: 901,
            question: "Cuối tuần đi đâu?".into(),
            options: vec![
                PollOptionDto {
                    id: 51,
                    content: "Đà Lạt".into(),
                    votes: 0,
                    voted: false,
                },
                PollOptionDto {
                    id: 52,
                    content: "Vũng Tàu".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: true,
            allow_add_option: true,
            is_anonymous: true,
            expired_time: 1_960_000_000_000,
            closed: false,
        };
        let synthetic_created_msg = MessageDto {
            msg_id: "poll-901".into(),
            cli_msg_id: None,
            thread_id: "g300400".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "Bình chọn: Cuối tuần đi đâu?".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_020_500,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&created_poll).unwrap()
            )),
        };

        let result =
            super::persist_created_poll_message_in_store(&mut store, synthetic_created_msg)
                .unwrap();
        let merged_msg = match result {
            super::PollCreatePersistResult::MergedIntoReal { msg } => msg,
            super::PollCreatePersistResult::InsertedNew { .. } => {
                panic!("expected reverse-order poll_create to merge into existing real WS row")
            }
        };
        assert_eq!(merged_msg.msg_id, "99001122");
        assert_eq!(merged_msg.text, "Bình chọn: Cuối tuần đi đâu?");
        let merged_poll = super::parse_message_poll_dto(&merged_msg).unwrap();
        assert_eq!(merged_poll.question, "Cuối tuần đi đâu?");
        assert_eq!(merged_poll.options.len(), 2);
        assert!(merged_poll.allow_multi);
        assert!(merged_poll.allow_add_option);
        assert!(merged_poll.is_anonymous);

        // Verify zero duplicate rows in SQLite and messages_fts.
        assert!(store.get_message("300400", "poll-901").unwrap().is_none());
        assert!(store.get_message("g300400", "poll-901").unwrap().is_none());
        let thread_msgs = store.load_messages("300400", None, 20).unwrap();
        assert_eq!(thread_msgs.len(), 1);
        assert_eq!(thread_msgs[0].msg_id, "99001122");
        let fts_hits = store.search_messages("Cuối tuần", 20).unwrap();
        assert_eq!(fts_hits.len(), 1);
        assert_eq!(fts_hits[0].msg_id, "99001122");

        // 3. Normal order (no real WS row yet) inserts the synthetic row as expected.
        let normal_poll = PollDto {
            poll_id: 902,
            question: "Uống trà sữa?".into(),
            options: vec![
                PollOptionDto {
                    id: 61,
                    content: "Có".into(),
                    votes: 0,
                    voted: false,
                },
                PollOptionDto {
                    id: 62,
                    content: "Không".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let normal_msg = MessageDto {
            msg_id: "poll-902".into(),
            cli_msg_id: None,
            thread_id: "300400".into(),
            sender_id: "me".into(),
            sender_name: "Bạn".into(),
            text: "Bình chọn: Uống trà sữa?".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_021_000,
            is_outgoing: true,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&normal_poll).unwrap()
            )),
        };
        match super::persist_created_poll_message_in_store(&mut store, normal_msg).unwrap() {
            super::PollCreatePersistResult::InsertedNew { msg, .. } => {
                assert_eq!(msg.msg_id, "poll-902");
            }
            super::PollCreatePersistResult::MergedIntoReal { .. } => {
                panic!("expected normal-order poll_create to insert new synthetic row")
            }
        }

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_invoke_body_bytes_decodes_indexed_json_object_and_base64_string() {
        use tauri::ipc::InvokeBody;

        // Cross-realm Uint8Array JSON.stringify produces contiguous "0".."N-1" object
        let indexed_obj = InvokeBody::Json(serde_json::json!({
            "2": 78,
            "0": 137,
            "3": 71,
            "1": 80
        }));
        assert_eq!(
            super::extract_invoke_body_bytes(&indexed_obj, 16)
                .unwrap()
                .as_ref(),
            &[137, 80, 78, 71]
        );
        assert!(super::extract_invoke_body_bytes(&indexed_obj, 3).is_err());

        // Gaps, non-numeric keys, out-of-range bytes, and empty objects must be rejected
        let gapped = InvokeBody::Json(serde_json::json!({ "0": 1, "2": 3 }));
        assert!(super::extract_invoke_body_bytes(&gapped, 16).is_err());
        let bad_val = InvokeBody::Json(serde_json::json!({ "0": 256 }));
        assert!(super::extract_invoke_body_bytes(&bad_val, 16).is_err());
        let empty_obj = InvokeBody::Json(serde_json::json!({}));
        assert!(super::extract_invoke_body_bytes(&empty_obj, 16).is_err());

        // Standard base64 JSON string decodes cleanly and enforces max_bytes
        let b64 = InvokeBody::Json(serde_json::json!("iVBORw=="));
        assert_eq!(
            super::extract_invoke_body_bytes(&b64, 16).unwrap().as_ref(),
            &[137, 80, 78, 71]
        );
        assert!(super::extract_invoke_body_bytes(&b64, 3).is_err());
        let empty_str = InvokeBody::Json(serde_json::json!("   "));
        assert!(super::extract_invoke_body_bytes(&empty_str, 16).is_err());
        let invalid_b64 = InvokeBody::Json(serde_json::json!("not!valid!b64"));
        assert!(super::extract_invoke_body_bytes(&invalid_b64, 16).is_err());
    }

    #[test]
    fn should_dispatch_ws_event_accepts_e2ee_push_and_payload_bearing_unknown_cmds() {
        let mk_ev = |cmd: u16, payload: serde_json::Value| zalo_core::socket::WsEvent {
            cmd,
            sub_cmd: 0,
            version: 1,
            payload,
        };

        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PULL_MSG_E2EE_1_1,
            serde_json::json!({})
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PULL_MSG_E2EE_GROUP,
            serde_json::json!({})
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PUSH_MSG_E2EE_1_1,
            serde_json::json!({})
        )));
        assert!(super::should_dispatch_ws_event(&mk_ev(
            zalo_core::socket::CMD_PUSH_MSG_E2EE_GROUP,
            serde_json::json!({})
        )));

        // Unknown qCmds event carrying chat message payload must be dispatched
        let unknown_stranger_push = mk_ev(
            599,
            serde_json::json!({
                "data": {
                    "strangerMsgs": [
                        { "msgId": "7001", "uidFrom": "888777", "idTo": "111222", "content": "Hello" }
                    ]
                }
            }),
        );
        assert!(super::should_dispatch_ws_event(&unknown_stranger_push));

        // Unknown event with empty payload stays ignored
        assert!(!super::should_dispatch_ws_event(&mk_ev(
            599,
            serde_json::json!({})
        )));
    }

    #[test]
    fn upsert_incoming_message_merges_real_vs_real_poll_votes_in_place() {
        use std::time::{SystemTime, UNIX_EPOCH};
        use zalo_core::chat::{MessageDto, PollDto, PollOptionDto};
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_poll_real_merge_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let mut store = MessageStore::open(&paths.history_db(Some("u-poll-real"))).unwrap();

        let original_poll = PollDto {
            poll_id: 5501,
            question: "Đi ăn tối lúc mấy giờ?".into(),
            options: vec![
                PollOptionDto {
                    id: 1,
                    content: "19h".into(),
                    votes: 1,
                    voted: true,
                },
                PollOptionDto {
                    id: 2,
                    content: "20h".into(),
                    votes: 0,
                    voted: false,
                },
            ],
            allow_multi: false,
            allow_add_option: true,
            is_anonymous: false,
            expired_time: 1_999_000_000_000,
            closed: false,
        };
        let mut first_real = MessageDto {
            msg_id: "800001".into(),
            cli_msg_id: Some("cli-5501".into()),
            thread_id: "g9001".into(),
            sender_id: "u-creator".into(),
            sender_name: "Minh".into(),
            text: "Bình chọn: Đi ăn tối lúc mấy giờ?".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_100_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&original_poll).unwrap()
            )),
        };

        let first_was_updated =
            super::upsert_incoming_message(&mut store, &mut first_real).unwrap();
        assert!(
            !first_was_updated,
            "first real poll insertion must report was_updated_existing == false"
        );

        // Second real server message arrives with a brand-new msg_id from someone voting
        let vote_poll = PollDto {
            poll_id: 5501,
            question: "Bình chọn".into(),
            options: vec![
                PollOptionDto {
                    id: 1,
                    content: "19h".into(),
                    votes: 1,
                    voted: true,
                },
                PollOptionDto {
                    id: 2,
                    content: "20h".into(),
                    votes: 2,
                    voted: false,
                },
            ],
            allow_multi: false,
            allow_add_option: false,
            is_anonymous: false,
            expired_time: 0,
            closed: false,
        };
        let mut vote_msg = MessageDto {
            msg_id: "800099".into(),
            cli_msg_id: Some("cli-5501-vote".into()),
            thread_id: "g9001".into(),
            sender_id: "u-voter".into(),
            sender_name: "Lan".into(),
            text: "Lan đã tham gia bình chọn".into(),
            msg_type: "chat.poll".into(),
            ts: 1_700_000_200_000,
            is_outgoing: false,
            is_recalled: false,
            is_pinned: false,
            quote: None,
            reactions: Vec::new(),
            attachment_url: None,
            attachment_name: Some(format!(
                "teza-poll:{}",
                serde_json::to_string(&vote_poll).unwrap()
            )),
        };

        let second_was_updated =
            super::upsert_incoming_message(&mut store, &mut vote_msg).unwrap();
        assert!(
            second_was_updated,
            "real-vs-real poll vote must report was_updated_existing == true"
        );
        // Caller receives canonical original msg_id/ts/sender with merged poll state
        assert_eq!(vote_msg.msg_id, "800001");
        assert_eq!(vote_msg.ts, 1_700_000_100_000);
        assert_eq!(vote_msg.sender_id, "u-creator");
        assert_eq!(vote_msg.sender_name, "Minh");
        assert_eq!(vote_msg.text, "Bình chọn: Đi ăn tối lúc mấy giờ?");
        let merged_poll = super::parse_message_poll_dto(&vote_msg).unwrap();
        assert_eq!(merged_poll.question, "Đi ăn tối lúc mấy giờ?");
        assert_eq!(merged_poll.options.len(), 2);
        assert_eq!(merged_poll.options[1].votes, 2);
        assert!(merged_poll.allow_add_option);

        // Only 1 row exists in SQLite — no duplicate created
        let rows = store.load_messages("g9001", None, 20).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].msg_id, "800001");

        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_raw_upload_helpers_sanitize_names_and_evict_stale_and_excess_entries() {
        use std::collections::HashMap;
        use std::path::PathBuf;

        assert_eq!(
            super::sanitize_staged_upload_filename("C:\\Users\\Alice\\Pictures\\shot.png"),
            "shot.png"
        );
        assert_eq!(
            super::sanitize_staged_upload_filename("../../etc/passwd"),
            "passwd"
        );
        assert_eq!(super::sanitize_staged_upload_filename("   "), "clipboard.png");
        assert_eq!(super::sanitize_staged_upload_filename(".."), "clipboard.png");

        let mut map = HashMap::new();
        let now_ms: i64 = 10_000_000;
        // 1 expired entry (> 30 min)
        map.insert(
            "expired".to_string(),
            super::StagedRawUpload {
                path: PathBuf::from("expired.part"),
                file_name: "old.png".into(),
                bytes_written: 10,
                created_ms: now_ms - super::STAGED_RAW_UPLOAD_TTL_MS - 1,
            },
        );
        // 16 active entries (exceeds reserve_slot cap of 15)
        for i in 0..super::MAX_STAGED_RAW_UPLOADS {
            map.insert(
                format!("up-{i}"),
                super::StagedRawUpload {
                    path: PathBuf::from(format!("up-{i}.part")),
                    file_name: format!("f{i}.png"),
                    bytes_written: 10,
                    created_ms: now_ms - (super::MAX_STAGED_RAW_UPLOADS - i) as i64 * 1000,
                },
            );
        }

        let evicted = super::prune_staged_raw_uploads(&mut map, now_ms, true);
        assert_eq!(evicted.len(), 2);
        assert!(evicted.contains(&PathBuf::from("expired.part")));
        assert!(evicted.contains(&PathBuf::from("up-0.part")));
        assert_eq!(map.len(), super::MAX_STAGED_RAW_UPLOADS - 1);

        // Direct peer thread ID helper
        assert!(super::is_direct_peer_thread_id("88997766", "111", "222"));
        assert!(!super::is_direct_peer_thread_id("g12345", "111", "222"));
        assert!(!super::is_direct_peer_thread_id("cloud_123", "111", "222"));
        assert!(!super::is_direct_peer_thread_id("cloud-me", "111", "222"));
        assert!(!super::is_direct_peer_thread_id("111", "111", "222"));
        assert!(!super::is_direct_peer_thread_id("222", "111", "222"));
    }

    #[test]
    fn local_vault_only_forward_resolution_succeeds_without_attachment_url() {
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::sync::Mutex;
        use zalo_core::chat::messaging;
        use zalo_core::data_dir::StoragePaths;
        use zalo_core::media_vault::{MediaVault, MediaVaultConfig};
        use zalo_core::store::MessageStore;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_tauri_fwd_local_vault_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = StoragePaths::from_dir(&dir).unwrap();
        let store = Arc::new(Mutex::new(
            MessageStore::open(&paths.history_db(Some("u-fwd-local"))).unwrap(),
        ));
        let vault = Arc::new(
            MediaVault::open(
                &paths,
                Some("u-fwd-local"),
                MediaVaultConfig {
                    encrypt_at_rest: true,
                    ..MediaVaultConfig::default()
                },
            )
            .unwrap(),
        );

        let png_payload = b"\x89PNG\r\n\x1a\nlocal-only-forward-test-bytes".to_vec();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Ingest a local-only media attachment (url_opt = None -> url_hash = "", no original_name)
            let ingested = vault
                .ingest_bytes(
                    &store,
                    &png_payload,
                    "image",
                    "image/png",
                    None,
                    Some(("peer-42", "msg-local-1")),
                    None,
                )
                .await
                .unwrap();

            let mm_rec = {
                let guard = store.lock().await;
                guard
                    .get_message_media_record("peer-42", "msg-local-1", "primary")
                    .unwrap()
                    .expect("message_media row must exist")
            };
            assert!(mm_rec.url_hash.is_empty());

            // Even with attachment_url == None, attachment_name == None, original_name == None,
            // effective_forward_attachment_name + is_forwardable_media_message must classify it as media
            let eff_name = super::effective_forward_attachment_name(None, Some(&mm_rec));
            assert_eq!(eff_name, Some("attachment.bin"));
            assert!(messaging::is_forwardable_media_message(
                "chat.photo",
                None,
                eff_name
            ));

            // Named message_media.original_name takes precedence over fallback "attachment.bin"
            let mut mm_named = mm_rec.clone();
            mm_named.original_name = Some("invoice.pdf".to_string());
            assert_eq!(
                super::effective_forward_attachment_name(None, Some(&mm_named)),
                Some("invoice.pdf")
            );
            assert_eq!(
                super::effective_forward_attachment_name(Some("explicit.png"), Some(&mm_named)),
                Some("explicit.png")
            );

            // Local vault lookup resolves directly by (thread_id, msg_id) without any CDN URL
            let resolved = super::resolve_local_vault_media_by_msg(
                &vault,
                &store,
                Some(("peer-42", "msg-local-1")),
            )
            .await
            .expect("local-vault-only media must resolve directly by msg_ref");
            assert_eq!(resolved.sha256, ingested.sha256);
            assert_eq!(resolved.size_bytes, png_payload.len() as u64);

            let exported_path = dir.join("forwarded_local.png");
            vault
                .export_to_path(&resolved, &exported_path)
                .await
                .unwrap();
            assert_eq!(std::fs::read(&exported_path).unwrap(), png_payload);

            // Missing blob on disk must return None cleanly (so caller falls back to GDrive/error)
            if let Some(rel) = resolved.local_rel_path.as_deref() {
                let blob_path = vault.resolve_blob_path(rel).unwrap();
                std::fs::remove_file(&blob_path).unwrap();
            }
            assert!(super::resolve_local_vault_media_by_msg(
                &vault,
                &store,
                Some(("peer-42", "msg-local-1")),
            )
            .await
            .is_none());
        });

        drop(vault);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ws_message_fallback_title_rejects_raw_and_suffixed_uids_while_preserving_real_names() {
        let mk = |thread_id: &str, sender_id: &str, sender_name: &str, is_outgoing: bool| {
            zalo_core::chat::MessageDto {
                msg_id: "m1".into(),
                cli_msg_id: None,
                thread_id: thread_id.into(),
                sender_id: sender_id.into(),
                sender_name: sender_name.into(),
                text: "hi".into(),
                msg_type: "chat.text".into(),
                ts: 1_700_000_000_000,
                is_outgoing,
                is_recalled: false,
                is_pinned: false,
                quote: None,
                reactions: Vec::new(),
                attachment_url: None,
                attachment_name: None,
            }
        };

        // _0-suffixed and raw numeric UIDs must NEVER leak as thread fallback title
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001_0", "777001_0", false)),
            None
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "999999", "777001_0", false)),
            None
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001", "777001", false)),
            None
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("peer-abc", "peer-abc", "peer-abc", false)),
            None
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "me", "Alice", true)),
            None
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001", "   ", false)),
            None
        );

        // Real names (including underscores or mixed digits+letters) must pass through
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001_0", "minh_anh", false)),
            Some("minh_anh")
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001_0", "user_123", false)),
            Some("user_123")
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001_0", "2025_team", false)),
            Some("2025_team")
        );
        assert_eq!(
            super::ws_message_fallback_title(&mk("777001", "777001_0", "Nguyễn Văn A", false)),
            Some("Nguyễn Văn A")
        );
    }
}
