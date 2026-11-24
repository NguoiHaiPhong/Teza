//! Tauri v2 bridge manager for 1-1 direct-network VoIP calling powered by `teza-call`.
//!
//! Enforces strict Relay exclusion (calling is only active when `session.relay.enabled == false`),
//! manages live WS + `lp_control` signaling intake, forwards call lifecycle events and throttled
//! audio telemetry to the React UI, and persists preferred microphone/speaker selections locally.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use teza_call::{
    list_input_devices, list_output_devices, poll_voip_controls, AudioDeviceInfo,
    AudioDeviceSelection, AudioLoopbackSession, CallEndReason, CallEngine, CallEngineConfig,
    CallError, CallEvent, CallPhase, CallType, CtrlPollState,
};
use zalo_core::chat::parser::VoipControlEvent;
use zalo_core::data_dir::{write_private_atomic, StoragePaths};
use zalo_core::session::Session;
use zalo_core::store::MessageStore;

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
            duration_secs: snap.duration_secs,
        }),
    }
}

pub struct CallManager {
    engine: Arc<Mutex<Option<Arc<CallEngine>>>>,
    forwarder_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    telemetry_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    lp_poll_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    devices: Arc<Mutex<AudioDeviceSelection>>,
    paths: StoragePaths,
}

impl CallManager {
    pub fn new(paths: StoragePaths) -> Self {
        let initial_devices = load_saved_devices(&paths.call_audio_file());
        Self {
            engine: Arc::new(Mutex::new(None)),
            forwarder_task: Arc::new(Mutex::new(None)),
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

    pub async fn teardown(&self, app: Option<&AppHandle>, authed: bool, relay_enabled: bool) {
        if let Some(h) = self.lp_poll_task.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.telemetry_task.lock().await.take() {
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

    pub async fn ensure_started(
        &self,
        app: AppHandle,
        session: &Session,
        store_arc: Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        if !session.is_authenticated() || session.relay.enabled {
            self.teardown(
                Some(&app),
                session.is_authenticated(),
                session.relay.enabled,
            )
            .await;
            return Ok(());
        }
        if self.active_engine().await.is_none() {
            return self.restart(app, session, store_arc).await;
        }
        Ok(())
    }

    pub async fn restart(
        &self,
        app: AppHandle,
        session: &Session,
        store_arc: Arc<Mutex<MessageStore>>,
    ) -> Result<(), String> {
        self.teardown(None, session.is_authenticated(), session.relay.enabled)
            .await;

        if !session.is_authenticated() || session.relay.enabled {
            let _ = app.emit(
                "core://call-availability",
                build_unavailable_state(session.is_authenticated(), session.relay.enabled),
            );
            return Ok(());
        }

        let selected_devices = self.devices.lock().await.clone();
        let eng = Arc::new(
            CallEngine::new(
                session.clone_shallow(),
                CallEngineConfig {
                    enable_audio: true,
                    devices: selected_devices,
                    trace: None,
                },
            )
            .map_err(|e| e.to_string())?,
        );
        let mut rx = eng.subscribe();
        *self.engine.lock().await = Some(eng.clone());

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
                            if let Some(win) = app_ev.get_webview_window("main") {
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
        *self.forwarder_task.lock().await = Some(fwd_handle);

        // 2. Throttled live audio telemetry emitter while Connected
        let app_tel = app.clone();
        let eng_tel = eng.clone();
        let tel_handle = tokio::spawn(async move {
            loop {
                if let Some(snap) = eng_tel.snapshot().await {
                    if snap.phase == CallPhase::Connected {
                        if let Some(t) = eng_tel.audio_telemetry().await {
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
        *self.telemetry_task.lock().await = Some(tel_handle);

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
        *self.lp_poll_task.lock().await = Some(poll_handle);

        let initial_state = build_state_from_engine(Some(&eng), true, false, &store_arc).await;
        let _ = app.emit("core://call-availability", initial_state);
        Ok(())
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
        };
        let raw_t = serde_json::to_string(&telem).unwrap();
        let dec_t: CallTelemetryDto = serde_json::from_str(&raw_t).unwrap();
        assert_eq!(dec_t, telem);

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
}
