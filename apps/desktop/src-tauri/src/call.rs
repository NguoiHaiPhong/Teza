//! Tauri v2 bridge manager for 1-1 direct-network VoIP calling powered by `teza-call`.
//!
//! Enforces strict Relay exclusion (calling is only active when `session.relay.enabled == false`),
//! manages live WS + `lp_control` signaling intake, forwards call lifecycle events and throttled
//! audio telemetry to the React UI, and persists preferred microphone/speaker selections locally.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Mutex, MutexGuard};
use tokio::task::JoinHandle;

use teza_call::{
    list_input_devices, list_output_devices, poll_voip_controls, AudioDeviceInfo,
    AudioDeviceSelection, AudioLoopbackSession, CallEndReason, CallEngine, CallEngineConfig,
    CallError, CallEvent, CallPhase, CallType, CtrlPollState,
};
use teza_video::{InboundVideoFrame, VideoMediaHook, VideoSessionConfig};
use zalo_core::chat::parser::VoipControlEvent;
use zalo_core::data_dir::{write_private_atomic, StoragePaths};
use zalo_core::session::Session;
use zalo_core::store::MessageStore;

pub const MAX_OUTBOUND_VIDEO_AU_BYTES: usize = 512 * 1024;
const MAX_OUTBOUND_VIDEO_AU_B64_LEN: usize = MAX_OUTBOUND_VIDEO_AU_BYTES.div_ceil(3) * 4;
const INBOUND_VIDEO_FRAME_QUEUE_CAP: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallDevicesDto {
    pub inputs: Vec<AudioDeviceInfo>,
    pub outputs: Vec<AudioDeviceInfo>,
    pub selected_input: Option<String>,
    pub selected_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallSnapshotDto {
    pub call_id: i64,
    pub partner_id: String,
    pub partner_name: String,
    pub partner_avatar: String,
    pub call_type: CallType,
    pub is_caller: bool,
    pub phase: CallPhase,
    pub mic_muted: bool,
    pub peer_mic_muted: bool,
    #[serde(default)]
    pub cam_enabled: bool,
    #[serde(default)]
    pub peer_cam_enabled: bool,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallStateDto {
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub phase: CallPhase,
    pub active: Option<CallSnapshotDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallEventEnvelopeDto {
    pub event: CallEvent,
    pub state: CallStateDto,
    pub partner_name: Option<String>,
    pub partner_avatar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallTelemetryDto {
    pub call_id: i64,
    pub duration_secs: u64,
    pub mic_muted: bool,
    pub has_input_device: bool,
    pub mic_level_permille: u32,
    pub remote_level_permille: u32,
    pub remote_speaking: bool,
    pub tx_packets: u64,
    pub rx_packets: u64,
    #[serde(default)]
    pub active_input_device: Option<String>,
    #[serde(default)]
    pub active_output_device: Option<String>,
    #[serde(default)]
    pub input_is_virtual: bool,
    #[serde(default)]
    pub output_is_virtual: bool,
    #[serde(default)]
    pub tx_errors: u64,
    #[serde(default)]
    pub video_cam_enabled: bool,
    #[serde(default)]
    pub video_peer_active: bool,
    #[serde(default)]
    pub video_synced: bool,
    #[serde(default)]
    pub video_tx_frames: u64,
    #[serde(default)]
    pub video_rx_frames: u64,
    #[serde(default)]
    pub video_rx_dropped_incomplete: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallVideoFrameDto {
    pub call_id: i64,
    pub ssrc: u32,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub is_keyframe: bool,
    pub just_synced: bool,
    pub data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallVideoKeyframeRequestDto {
    pub call_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallAudioTestResultDto {
    pub output_device: Option<String>,
    pub output_is_virtual: bool,
    pub input_device: Option<String>,
    pub input_is_virtual: bool,
    pub has_input_device: bool,
    pub tx_packets: u64,
    pub rx_packets: u64,
}

pub fn normalize_device_name(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn load_saved_devices(path: &Path) -> AudioDeviceSelection {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return AudioDeviceSelection::default();
    };
    let Ok(parsed) = serde_json::from_str::<AudioDeviceSelection>(&raw) else {
        return AudioDeviceSelection::default();
    };
    AudioDeviceSelection {
        input_device_name: normalize_device_name(parsed.input_device_name),
        output_device_name: normalize_device_name(parsed.output_device_name),
    }
}

pub fn save_devices_file(path: &Path, sel: &AudioDeviceSelection) -> Result<(), String> {
    let normalized = AudioDeviceSelection {
        input_device_name: normalize_device_name(sel.input_device_name.clone()),
        output_device_name: normalize_device_name(sel.output_device_name.clone()),
    };
    let json = serde_json::to_vec_pretty(&normalized)
        .map_err(|e| format!("Không tuần tự hóa được cấu hình thiết bị âm thanh: {e}"))?;
    write_private_atomic(path, &json)
        .map_err(|e| format!("Không lưu được cấu hình thiết bị âm thanh: {e}"))
}

pub fn build_unavailable_state(authed: bool, relay_enabled: bool) -> CallStateDto {
    let unavailable_reason = if !authed {
        Some("not_authenticated".to_string())
    } else if relay_enabled {
        Some("relay_enabled".to_string())
    } else {
        None
    };
    CallStateDto {
        available: authed && !relay_enabled,
        unavailable_reason,
        phase: CallPhase::Idle,
        active: None,
    }
}

fn pick_partner_display(
    partner_id: &str,
    fallback_name: &str,
    profiles: &HashMap<String, (String, String)>,
) -> (String, String) {
    let clean_fallback = fallback_name.trim();
    let fallback_is_generic = clean_fallback.is_empty()
        || clean_fallback == partner_id
        || clean_fallback == "Người dùng"
        || clean_fallback.chars().all(|c| c.is_ascii_digit());

    if let Some((stored_name, stored_avatar)) = profiles.get(partner_id) {
        let clean_stored = stored_name.trim();
        let name = if !clean_stored.is_empty() && (fallback_is_generic || clean_stored != partner_id)
        {
            clean_stored.to_string()
        } else if !clean_fallback.is_empty() {
            clean_fallback.to_string()
        } else {
            partner_id.to_string()
        };
        (name, stored_avatar.clone())
    } else {
        let name = if !clean_fallback.is_empty() {
            clean_fallback.to_string()
        } else {
            partner_id.to_string()
        };
        (name, String::new())
    }
}

async fn resolve_profiles_snapshot(
    store_arc: &Arc<Mutex<MessageStore>>,
) -> HashMap<String, (String, String)> {
    let store = store_arc.lock().await;
    store.resolve_known_profiles().unwrap_or_default()
}

async fn build_state_from_engine(
    engine: Option<&Arc<CallEngine>>,
    authed: bool,
    relay_enabled: bool,
    store_arc: &Arc<Mutex<MessageStore>>,
) -> CallStateDto {
    if !authed || relay_enabled {
        return build_unavailable_state(authed, relay_enabled);
    }
    let Some(eng) = engine else {
        return build_unavailable_state(authed, relay_enabled);
    };
    let Some(snap) = eng.snapshot().await else {
        return CallStateDto {
            available: true,
            unavailable_reason: None,
            phase: CallPhase::Idle,
            active: None,
        };
    };
    let profiles = resolve_profiles_snapshot(store_arc).await;
    let (partner_name, partner_avatar) =
        pick_partner_display(&snap.partner_id, &snap.partner_name, &profiles);
    CallStateDto {
        available: true,
        unavailable_reason: None,
        phase: snap.phase,
        active: Some(CallSnapshotDto {
            call_id: snap.call_id,
            partner_id: snap.partner_id,
            partner_name,
            partner_avatar,
            call_type: snap.call_type,
            is_caller: snap.is_caller,
            phase: snap.phase,
            mic_muted: snap.mic_muted,
            peer_mic_muted: snap.peer_mic_muted,
            cam_enabled: snap.cam_enabled,
            peer_cam_enabled: snap.peer_cam_enabled,
            duration_secs: snap.duration_secs,
        }),
    }
}

fn decode_outbound_video_au(data_b64: &str) -> Result<Vec<u8>, String> {
    let trimmed = data_b64.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.len() > MAX_OUTBOUND_VIDEO_AU_B64_LEN {
        return Err(format!(
            "Khung hình video vượt quá giới hạn kích thước ({} KiB)",
            MAX_OUTBOUND_VIDEO_AU_BYTES / 1024
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| format!("Dữ liệu khung hình video base64 không hợp lệ: {e}"))?;
    if bytes.len() > MAX_OUTBOUND_VIDEO_AU_BYTES {
        return Err(format!(
            "Khung hình video vượt quá giới hạn kích thước ({} KiB)",
            MAX_OUTBOUND_VIDEO_AU_BYTES / 1024
        ));
    }
    Ok(bytes)
}

pub struct CallManager {
    lifecycle: Arc<Mutex<()>>,
    engine: Arc<Mutex<Option<Arc<CallEngine>>>>,
    video_hook: Arc<Mutex<Option<Arc<VideoMediaHook>>>>,
    forwarder_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    video_frame_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    telemetry_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    lp_poll_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    devices: Arc<Mutex<AudioDeviceSelection>>,
    paths: StoragePaths,
}

impl CallManager {
    pub fn new(paths: StoragePaths) -> Self {
        let initial_devices = load_saved_devices(&paths.call_audio_file());
        Self {
            lifecycle: Arc::new(Mutex::new(())),
            engine: Arc::new(Mutex::new(None)),
            video_hook: Arc::new(Mutex::new(None)),
            forwarder_task: Arc::new(Mutex::new(None)),
            video_frame_task: Arc::new(Mutex::new(None)),
            telemetry_task: Arc::new(Mutex::new(None)),
            lp_poll_task: Arc::new(Mutex::new(None)),
            devices: Arc::new(Mutex::new(initial_devices)),
            paths,
        }
    }

    pub async fn active_engine(&self) -> Option<Arc<CallEngine>> {
        self.engine.lock().await.clone()
    }

    pub async fn get_state(
        &self,
        authed: bool,
        relay_enabled: bool,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> CallStateDto {
        let eng = self.active_engine().await;
        build_state_from_engine(eng.as_ref(), authed, relay_enabled, store_arc).await
    }

    pub async fn feed_ws_voip(&self, ev: &VoipControlEvent) {
        if let Some(eng) = self.active_engine().await {
            let _ = eng.handle_voip_event(ev).await;
        }
    }

    async fn teardown_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        app: Option<&AppHandle>,
        authed: bool,
        relay_enabled: bool,
    ) {
        if let Some(h) = self.lp_poll_task.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.telemetry_task.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.video_frame_task.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.forwarder_task.lock().await.take() {
            h.abort();
        }

        let old_engine = self.engine.lock().await.take();
        let mut ended_envelope_info: Option<(i64, String, u64, CallEndReason)> = None;
        if let Some(eng) = old_engine {
            let prior_snap = eng.snapshot().await;
            let stop_reason = if relay_enabled {
                CallEndReason::Error(
                    "Đã bật chế độ Relay — cuộc gọi trực tiếp đã dừng".to_string(),
                )
            } else {
                CallEndReason::LocalHangup
            };
            if let Some(snap) = prior_snap {
                ended_envelope_info = Some((
                    snap.call_id,
                    snap.partner_id,
                    snap.duration_secs,
                    stop_reason.clone(),
                ));
            }
            eng.force_stop(Some(stop_reason)).await;
        }
        if let Some(vh) = self.video_hook.lock().await.take() {
            vh.stop_active();
        }

        if let Some(app_handle) = app {
            let unavailable = build_unavailable_state(authed, relay_enabled);
            if relay_enabled {
                if let Some((call_id, partner_id, duration_secs, reason)) = ended_envelope_info {
                    let _ = app_handle.emit(
                        "core://call-event",
                        CallEventEnvelopeDto {
                            event: CallEvent::CallEnded {
                                call_id,
                                partner_id,
                                duration_secs,
                                reason,
                            },
                            state: unavailable.clone(),
                            partner_name: None,
                            partner_avatar: None,
                        },
                    );
                }
            }
            let _ = app_handle.emit("core://call-availability", unavailable);
        }
    }

    async fn ensure_started_locked(
        &self,
        guard: &MutexGuard<'_, ()>,
        app: &AppHandle,
        session: &Session,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        if !session.is_authenticated() || session.relay.enabled {
            self.teardown_locked(
                guard,
                Some(app),
                session.is_authenticated(),
                session.relay.enabled,
            )
            .await;
            return Ok(());
        }
        if self.active_engine().await.is_none() {
            return self.restart_locked(guard, app, session, store_arc).await;
        }
        Ok(())
    }

    async fn restart_locked(
        &self,
        guard: &MutexGuard<'_, ()>,
        app: &AppHandle,
        session: &Session,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        if !session.is_authenticated() || session.relay.enabled {
            self.teardown_locked(
                guard,
                Some(app),
                session.is_authenticated(),
                session.relay.enabled,
            )
            .await;
            return Ok(());
        }

        self.teardown_locked(guard, None, true, false).await;

        let selected_devices = self.devices.lock().await.clone();
        let (video_frame_tx, mut video_frame_rx) =
            mpsc::channel::<InboundVideoFrame>(INBOUND_VIDEO_FRAME_QUEUE_CAP);
        let app_kf = app.clone();
        let video_hook = Arc::new_cyclic(|weak_hook: &Weak<VideoMediaHook>| {
            let weak_cb = weak_hook.clone();
            VideoMediaHook::new(VideoSessionConfig {
                send_synthetic_when_no_external_frames: false,
                on_inbound_frame: Some(Arc::new(move |frame: InboundVideoFrame| {
                    // Non-blocking try_send so UDP recv thread never blocks on UI/IPC backpressure.
                    // On overflow, immediately request a remote keyframe (RTCP PLI) so the decoder
                    // recovers cleanly instead of stalling on a broken reference chain.
                    if let Err(mpsc::error::TrySendError::Full(dropped)) =
                        video_frame_tx.try_send(frame)
                    {
                        if let Some(hook) = weak_cb.upgrade() {
                            hook.request_remote_keyframe(Some(dropped.call_id));
                        }
                    }
                })),
                on_keyframe_requested: Some(Arc::new(move |call_id: i64| {
                    let _ = app_kf.emit(
                        "core://call-video-keyframe-request",
                        CallVideoKeyframeRequestDto { call_id },
                    );
                })),
                ..Default::default()
            })
        });

        let eng = Arc::new(
            CallEngine::new(
                session.clone_shallow(),
                CallEngineConfig {
                    enable_audio: true,
                    devices: selected_devices,
                    trace: None,
                },
            )
            .map_err(|e| e.to_string())?
            .with_aux_media_hook(video_hook.clone()),
        );
        let mut rx = eng.subscribe();
        if let Some(old_eng) = self.engine.lock().await.replace(eng.clone()) {
            old_eng.force_stop(None).await;
        }
        if let Some(old_vh) = self.video_hook.lock().await.replace(video_hook.clone()) {
            old_vh.stop_active();
        }

        // 0. Forward reassembled inbound H.264 Annex-B frames to UI without blocking UDP recv
        let app_vf = app.clone();
        let vf_handle = tokio::spawn(async move {
            while let Some(frame) = video_frame_rx.recv().await {
                let data_b64 = base64::engine::general_purpose::STANDARD.encode(&frame.annexb_au);
                let _ = app_vf.emit(
                    "core://call-video-frame",
                    CallVideoFrameDto {
                        call_id: frame.call_id,
                        ssrc: frame.ssrc,
                        sequence_number: frame.sequence_number,
                        timestamp: frame.timestamp,
                        is_keyframe: frame.is_keyframe,
                        just_synced: frame.just_synced,
                        data_b64,
                    },
                );
            }
        });
        if let Some(old) = self.video_frame_task.lock().await.replace(vf_handle) {
            old.abort();
        }

        // 1. Forward `CallEvent` stream to UI with enriched CallState snapshot & partner metadata
        let app_ev = app.clone();
        let eng_ev = eng.clone();
        let store_ev = store_arc.clone();
        let fwd_handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let profiles = resolve_profiles_snapshot(&store_ev).await;
                        let (partner_name, partner_avatar) = match &ev {
                            CallEvent::IncomingCall {
                                caller_id,
                                caller_name,
                                ..
                            } => {
                                let (n, a) =
                                    pick_partner_display(caller_id, caller_name, &profiles);
                                (Some(n), (!a.is_empty()).then_some(a))
                            }
                            CallEvent::Connected { partner_id, .. }
                            | CallEvent::CallEnded { partner_id, .. } => {
                                let (n, a) = pick_partner_display(partner_id, "", &profiles);
                                (Some(n), (!a.is_empty()).then_some(a))
                            }
                            _ => (None, None),
                        };

                        if matches!(ev, CallEvent::IncomingCall { .. }) {
                            if let Some(win) = app_ev.get_window("main") {
                                if matches!(win.is_visible(), Ok(false)) {
                                    let _ = win.show();
                                    let _ = win.unminimize();
                                }
                                let _ = win.request_user_attention(Some(
                                    tauri::UserAttentionType::Critical,
                                ));
                            }
                        }

                        match &ev {
                            CallEvent::Connected {
                                call_id, rtp_ip, ..
                            } => {
                                eprintln!(
                                    "[teza][call] connected call_id={call_id} rtp={rtp_ip}"
                                );
                            }
                            CallEvent::MediaWarning { call_id, message } => {
                                eprintln!(
                                    "[teza][call] media_warning call_id={call_id}: {message}"
                                );
                            }
                            _ => {}
                        }

                        let state = build_state_from_engine(Some(&eng_ev), true, false, &store_ev)
                            .await;
                        let _ = app_ev.emit(
                            "core://call-event",
                            CallEventEnvelopeDto {
                                event: ev,
                                state,
                                partner_name,
                                partner_avatar,
                            },
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        if let Some(old) = self.forwarder_task.lock().await.replace(fwd_handle) {
            old.abort();
        }

        // 2. Throttled live audio + video telemetry emitter while Connected
        let app_tel = app.clone();
        let eng_tel = eng.clone();
        let video_tel = video_hook.clone();
        let tel_handle = tokio::spawn(async move {
            loop {
                if let Some(snap) = eng_tel.snapshot().await {
                    if snap.phase == CallPhase::Connected {
                        let audio_t = eng_tel.audio_telemetry().await;
                        let video_t = if snap.call_type == CallType::DirectVideo {
                            video_tel.telemetry_for_call(snap.call_id)
                        } else {
                            None
                        };
                        if audio_t.is_some() || video_t.is_some() {
                            let t = audio_t.unwrap_or_default();
                            let vt = video_t.unwrap_or_default();
                            let _ = app_tel.emit(
                                "core://call-telemetry",
                                CallTelemetryDto {
                                    call_id: snap.call_id,
                                    duration_secs: snap.duration_secs,
                                    mic_muted: snap.mic_muted,
                                    has_input_device: t.has_input_device,
                                    mic_level_permille: t.mic_level_permille,
                                    remote_level_permille: t.remote_level_permille,
                                    remote_speaking: t.remote_speaking,
                                    tx_packets: t.tx_packets,
                                    rx_packets: t.rx_packets,
                                    active_input_device: t.active_input_device,
                                    active_output_device: t.active_output_device,
                                    input_is_virtual: t.input_is_virtual,
                                    output_is_virtual: t.output_is_virtual,
                                    tx_errors: t.tx_errors,
                                    video_cam_enabled: vt.cam_enabled,
                                    video_peer_active: vt.peer_video_active,
                                    video_synced: vt.video_synced,
                                    video_tx_frames: vt.tx_frames,
                                    video_rx_frames: vt.rx_frames,
                                    video_rx_dropped_incomplete: vt.rx_dropped_incomplete,
                                },
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        if let Some(old) = self.telemetry_task.lock().await.replace(tel_handle) {
            old.abort();
        }

        // 3. Resilient `lp_control` HTTP long-poll backup (direct network only; catches invites if WS reconnects)
        let sess_poll = session.clone_shallow();
        let eng_poll = eng.clone();
        let poll_handle = tokio::spawn(async move {
            let mut poll_state = CtrlPollState::new_live_only();
            let mut err_backoff_secs = 2u64;
            if sess_poll.relay.enabled {
                return;
            }
            loop {
                match poll_voip_controls(&sess_poll, &mut poll_state).await {
                    Ok(events) => {
                        err_backoff_secs = 2;
                        for ev in events {
                            let _ = eng_poll.handle_voip_event(&ev).await;
                        }
                        let pause_ms = if poll_state.last_more { 50 } else { 200 };
                        tokio::time::sleep(Duration::from_millis(pause_ms)).await;
                    }
                    Err(e) if e.is_timeout() => {
                        err_backoff_secs = 2;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(CallError::RelayUnsupported) | Err(CallError::SessionExpired { .. }) => {
                        break;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_secs(err_backoff_secs)).await;
                        err_backoff_secs = (err_backoff_secs * 2).min(15);
                    }
                }
            }
        });
        if let Some(old) = self.lp_poll_task.lock().await.replace(poll_handle) {
            old.abort();
        }

        let initial_state = build_state_from_engine(Some(&eng), true, false, store_arc).await;
        let _ = app.emit("core://call-availability", initial_state);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn teardown(&self, app: Option<&AppHandle>, authed: bool, relay_enabled: bool) {
        let guard = self.lifecycle.lock().await;
        self.teardown_locked(&guard, app, authed, relay_enabled)
            .await;
    }

    pub async fn teardown_for_session(
        &self,
        app: Option<&AppHandle>,
        session_arc: &Arc<Mutex<Session>>,
    ) {
        let guard = self.lifecycle.lock().await;
        let (authed, relay_enabled) = {
            let sess = session_arc.lock().await;
            (sess.is_authenticated(), sess.relay.enabled)
        };
        self.teardown_locked(&guard, app, authed, relay_enabled)
            .await;
    }

    #[allow(dead_code)]
    pub async fn ensure_started(
        &self,
        app: AppHandle,
        session: &Session,
        store_arc: Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        let guard = self.lifecycle.lock().await;
        self.ensure_started_locked(&guard, &app, session, &store_arc)
            .await
    }

    pub async fn ensure_started_from_session(
        &self,
        app: &AppHandle,
        session_arc: &Arc<Mutex<Session>>,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> CallStateDto {
        let guard = self.lifecycle.lock().await;
        let (authed, relay_enabled, sess_snap) = {
            let sess = session_arc.lock().await;
            let authed = sess.is_authenticated();
            let relay_enabled = sess.relay.enabled;
            let snap = (authed && !relay_enabled).then(|| sess.clone_shallow());
            (authed, relay_enabled, snap)
        };
        if let Some(snap) = sess_snap {
            if let Err(err) = self
                .ensure_started_locked(&guard, app, &snap, store_arc)
                .await
            {
                eprintln!("[teza][call] ensure_started_from_session error: {err}");
            }
            let eng = self.active_engine().await;
            build_state_from_engine(eng.as_ref(), true, false, store_arc).await
        } else {
            self.teardown_locked(&guard, Some(app), authed, relay_enabled)
                .await;
            build_unavailable_state(authed, relay_enabled)
        }
    }

    #[allow(dead_code)]
    pub async fn restart(
        &self,
        app: AppHandle,
        session: &Session,
        store_arc: Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        let guard = self.lifecycle.lock().await;
        self.restart_locked(&guard, &app, session, &store_arc).await
    }

    pub async fn restart_from_session(
        &self,
        app: &AppHandle,
        session_arc: &Arc<Mutex<Session>>,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> CallStateDto {
        let guard = self.lifecycle.lock().await;
        let (authed, relay_enabled, sess_snap) = {
            let sess = session_arc.lock().await;
            let authed = sess.is_authenticated();
            let relay_enabled = sess.relay.enabled;
            let snap = (authed && !relay_enabled).then(|| sess.clone_shallow());
            (authed, relay_enabled, snap)
        };
        if let Some(snap) = sess_snap {
            if let Err(err) = self.restart_locked(&guard, app, &snap, store_arc).await {
                eprintln!("[teza][call] restart_from_session error: {err}");
            }
            let eng = self.active_engine().await;
            build_state_from_engine(eng.as_ref(), true, false, store_arc).await
        } else {
            self.teardown_locked(&guard, Some(app), authed, relay_enabled)
                .await;
            build_unavailable_state(authed, relay_enabled)
        }
    }

    pub async fn start_outgoing_call_for_session(
        &self,
        app: &AppHandle,
        session_arc: &Arc<Mutex<Session>>,
        store_arc: &Arc<Mutex<MessageStore>>,
        partner_id: String,
        partner_name: Option<String>,
        video: bool,
    ) -> Result<CallStateDto, String> {
        let guard = self.lifecycle.lock().await;
        let sess_snap = {
            let sess = session_arc.lock().await;
            let authed = sess.is_authenticated();
            let relay_enabled = sess.relay.enabled;
            if !authed || relay_enabled {
                drop(sess);
                self.teardown_locked(&guard, Some(app), authed, relay_enabled)
                    .await;
                return Err(if !authed {
                    "Chưa đăng nhập".into()
                } else {
                    CallError::RelayUnsupported.to_string()
                });
            }
            sess.clone_shallow()
        };

        self.ensure_started_locked(&guard, app, &sess_snap, store_arc)
            .await?;
        let eng = self
            .active_engine()
            .await
            .ok_or_else(|| "Engine gọi thoại chưa sẵn sàng".to_string())?;
        let call_type = CallType::from_video_flag(video);
        let display_name = partner_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| partner_id.clone());
        eng.make_call(partner_id, display_name, call_type)
            .await
            .map_err(|e| e.to_string())?;

        // Re-verify session auth & relay state before returning in case relay was enabled mid-dial.
        let (authed_after, relay_after) = {
            let sess = session_arc.lock().await;
            (sess.is_authenticated(), sess.relay.enabled)
        };
        if !authed_after || relay_after {
            self.teardown_locked(&guard, Some(app), authed_after, relay_after)
                .await;
            return Err(if !authed_after {
                "Chưa đăng nhập".into()
            } else {
                CallError::RelayUnsupported.to_string()
            });
        }
        Ok(build_state_from_engine(Some(&eng), true, false, store_arc).await)
    }

    pub async fn list_devices(&self) -> Result<CallDevicesDto, String> {
        let (inputs, outputs) = tokio::task::spawn_blocking(|| {
            let ins = list_input_devices().map_err(|e| e.to_string())?;
            let outs = list_output_devices().map_err(|e| e.to_string())?;
            Ok::<_, String>((ins, outs))
        })
        .await
        .map_err(|e| format!("Lỗi tác vụ liệt kê thiết bị âm thanh: {e}"))??;

        let sel = self.devices.lock().await.clone();
        Ok(CallDevicesDto {
            inputs,
            outputs,
            selected_input: sel.input_device_name,
            selected_output: sel.output_device_name,
        })
    }

    pub async fn set_devices(
        &self,
        input_device: Option<String>,
        output_device: Option<String>,
    ) -> Result<CallDevicesDto, String> {
        let next = AudioDeviceSelection {
            input_device_name: normalize_device_name(input_device),
            output_device_name: normalize_device_name(output_device),
        };
        save_devices_file(&self.paths.call_audio_file(), &next)?;
        *self.devices.lock().await = next.clone();
        if let Some(eng) = self.active_engine().await {
            eng.set_devices(next).await;
        }
        self.list_devices().await
    }

    pub async fn push_outbound_video_frame_for_session(
        &self,
        session_arc: &Arc<Mutex<Session>>,
        call_id: i64,
        data_b64: &str,
    ) -> Result<bool, String> {
        let allowed = {
            let sess = session_arc.lock().await;
            sess.is_authenticated() && !sess.relay.enabled
        };
        if !allowed {
            return Ok(false);
        }
        self.push_outbound_video_frame(call_id, data_b64).await
    }

    pub async fn request_remote_video_keyframe(&self, call_id: Option<i64>) -> bool {
        let Some(hook) = self.video_hook.lock().await.clone() else {
            return false;
        };
        hook.request_remote_keyframe(call_id)
    }

    pub async fn push_outbound_video_frame(
        &self,
        call_id: i64,
        data_b64: &str,
    ) -> Result<bool, String> {
        let bytes = decode_outbound_video_au(data_b64)?;
        if bytes.is_empty() {
            return Ok(false);
        }
        let Some(eng) = self.active_engine().await else {
            return Ok(false);
        };
        let Some(snap) = eng.snapshot().await else {
            return Ok(false);
        };
        if snap.call_type != CallType::DirectVideo
            || !matches!(snap.phase, CallPhase::Connecting | CallPhase::Connected)
        {
            return Ok(false);
        }
        if call_id > 0 && snap.call_id != call_id {
            return Ok(false);
        }
        let Some(hook) = self.video_hook.lock().await.clone() else {
            return Ok(false);
        };
        Ok(hook.push_outbound_au_for_call(snap.call_id, &bytes))
    }

    pub async fn test_audio(&self) -> Result<CallAudioTestResultDto, String> {
        if let Some(eng) = self.active_engine().await {
            if eng.phase().await != CallPhase::Idle {
                return Err("Không thể kiểm tra loa khi đang trong cuộc gọi".to_string());
            }
        }
        let sel = self.devices.lock().await.clone();
        tokio::task::spawn_blocking(move || {
            let sess = AudioLoopbackSession::start_with_tone(sel, 0, Some(440.0))
                .map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(1100));
            let t = sess.telemetry();
            sess.stop();
            Ok::<_, String>(CallAudioTestResultDto {
                output_device: t.active_output_device,
                output_is_virtual: t.output_is_virtual,
                input_device: t.active_input_device,
                input_is_virtual: t.input_is_virtual,
                has_input_device: t.has_input_device,
                tx_packets: t.tx_packets,
                rx_packets: t.rx_packets,
            })
        })
        .await
        .map_err(|e| format!("Lỗi tác vụ kiểm tra âm thanh: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn unavailable_state_enforces_auth_and_relay_priority() {
        let not_authed = build_unavailable_state(false, true);
        assert!(!not_authed.available);
        assert_eq!(
            not_authed.unavailable_reason.as_deref(),
            Some("not_authenticated")
        );
        assert_eq!(not_authed.phase, CallPhase::Idle);

        let relay_on = build_unavailable_state(true, true);
        assert!(!relay_on.available);
        assert_eq!(relay_on.unavailable_reason.as_deref(), Some("relay_enabled"));

        let direct_ready = build_unavailable_state(true, false);
        assert!(direct_ready.available);
        assert!(direct_ready.unavailable_reason.is_none());
    }

    #[test]
    fn device_selection_normalizes_and_persists_roundtrip() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_call_audio_cfg_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("call_audio.json");

        assert_eq!(load_saved_devices(&file), AudioDeviceSelection::default());

        let sel = AudioDeviceSelection {
            input_device_name: Some("  USB Mic  ".to_string()),
            output_device_name: Some("   ".to_string()),
        };
        save_devices_file(&file, &sel).unwrap();
        let reloaded = load_saved_devices(&file);
        assert_eq!(
            reloaded,
            AudioDeviceSelection {
                input_device_name: Some("USB Mic".to_string()),
                output_device_name: None,
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn call_error_relay_unsupported_matches_contract() {
        let err = CallError::RelayUnsupported;
        assert!(err.to_string().contains("Relay"));
    }

    #[test]
    fn partner_display_prefers_stored_contact_over_generic_or_numeric_fallback() {
        let mut map = HashMap::new();
        map.insert(
            "2000".to_string(),
            ("Trang Mary".to_string(), "https://cdn.example/a.jpg".to_string()),
        );
        assert_eq!(
            pick_partner_display("2000", "Người dùng", &map),
            ("Trang Mary".to_string(), "https://cdn.example/a.jpg".to_string())
        );
        assert_eq!(
            pick_partner_display("2000", "2000", &map),
            ("Trang Mary".to_string(), "https://cdn.example/a.jpg".to_string())
        );
        assert_eq!(
            pick_partner_display("9999", "Unknown Peer", &map),
            ("Unknown Peer".to_string(), String::new())
        );
    }

    #[test]
    fn telemetry_and_audio_test_dtos_serialize_roundtrip() {
        let telem = CallTelemetryDto {
            call_id: 123,
            duration_secs: 45,
            mic_muted: false,
            has_input_device: true,
            mic_level_permille: 120,
            remote_level_permille: 340,
            remote_speaking: true,
            tx_packets: 200,
            rx_packets: 198,
            active_input_device: Some("CABLE Output (VB-Audio Virtual Cable)".to_string()),
            active_output_device: Some("Speakers (Realtek(R) Audio)".to_string()),
            input_is_virtual: true,
            output_is_virtual: false,
            tx_errors: 0,
            video_cam_enabled: true,
            video_peer_active: true,
            video_synced: true,
            video_tx_frames: 150,
            video_rx_frames: 148,
            video_rx_dropped_incomplete: 2,
        };
        let raw_t = serde_json::to_string(&telem).unwrap();
        let dec_t: CallTelemetryDto = serde_json::from_str(&raw_t).unwrap();
        assert_eq!(dec_t, telem);

        // Older telemetry JSON without video_* fields still deserializes with defaults.
        let legacy_json = serde_json::json!({
            "call_id": 9,
            "duration_secs": 3,
            "mic_muted": true,
            "has_input_device": false,
            "mic_level_permille": 0,
            "remote_level_permille": 10,
            "remote_speaking": false,
            "tx_packets": 1,
            "rx_packets": 2
        });
        let dec_legacy: CallTelemetryDto = serde_json::from_value(legacy_json).unwrap();
        assert!(!dec_legacy.video_cam_enabled);
        assert!(!dec_legacy.video_peer_active);
        assert!(!dec_legacy.video_synced);
        assert_eq!(dec_legacy.video_tx_frames, 0);
        assert_eq!(dec_legacy.video_rx_frames, 0);
        assert_eq!(dec_legacy.video_rx_dropped_incomplete, 0);

        let res = CallAudioTestResultDto {
            output_device: Some("CABLE Input (VB-Audio Virtual Cable)".to_string()),
            output_is_virtual: true,
            input_device: None,
            input_is_virtual: false,
            has_input_device: false,
            tx_packets: 50,
            rx_packets: 50,
        };
        let raw_r = serde_json::to_string(&res).unwrap();
        let dec_r: CallAudioTestResultDto = serde_json::from_str(&raw_r).unwrap();
        assert_eq!(dec_r, res);
    }

    #[test]
    fn call_snapshot_and_video_dtos_serialize_roundtrip() {
        let snap = CallSnapshotDto {
            call_id: 555_001,
            partner_id: "2000".to_string(),
            partner_name: "Video Peer".to_string(),
            partner_avatar: "https://cdn.example/avatar.jpg".to_string(),
            call_type: CallType::DirectVideo,
            is_caller: true,
            phase: CallPhase::Connected,
            mic_muted: false,
            peer_mic_muted: true,
            cam_enabled: true,
            peer_cam_enabled: false,
            duration_secs: 12,
        };
        let raw_snap = serde_json::to_string(&snap).unwrap();
        let dec_snap: CallSnapshotDto = serde_json::from_str(&raw_snap).unwrap();
        assert_eq!(dec_snap, snap);

        // Older snapshot JSON without cam_enabled/peer_cam_enabled defaults both to false.
        let legacy_snap = serde_json::json!({
            "call_id": 10,
            "partner_id": "2000",
            "partner_name": "Peer",
            "partner_avatar": "",
            "call_type": "DirectVoice",
            "is_caller": false,
            "phase": "connected",
            "mic_muted": false,
            "peer_mic_muted": false,
            "duration_secs": 1
        });
        let dec_legacy_snap: CallSnapshotDto = serde_json::from_value(legacy_snap).unwrap();
        assert!(!dec_legacy_snap.cam_enabled);
        assert!(!dec_legacy_snap.peer_cam_enabled);

        let frame = CallVideoFrameDto {
            call_id: 555_001,
            ssrc: 0x1122_3344,
            sequence_number: 42,
            timestamp: 90_000,
            is_keyframe: true,
            just_synced: true,
            data_b64: base64::engine::general_purpose::STANDARD.encode([0u8, 0, 0, 1, 0x65]),
        };
        let raw_frame = serde_json::to_string(&frame).unwrap();
        let dec_frame: CallVideoFrameDto = serde_json::from_str(&raw_frame).unwrap();
        assert_eq!(dec_frame, frame);

        let kf_req = CallVideoKeyframeRequestDto { call_id: 555_001 };
        let raw_kf = serde_json::to_string(&kf_req).unwrap();
        let dec_kf: CallVideoKeyframeRequestDto = serde_json::from_str(&raw_kf).unwrap();
        assert_eq!(dec_kf, kf_req);
    }

    #[test]
    fn push_outbound_video_frame_validates_base64_and_size_limit() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_call_video_mgr_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = CallManager::new(StoragePaths { root: dir.clone() });

        tauri::async_runtime::block_on(async {
            // Invalid base64 must return Err even when idle.
            let invalid = mgr.push_outbound_video_frame(123, "!!!not-base64!!!").await;
            assert!(invalid.is_err(), "expected invalid base64 error");

            // Exact 512 KiB decoded payload passes validation and returns Ok(false) when idle.
            let exact_limit = vec![0xABu8; MAX_OUTBOUND_VIDEO_AU_BYTES];
            let exact_b64 = base64::engine::general_purpose::STANDARD.encode(&exact_limit);
            assert_eq!(
                mgr.push_outbound_video_frame(123, &exact_b64).await,
                Ok(false)
            );

            // 512 KiB + 1 byte must be rejected.
            let over_by_one = vec![0xABu8; MAX_OUTBOUND_VIDEO_AU_BYTES + 1];
            let over_one_b64 = base64::engine::general_purpose::STANDARD.encode(&over_by_one);
            assert!(
                mgr.push_outbound_video_frame(123, &over_one_b64)
                    .await
                    .is_err(),
                "expected 512 KiB + 1 byte payload to be rejected"
            );

            // Significantly oversized payload is rejected before decoding.
            let huge_b64 = "A".repeat(MAX_OUTBOUND_VIDEO_AU_B64_LEN + 16);
            assert!(
                mgr.push_outbound_video_frame(123, &huge_b64).await.is_err(),
                "expected oversized base64 string to be rejected"
            );

            // Empty payload returns Ok(false).
            assert_eq!(mgr.push_outbound_video_frame(123, "   ").await, Ok(false));
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_aware_video_push_keyframe_request_and_serialized_teardown() {
        use std::net::UdpSocket;
        use teza_call::{AuxiliaryMediaHook, MediaTransportContext};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("teza_call_mgr_session_{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = Arc::new(CallManager::new(StoragePaths { root: dir.clone() }));

        tauri::async_runtime::block_on(async {
            // 1. Unauthenticated session rejects push_outbound_video_frame_for_session cleanly
            let unauth_sess = Arc::new(Mutex::new(Session::new().unwrap()));
            assert_eq!(
                mgr.push_outbound_video_frame_for_session(&unauth_sess, 123, "!!!bad!!!")
                    .await,
                Ok(false),
                "unauthenticated session must short-circuit to Ok(false)"
            );

            // 2. Authenticated session with relay enabled also rejects video frame pushes
            let mut relay_sess = Session::new().unwrap();
            relay_sess.zpw_sek = Some("test_zpw_sek".to_string());
            relay_sess.secret_key = Some(vec![0x11u8; 32]);
            relay_sess.profile.uid = "1000".to_string();
            relay_sess.relay.enabled = true;
            assert!(relay_sess.is_authenticated());
            let relay_sess_arc = Arc::new(Mutex::new(relay_sess));
            assert_eq!(
                mgr.push_outbound_video_frame_for_session(&relay_sess_arc, 123, "!!!bad!!!")
                    .await,
                Ok(false),
                "relay-enabled session must short-circuit to Ok(false)"
            );

            // 3. Direct authenticated session proceeds into validation (rejecting invalid base64)
            let mut direct_sess = Session::new().unwrap();
            direct_sess.zpw_sek = Some("test_zpw_sek".to_string());
            direct_sess.secret_key = Some(vec![0x11u8; 32]);
            direct_sess.profile.uid = "1000".to_string();
            direct_sess.relay.enabled = false;
            assert!(direct_sess.is_authenticated());
            let direct_sess_arc = Arc::new(Mutex::new(direct_sess));
            assert!(
                mgr.push_outbound_video_frame_for_session(&direct_sess_arc, 123, "!!!bad!!!")
                    .await
                    .is_err(),
                "authenticated direct session must validate base64 payload"
            );

            // 4. request_remote_video_keyframe routes through active VideoMediaHook by call_id
            assert!(!mgr.request_remote_video_keyframe(None).await);
            let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
            let addr = sock.local_addr().unwrap();
            let hook = Arc::new(VideoMediaHook::new(VideoSessionConfig {
                send_synthetic_when_no_external_frames: false,
                ..Default::default()
            }));
            let _aux = hook
                .on_media_start(MediaTransportContext {
                    call_id: 777,
                    call_type: CallType::DirectVideo,
                    is_caller: true,
                    from_uid_u32: 1001,
                    partner_uid_u32: 2002,
                    ssrc: 1001,
                    rtp_target: addr,
                    rtcp_target: None,
                    relay_token: 0,
                    live_relay_token: None,
                    live_rtcp_relay_token: None,
                    udp_rtp_sock: sock,
                    udp_rtcp_sock: None,
                    initial_cam_enabled: false,
                })
                .expect("video session should start");
            *mgr.video_hook.lock().await = Some(hook);

            assert!(!mgr.request_remote_video_keyframe(Some(888)).await);
            assert!(mgr.request_remote_video_keyframe(Some(777)).await);
            assert!(mgr.request_remote_video_keyframe(None).await);

            // 5. teardown_for_session serializes on lifecycle lock and aborts all task handles
            let h1 = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
            let h2 = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
            let h3 = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
            let h4 = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
            *mgr.lp_poll_task.lock().await = Some(h1);
            *mgr.telemetry_task.lock().await = Some(h2);
            *mgr.video_frame_task.lock().await = Some(h3);
            *mgr.forwarder_task.lock().await = Some(h4);

            // Hold lifecycle lock briefly to verify teardown_for_session waits on it
            let held = mgr.lifecycle.lock().await;
            let mgr_td = mgr.clone();
            let sess_td = relay_sess_arc.clone();
            let td_handle = tokio::spawn(async move {
                mgr_td.teardown_for_session(None, &sess_td).await;
            });
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert!(
                !td_handle.is_finished(),
                "teardown_for_session must serialize on self.lifecycle"
            );
            drop(held);
            td_handle.await.unwrap();

            assert!(mgr.lp_poll_task.lock().await.is_none());
            assert!(mgr.telemetry_task.lock().await.is_none());
            assert!(mgr.video_frame_task.lock().await.is_none());
            assert!(mgr.forwarder_task.lock().await.is_none());
            assert!(!mgr.request_remote_video_keyframe(Some(777)).await);
        });

        let _ = std::fs::remove_dir_all(&dir);
    }
}
