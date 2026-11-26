//! 1-1 VoIP call state machine and async runtime engine.
//!
//! Splits pure state transitions ([`CallStateMachine`]) from async I/O execution ([`CallEngine`])
//! so every call lifecycle path — including passive ringing, benign `status=5` packets, and
//! `dismissed_by_peer` multi-device handling — is deterministically unit-testable.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};
use zalo_core::chat::parser::VoipControlEvent;
use zalo_core::session::Session;

#[cfg(test)]
use crate::audio::TestAudioFault;
use crate::audio::{AudioDeviceSelection, AudioMediaConfig, AudioMediaSession, AudioTelemetry};
use crate::error::{CallError, CallResult};
use crate::media_hook::{AuxiliaryMediaHook, AuxiliaryMediaSession, MediaTransportContext};
use crate::signaling::{
    build_callee_extend_data, build_caller_invite_fields, generate_call_session, AnswerParams,
    CallType, LogEndCallParams, MediaConfParams, RequestCallParams, RequestMediaParams,
    SessionSignalingTransport, SignalingClient, SignalingTransport, OPUS_16K_CODEC_JSON,
};
use crate::trace::{emit_trace, format_trace_json, TraceSink};
use crate::zrtp::{collect_relay_servers, uid_str_to_u32, RelayServer, ZrtpRelaySession};

const MAX_DISMISSED_HISTORY: usize = 256;
const STALE_INVITE_MAX_AGE_MS: i64 = 90_000;
const EARLY_STATUS5_GRACE_MS: i64 = 8_000;
const INCOMING_RING_TIMEOUT_SECS: u64 = 60;

/// Lifecycle phase of a 1-1 VoIP call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPhase {
    Idle,
    Dialing,
    RingingOutgoing,
    IncomingRinging,
    Connecting,
    Connected,
}

/// Reason a call ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallEndReason {
    LocalHangup,
    LocalDeclined,
    LocalCancelled,
    RemoteEnded,
    RemoteCancelled,
    RemoteRejected { code: i64, message: String },
    AnsweredOnOtherDevice,
    EndedOnOtherDevice,
    RingTimeout,
    Error(String),
}

/// Snapshot of the currently active (or ringing) 1-1 call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSnapshot {
    pub call_id: i64,
    pub partner_id: String,
    pub partner_name: String,
    pub call_type: CallType,
    pub is_caller: bool,
    pub phase: CallPhase,
    pub mic_muted: bool,
    #[serde(default)]
    pub peer_mic_muted: bool,
    #[serde(default)]
    pub cam_enabled: bool,
    #[serde(default)]
    pub peer_cam_enabled: bool,
    pub rtp_ip: String,
    pub rtcp_ip: String,
    pub relay_token: u32,
    pub duration_secs: u64,
}

/// High-level events emitted by [`CallStateMachine`] / [`CallEngine`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallEvent {
    StateChanged {
        phase: CallPhase,
        call_id: Option<i64>,
    },
    IncomingCall {
        call_id: i64,
        caller_id: String,
        caller_name: String,
        call_type: CallType,
    },
    RemoteRinging {
        call_id: i64,
    },
    Connected {
        call_id: i64,
        partner_id: String,
        rtp_ip: String,
    },
    DismissedByOtherDevice {
        call_id: i64,
        answered_elsewhere: bool,
    },
    CallEnded {
        call_id: i64,
        partner_id: String,
        duration_secs: u64,
        reason: CallEndReason,
    },
    MediaStateChanged {
        call_id: i64,
        mic_muted: bool,
    },
    PeerMediaChanged {
        call_id: i64,
        mic_muted: bool,
    },
    CameraStateChanged {
        call_id: i64,
        cam_enabled: bool,
    },
    PeerCameraChanged {
        call_id: i64,
        cam_enabled: bool,
    },
    MediaWarning {
        call_id: i64,
        message: String,
    },
    SignalingWarning {
        call_id: i64,
        command: u16,
        code: Option<i64>,
        message: String,
    },
}

/// Internal active call record owned by [`CallStateMachine`].
#[derive(Debug, Clone)]
pub struct ActiveCallRecord {
    pub call_id: i64,
    pub partner_id: String,
    pub partner_name: String,
    pub call_type: CallType,
    pub is_caller: bool,
    pub phase: CallPhase,
    pub sess_id: String,
    pub rtp_ip: String,
    pub rtcp_ip: String,
    pub from_uid_u32: u32,
    pub partner_uid_u32: u32,
    pub relay_token: u32,
    pub mic_muted: bool,
    pub peer_mic_muted: bool,
    pub cam_enabled: bool,
    pub peer_cam_enabled: bool,
    pub started_at_ms: i64,
    pub connected_at_ms: Option<i64>,
    pub invite_payload: Value,
    pub ringing_sent: bool,
    pub remote_ringing_notified: bool,
}

impl ActiveCallRecord {
    pub fn snapshot(&self, now_ms: i64) -> CallSnapshot {
        let duration_secs = match self.connected_at_ms {
            Some(ts) => now_ms.saturating_sub(ts).max(0) as u64 / 1000,
            None => 0,
        };
        CallSnapshot {
            call_id: self.call_id,
            partner_id: self.partner_id.clone(),
            partner_name: self.partner_name.clone(),
            call_type: self.call_type,
            is_caller: self.is_caller,
            phase: self.phase,
            mic_muted: self.mic_muted,
            peer_mic_muted: self.peer_mic_muted,
            cam_enabled: self.cam_enabled,
            peer_cam_enabled: self.peer_cam_enabled,
            rtp_ip: self.rtp_ip.clone(),
            rtcp_ip: self.rtcp_ip.clone(),
            relay_token: self.relay_token,
            duration_secs,
        }
    }
}

/// Side-effect commands produced by pure [`CallStateMachine`] transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEffect {
    SendAnswerAck408 {
        callee_id: String,
        call_id: i64,
    },
    SendCancel405 {
        to_id: String,
        call_id: i64,
        call_type: CallType,
    },
    SendAnswerDecline402 {
        caller_id: String,
        call_id: i64,
        rtp_address: String,
        rtcp_address: String,
        session_id: String,
    },
    SendEndCall409 {
        uid_to: String,
        call_id: i64,
    },
    SendLogEndCall406 {
        call_id: i64,
        partner_id: String,
        duration_secs: u64,
        role: u8,
    },
    SendMediaConf12006 {
        callee_id: String,
        call_id: i64,
        is_on_mic: bool,
        is_on_cam: bool,
    },
    StartCallerMedia {
        call_id: i64,
        rtp_ip: String,
        rtcp_ip: String,
        from_uid_u32: u32,
        sess_id: String,
    },
    ApplyLocalMute(bool),
    ApplyLocalCam(bool),
    RequestRemoteKeyframe {
        call_id: i64,
    },
    ScheduleIncomingTimeout {
        call_id: i64,
        secs: u64,
    },
    StopMediaAndZrtp,
}

/// Output bundle of one state-machine step.
#[derive(Debug, Default)]
pub struct TransitionOutput {
    pub events: Vec<CallEvent>,
    pub effects: Vec<CallEffect>,
}

/// Deterministic, sans-IO 1-1 VoIP call state machine.
#[derive(Debug, Default)]
pub struct CallStateMachine {
    self_uid: String,
    self_uid_u32: u32,
    active: Option<ActiveCallRecord>,
    /// Call IDs dismissed because another logged-in device (e.g. mobile) handled the call or
    /// because the peer already cancelled/ended it. Any subsequent local decline/hangup for these
    /// IDs MUST NOT send `402` to the server.
    dismissed_by_peer: HashSet<i64>,
    dismissed_order: VecDeque<i64>,
}

impl CallStateMachine {
    pub fn new(self_uid: impl Into<String>, self_uid_u32: u32) -> Self {
        Self {
            self_uid: self_uid.into(),
            self_uid_u32,
            active: None,
            dismissed_by_peer: HashSet::new(),
            dismissed_order: VecDeque::new(),
        }
    }

    pub fn phase(&self) -> CallPhase {
        self.active
            .as_ref()
            .map(|c| c.phase)
            .unwrap_or(CallPhase::Idle)
    }

    pub fn active(&self) -> Option<&ActiveCallRecord> {
        self.active.as_ref()
    }

    pub fn active_mut(&mut self) -> Option<&mut ActiveCallRecord> {
        self.active.as_mut()
    }

    pub fn snapshot(&self, now_ms: i64) -> Option<CallSnapshot> {
        self.active.as_ref().map(|c| c.snapshot(now_ms))
    }

    pub fn is_dismissed_by_peer(&self, call_id: i64) -> bool {
        self.dismissed_by_peer.contains(&call_id)
    }

    fn is_self_id(&self, v: &str) -> bool {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            return false;
        }
        (!self.self_uid.is_empty() && trimmed == self.self_uid)
            || (self.self_uid_u32 != 0 && trimmed == self.self_uid_u32.to_string())
    }

    fn mark_dismissed(&mut self, call_id: i64) {
        if call_id == 0 {
            return;
        }
        if self.dismissed_by_peer.insert(call_id) {
            self.dismissed_order.push_back(call_id);
            while self.dismissed_order.len() > MAX_DISMISSED_HISTORY {
                if let Some(old) = self.dismissed_order.pop_front() {
                    self.dismissed_by_peer.remove(&old);
                }
            }
        }
    }

    /// Begin an outgoing 1-1 call (transition `Idle -> Dialing`).
    pub fn begin_outgoing(
        &mut self,
        call_id: i64,
        partner_id: String,
        partner_name: String,
        call_type: CallType,
        now_ms: i64,
    ) -> CallResult<TransitionOutput> {
        if self.active.is_some() {
            return Err(CallError::InvalidState(
                "Đang có cuộc gọi khác đang hoạt động".to_string(),
            ));
        }
        if partner_id.trim().is_empty() {
            return Err(CallError::InvalidArgument(
                "Thiếu mã người nhận (partner_id)".to_string(),
            ));
        }
        self.active = Some(ActiveCallRecord {
            call_id,
            partner_id: partner_id.clone(),
            partner_name,
            call_type,
            is_caller: true,
            phase: CallPhase::Dialing,
            sess_id: String::new(),
            rtp_ip: String::new(),
            rtcp_ip: String::new(),
            from_uid_u32: self.self_uid_u32,
            partner_uid_u32: uid_str_to_u32(&partner_id),
            relay_token: 0,
            mic_muted: false,
            peer_mic_muted: false,
            cam_enabled: call_type.is_video(),
            peer_cam_enabled: call_type.is_video(),
            started_at_ms: now_ms,
            connected_at_ms: None,
            invite_payload: Value::Null,
            ringing_sent: true,
            remote_ringing_notified: false,
        });
        Ok(TransitionOutput {
            events: vec![CallEvent::StateChanged {
                phase: CallPhase::Dialing,
                call_id: Some(call_id),
            }],
            effects: Vec::new(),
        })
    }

    /// Bind the real server-assigned `call_id`, UIDs, session token, and `401` payload immediately
    /// after `401` succeeds (while still `Dialing`), so concurrent cancels send `405` with the real
    /// `call_id` and early WS `ring_ring`/`answer` events match before `416` returns.
    ///
    /// Returns `false` if the outgoing call was already cancelled while awaiting `401`.
    pub fn bind_outgoing_401(
        &mut self,
        req_call_id: i64,
        real_call_id: i64,
        from_uid_u32: u32,
        partner_uid_u32: u32,
        sess_id: String,
        resp_401: Value,
    ) -> bool {
        if let Some(st) = self.active.as_mut() {
            if st.is_caller && st.call_id == req_call_id && st.phase == CallPhase::Dialing {
                st.call_id = real_call_id;
                st.from_uid_u32 = from_uid_u32;
                st.partner_uid_u32 = partner_uid_u32;
                st.sess_id = sess_id;
                st.invite_payload = resp_401;
                return true;
            }
        }
        false
    }

    /// Update the outgoing call after `401` + ZRTP probe + `416` succeed (`Dialing -> RingingOutgoing`).
    #[allow(clippy::too_many_arguments)]
    pub fn mark_outgoing_ringing(
        &mut self,
        real_call_id: i64,
        from_uid_u32: u32,
        partner_uid_u32: u32,
        sess_id: String,
        rtp_ip: String,
        rtcp_ip: String,
        relay_token: u32,
    ) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(st) = self.active.as_mut() {
            if st.is_caller && st.call_id == real_call_id && st.phase == CallPhase::Dialing {
                st.call_id = real_call_id;
                st.from_uid_u32 = from_uid_u32;
                st.partner_uid_u32 = partner_uid_u32;
                st.sess_id = sess_id;
                st.rtp_ip = rtp_ip;
                st.rtcp_ip = rtcp_ip;
                st.relay_token = relay_token;
                st.phase = CallPhase::RingingOutgoing;
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::RingingOutgoing,
                    call_id: Some(real_call_id),
                });
            }
        }
        out
    }

    /// Abort a failed outgoing dial attempt cleanly (`Dialing -> Idle`).
    pub fn fail_outgoing(&mut self, call_id: i64, reason: CallEndReason) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let matches = self.active.as_ref().is_some_and(|st| {
            st.is_caller
                && st.phase == CallPhase::Dialing
                && (call_id == 0 || st.call_id == call_id)
        });
        if matches {
            if let Some(st) = self.active.take() {
                self.mark_dismissed(st.call_id);
                out.effects.push(CallEffect::StopMediaAndZrtp);
                out.events.push(CallEvent::CallEnded {
                    call_id: st.call_id,
                    partner_id: st.partner_id,
                    duration_secs: 0,
                    reason,
                });
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::Idle,
                    call_id: Some(st.call_id),
                });
            }
        }
        out
    }

    /// Abort a failed incoming accept attempt cleanly (`Connecting -> Idle`).
    pub fn fail_connecting(&mut self, call_id: i64, reason: CallEndReason) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let matches = self.active.as_ref().is_some_and(|st| {
            !st.is_caller && st.phase == CallPhase::Connecting && (call_id == 0 || st.call_id == call_id)
        });
        if matches {
            if let Some(st) = self.active.take() {
                self.mark_dismissed(st.call_id);
                out.effects.push(CallEffect::StopMediaAndZrtp);
                out.events.push(CallEvent::CallEnded {
                    call_id: st.call_id,
                    partner_id: st.partner_id,
                    duration_secs: 0,
                    reason,
                });
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::Idle,
                    call_id: Some(st.call_id),
                });
            }
        }
        out
    }

    /// Begin accepting an incoming call locally (`IncomingRinging -> Connecting`).
    ///
    /// Returns `Ok(None)` if the call was already dismissed by another device.
    pub fn begin_accept_incoming(
        &mut self,
        target_call_id: Option<i64>,
    ) -> CallResult<Option<(ActiveCallRecord, TransitionOutput)>> {
        if let Some(cid) = target_call_id {
            if self.is_dismissed_by_peer(cid) {
                return Ok(None);
            }
        }
        let Some(st) = self.active.as_mut() else {
            return Err(CallError::InvalidState(
                "Không có cuộc gọi đến nào đang đổ chuông".to_string(),
            ));
        };
        if st.is_caller || st.phase != CallPhase::IncomingRinging {
            return Err(CallError::InvalidState(
                "Cuộc gọi hiện tại không ở trạng thái đổ chuông đến".to_string(),
            ));
        }
        if let Some(cid) = target_call_id {
            if cid != 0 && cid != st.call_id {
                return Err(CallError::InvalidState(format!(
                    "call_id không khớp (đang có {}, yêu cầu {cid})",
                    st.call_id
                )));
            }
        }
        if self.dismissed_by_peer.contains(&st.call_id) {
            return Ok(None);
        }
        st.phase = CallPhase::Connecting;
        st.ringing_sent = true;
        let rec = st.clone();
        let out = TransitionOutput {
            events: vec![CallEvent::StateChanged {
                phase: CallPhase::Connecting,
                call_id: Some(rec.call_id),
            }],
            effects: Vec::new(),
        };
        Ok(Some((rec, out)))
    }

    /// Finalize callee connection after `407` + ZRTP probe + `402` accept (`Connecting -> Connected`).
    pub fn complete_callee_connected(
        &mut self,
        call_id: i64,
        sess_id: String,
        rtp_ip: String,
        rtcp_ip: String,
        relay_token: u32,
        now_ms: i64,
    ) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(st) = self.active.as_mut() {
            if st.call_id == call_id && !st.is_caller && st.phase == CallPhase::Connecting {
                st.sess_id = sess_id;
                if !rtp_ip.is_empty() {
                    st.rtp_ip = rtp_ip;
                    st.rtcp_ip = rtcp_ip;
                }
                st.relay_token = relay_token;
                st.phase = CallPhase::Connected;
                st.connected_at_ms = Some(now_ms);
                out.events.push(CallEvent::Connected {
                    call_id: st.call_id,
                    partner_id: st.partner_id.clone(),
                    rtp_ip: st.rtp_ip.clone(),
                });
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::Connected,
                    call_id: Some(st.call_id),
                });
            }
        }
        out
    }

    /// Decline an incoming call locally.
    ///
    /// If `call_id` was already handled on another device (`dismissed_by_peer`), this strictly
    /// emits NO `SendAnswerDecline402` effect!
    pub fn decline_incoming(&mut self, target_call_id: Option<i64>) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(cid) = target_call_id {
            if self.is_dismissed_by_peer(cid) {
                return out;
            }
        }
        let Some(st) = self.active.as_ref() else {
            return out;
        };
        if st.is_caller || st.phase == CallPhase::Connected {
            return out;
        }
        if let Some(cid) = target_call_id {
            if cid != 0 && cid != st.call_id {
                return out;
            }
        }
        let Some(st) = self.active.take() else {
            return out;
        };
        let was_dismissed = self.dismissed_by_peer.contains(&st.call_id);
        self.mark_dismissed(st.call_id);
        out.effects.push(CallEffect::StopMediaAndZrtp);
        if !was_dismissed {
            out.effects.push(CallEffect::SendAnswerDecline402 {
                caller_id: st.partner_id.clone(),
                call_id: st.call_id,
                rtp_address: st.rtp_ip.clone(),
                rtcp_address: st.rtcp_ip.clone(),
                session_id: st.sess_id.clone(),
            });
        }
        out.events.push(CallEvent::CallEnded {
            call_id: st.call_id,
            partner_id: st.partner_id,
            duration_secs: 0,
            reason: CallEndReason::LocalDeclined,
        });
        out.events.push(CallEvent::StateChanged {
            phase: CallPhase::Idle,
            call_id: Some(st.call_id),
        });
        out
    }

    /// Terminate the active call from the local side.
    pub fn end_call(&mut self, now_ms: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let Some(st) = self.active.take() else {
            return out;
        };
        let was_dismissed = self.dismissed_by_peer.contains(&st.call_id);
        self.mark_dismissed(st.call_id);
        let duration_secs = match st.connected_at_ms {
            Some(ts) => now_ms.saturating_sub(ts).max(0) as u64 / 1000,
            None => 0,
        };
        out.effects.push(CallEffect::StopMediaAndZrtp);

        let reason = if st.phase == CallPhase::Connected {
            out.effects.push(CallEffect::SendEndCall409 {
                uid_to: st.partner_id.clone(),
                call_id: st.call_id,
            });
            out.effects.push(CallEffect::SendLogEndCall406 {
                call_id: st.call_id,
                partner_id: st.partner_id.clone(),
                duration_secs,
                role: if st.is_caller { 1 } else { 0 },
            });
            CallEndReason::LocalHangup
        } else if st.is_caller {
            out.effects.push(CallEffect::SendCancel405 {
                to_id: st.partner_id.clone(),
                call_id: st.call_id,
                call_type: st.call_type,
            });
            CallEndReason::LocalCancelled
        } else {
            if !was_dismissed {
                out.effects.push(CallEffect::SendAnswerDecline402 {
                    caller_id: st.partner_id.clone(),
                    call_id: st.call_id,
                    rtp_address: st.rtp_ip.clone(),
                    rtcp_address: st.rtcp_ip.clone(),
                    session_id: st.sess_id.clone(),
                });
            }
            CallEndReason::LocalDeclined
        };

        out.events.push(CallEvent::CallEnded {
            call_id: st.call_id,
            partner_id: st.partner_id,
            duration_secs,
            reason,
        });
        out.events.push(CallEvent::StateChanged {
            phase: CallPhase::Idle,
            call_id: Some(st.call_id),
        });
        out
    }

    /// Update microphone mute state during an active call.
    pub fn set_mic_muted(&mut self, muted: bool) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(st) = self.active.as_mut() {
            st.mic_muted = muted;
            out.effects.push(CallEffect::ApplyLocalMute(muted));
            // Respect passive ringing: never transmit 12006 to server before local accept.
            if st.phase != CallPhase::IncomingRinging {
                out.effects.push(CallEffect::SendMediaConf12006 {
                    callee_id: st.partner_id.clone(),
                    call_id: st.call_id,
                    is_on_mic: !muted,
                    is_on_cam: st.cam_enabled,
                });
            }
            out.events.push(CallEvent::MediaStateChanged {
                call_id: st.call_id,
                mic_muted: muted,
            });
        }
        out
    }

    /// Update local camera state during an active call.
    pub fn set_cam_enabled(&mut self, enabled: bool) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(st) = self.active.as_mut() {
            if !st.call_type.is_video() {
                return out;
            }
            st.cam_enabled = enabled;
            out.effects.push(CallEffect::ApplyLocalCam(enabled));
            if st.phase != CallPhase::IncomingRinging {
                out.effects.push(CallEffect::SendMediaConf12006 {
                    callee_id: st.partner_id.clone(),
                    call_id: st.call_id,
                    is_on_mic: !st.mic_muted,
                    is_on_cam: enabled,
                });
            }
            out.events.push(CallEvent::CameraStateChanged {
                call_id: st.call_id,
                cam_enabled: enabled,
            });
        }
        out
    }

    /// Handle passive 60-second incoming ring timeout (does NOT send `402`).
    pub fn handle_incoming_ring_timeout(&mut self, call_id: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let should_timeout = self.active.as_ref().is_some_and(|st| {
            st.call_id == call_id
                && !st.is_caller
                && st.phase == CallPhase::IncomingRinging
                && !st.ringing_sent
        });
        if should_timeout {
            if let Some(st) = self.active.take() {
                self.mark_dismissed(st.call_id);
                out.events.push(CallEvent::CallEnded {
                    call_id: st.call_id,
                    partner_id: st.partner_id,
                    duration_secs: 0,
                    reason: CallEndReason::RingTimeout,
                });
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::Idle,
                    call_id: Some(st.call_id),
                });
            }
        }
        out
    }

    /// Process an incoming Zalo VoIP control event (`act_type == "voip"`).
    pub fn handle_voip_event(&mut self, ev: &VoipControlEvent, now_ms: i64) -> TransitionOutput {
        let act = ev.act.as_str();
        let data = &ev.data;
        let ev_call_id = ev.call_id();

        match act {
            "ring_ring" => self.on_ring_ring(ev_call_id),
            "answer" => self.on_answer(ev, ev_call_id, now_ms),
            "end_call" | "cancel" => self.on_remote_end(act, ev_call_id, now_ms),
            "request" => self.on_incoming_request(ev, ev_call_id, data, now_ms),
            "mute_audio" => self.on_peer_mute_audio(ev, ev_call_id),
            "onoff_camera" | "onoff_cam" => self.on_peer_cam_state(ev, ev_call_id),
            _ => TransitionOutput::default(),
        }
    }

    fn on_peer_cam_state(&mut self, ev: &VoipControlEvent, ev_call_id: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let uid_n = value_as_string(ev.data.get("uidN"));
        let uid_from = value_as_string(ev.data.get("uidFrom"));
        if self.is_self_id(&uid_n) || self.is_self_id(&uid_from) {
            return out;
        }
        let Some(st) = self.active.as_mut() else {
            return out;
        };
        if (ev_call_id != 0 && ev_call_id != st.call_id)
            || !matches!(st.phase, CallPhase::Connecting | CallPhase::Connected)
        {
            return out;
        }
        let mp = ev.media_params();
        let parse_cam = |src: &Value| -> Option<bool> {
            src.get("type")
                .and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                        .or_else(|| v.as_bool().map(i64::from))
                })
                // Mobile clients send `type: 1` when camera is on/resumed, `type: 0` when turned off,
                // and `type: 2` (e.g. `reason: 2`) when camera is paused/backgrounded.
                .map(|t| t == 1)
                .or_else(|| {
                    src.get("isOnCam")
                        .or_else(|| src.get("cameraState"))
                        .and_then(|v| {
                            v.as_bool()
                                .or_else(|| v.as_i64().map(|n| n != 0))
                                .or_else(|| {
                                    v.as_str()
                                        .and_then(|s| s.trim().parse::<i64>().ok())
                                        .map(|n| n != 0)
                                })
                        })
                })
        };
        let raw_enabled = parse_cam(&mp).or_else(|| parse_cam(&ev.data));
        let Some(cam_enabled) = raw_enabled else {
            return out;
        };
        if st.peer_cam_enabled != cam_enabled {
            let resumed = !st.peer_cam_enabled && cam_enabled;
            st.peer_cam_enabled = cam_enabled;
            if resumed {
                out.effects.push(CallEffect::RequestRemoteKeyframe {
                    call_id: st.call_id,
                });
            }
            out.events.push(CallEvent::PeerCameraChanged {
                call_id: st.call_id,
                cam_enabled,
            });
        }
        out
    }

    fn on_peer_mute_audio(&mut self, ev: &VoipControlEvent, ev_call_id: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let uid_n = value_as_string(ev.data.get("uidN"));
        let uid_from = value_as_string(ev.data.get("uidFrom"));
        if self.is_self_id(&uid_n) || self.is_self_id(&uid_from) {
            return out;
        }
        let Some(st) = self.active.as_mut() else {
            return out;
        };
        if (ev_call_id != 0 && ev_call_id != st.call_id)
            || !matches!(st.phase, CallPhase::Connecting | CallPhase::Connected)
        {
            return out;
        }
        let mp = ev.media_params();
        let parse_muted = |src: &Value| -> Option<bool> {
            src.get("type")
                .and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                })
                .map(|t| t != 0)
                .or_else(|| {
                    src.get("isOnMic")
                        .and_then(|v| {
                            v.as_i64()
                                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                                .or_else(|| v.as_bool().map(i64::from))
                        })
                        .map(|on| on == 0)
                })
                .or_else(|| {
                    src.get("mute").and_then(|v| {
                        v.as_bool()
                            .or_else(|| v.as_i64().map(|n| n != 0))
                            .or_else(|| {
                                v.as_str()
                                    .and_then(|s| s.trim().parse::<i64>().ok())
                                    .map(|n| n != 0)
                            })
                    })
                })
        };
        let raw_muted = parse_muted(&mp).or_else(|| parse_muted(&ev.data));
        let Some(muted) = raw_muted else {
            return out;
        };
        if st.peer_mic_muted != muted {
            st.peer_mic_muted = muted;
            out.events.push(CallEvent::PeerMediaChanged {
                call_id: st.call_id,
                mic_muted: muted,
            });
        }
        out
    }

    fn on_ring_ring(&mut self, ev_call_id: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if let Some(st) = self.active.as_mut() {
            if (ev_call_id == 0 || ev_call_id == st.call_id)
                && st.is_caller
                && matches!(st.phase, CallPhase::Dialing | CallPhase::RingingOutgoing)
            {
                let phase_changed = st.phase != CallPhase::RingingOutgoing;
                st.phase = CallPhase::RingingOutgoing;
                if !st.remote_ringing_notified {
                    st.remote_ringing_notified = true;
                    out.events.push(CallEvent::RemoteRinging {
                        call_id: st.call_id,
                    });
                    out.events.push(CallEvent::StateChanged {
                        phase: CallPhase::RingingOutgoing,
                        call_id: Some(st.call_id),
                    });
                } else if phase_changed {
                    out.events.push(CallEvent::StateChanged {
                        phase: CallPhase::RingingOutgoing,
                        call_id: Some(st.call_id),
                    });
                }
            }
        }
        out
    }

    fn on_answer(
        &mut self,
        ev: &VoipControlEvent,
        ev_call_id: i64,
        now_ms: i64,
    ) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        let status = ev.status().unwrap_or(0);
        let data = &ev.data;

        if self.active.is_none() {
            // If we have no active call yet, distinguish our own other device answering/declining
            // (mark dismissed so a reordered late `request` won't ghost-ring) from a peer-originated
            // invite delivered as `act:"answer"` (status=5 or carrying invite media).
            if ev_call_id != 0 && !self.dismissed_by_peer.contains(&ev_call_id) {
                let uid_n = data
                    .get("uidN")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                let uid_from = value_as_string(data.get("uidFrom"));
                let uid_from_trim = uid_from.trim();
                let is_from_self = self.is_self_id(uid_n) || self.is_self_id(uid_from_trim);
                if is_from_self {
                    if status != 5 {
                        self.mark_dismissed(ev_call_id);
                    }
                    return out;
                }
                let is_from_peer = !uid_n.is_empty() || !uid_from_trim.is_empty();
                let media_p = ev.media_params();
                let has_invite_media = !media_p
                    .get("sessId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .is_empty()
                    || !media_p
                        .get("extendData")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .is_empty();
                if is_from_peer && (status == 5 || (matches!(status, 0 | 3) && has_invite_media)) {
                    let synth = VoipControlEvent {
                        act: "request".to_string(),
                        data: data.clone(),
                    };
                    return self.on_incoming_request(&synth, ev_call_id, &synth.data, now_ms);
                }
            }
            return out;
        }

        let Some(st_ref) = self.active.as_ref() else {
            return out;
        };
        if ev_call_id != 0 && ev_call_id != st_ref.call_id {
            return out;
        }
        if st_ref.phase == CallPhase::Connected {
            return out;
        }

        // Callee path: receiving `answer` while ringing/not yet connected locally
        if !st_ref.is_caller {
            // Native Zalo sends benign `status=5` ~0–1s into an incoming call: stay ringing!
            if status == 5 {
                return out;
            }
            // Otherwise the call was answered (status==0) or declined/ended (status!=0) on
            // another device (e.g. mobile phone). Dismiss locally and mark `dismissed_by_peer`
            // so we NEVER send `402` to the server.
            let Some(st) = self.active.take() else {
                return out;
            };
            self.mark_dismissed(st.call_id);
            let answered_elsewhere = status == 0;
            out.effects.push(CallEffect::StopMediaAndZrtp);
            out.events.push(CallEvent::DismissedByOtherDevice {
                call_id: st.call_id,
                answered_elsewhere,
            });
            out.events.push(CallEvent::CallEnded {
                call_id: st.call_id,
                partner_id: st.partner_id,
                duration_secs: 0,
                reason: if answered_elsewhere {
                    CallEndReason::AnsweredOnOtherDevice
                } else {
                    CallEndReason::EndedOnOtherDevice
                },
            });
            out.events.push(CallEvent::StateChanged {
                phase: CallPhase::Idle,
                call_id: Some(st.call_id),
            });
            return out;
        }

        // Caller path: callee answered or rejected
        let ans_media = ev.media_params();
        if status == 0 {
            if let Some(st) = self.active.as_mut() {
                if let Some(sel_srv) = ans_media
                    .get("rtpSerIp")
                    .or_else(|| ans_media.get("rtpIP"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let explicit_rtcp = ans_media
                        .get("rtcpSerIp")
                        .or_else(|| ans_media.get("rtcpIP"))
                        .or_else(|| ans_media.get("rtcpAddress"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let resolved_rtcp = explicit_rtcp
                        .or_else(|| {
                            collect_relay_servers(&st.invite_payload)
                                .into_iter()
                                .find(|s| s.rtp == sel_srv)
                                .map(|s| s.rtcp)
                        })
                        .or_else(|| {
                            (sel_srv == st.rtp_ip && !st.rtcp_ip.is_empty())
                                .then(|| st.rtcp_ip.clone())
                        })
                        .unwrap_or_else(|| sel_srv.to_string());
                    if st.rtp_ip != sel_srv {
                        st.relay_token = 0;
                    }
                    st.rtp_ip = sel_srv.to_string();
                    st.rtcp_ip = resolved_rtcp;
                }
                st.phase = CallPhase::Connected;
                st.connected_at_ms = Some(now_ms);
                out.effects.push(CallEffect::SendAnswerAck408 {
                    callee_id: st.partner_id.clone(),
                    call_id: st.call_id,
                });
                if st.call_type.is_video() {
                    out.effects.push(CallEffect::SendMediaConf12006 {
                        callee_id: st.partner_id.clone(),
                        call_id: st.call_id,
                        is_on_mic: !st.mic_muted,
                        is_on_cam: st.cam_enabled,
                    });
                }
                out.effects.push(CallEffect::StartCallerMedia {
                    call_id: st.call_id,
                    rtp_ip: st.rtp_ip.clone(),
                    rtcp_ip: st.rtcp_ip.clone(),
                    from_uid_u32: st.from_uid_u32,
                    sess_id: st.sess_id.clone(),
                });
                out.events.push(CallEvent::Connected {
                    call_id: st.call_id,
                    partner_id: st.partner_id.clone(),
                    rtp_ip: st.rtp_ip.clone(),
                });
                out.events.push(CallEvent::StateChanged {
                    phase: CallPhase::Connected,
                    call_id: Some(st.call_id),
                });
            }
            return out;
        }

        // Caller benign early status=5 within 8s when `extendData` is present
        let elapsed_ms = now_ms.saturating_sub(st_ref.started_at_ms);
        let has_ext = !ans_media
            .get("extendData")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty();
        if status == 5 && elapsed_ms < EARLY_STATUS5_GRACE_MS && has_ext {
            return out;
        }

        // Rejected / busy / unavailable
        let Some(st) = self.active.take() else {
            return out;
        };
        self.mark_dismissed(st.call_id);
        let msg = match status {
            1..=4 => "Người nhận đang bận hoặc đã từ chối cuộc gọi",
            5 => "Kết nối thoại bị gián đoạn hoặc người nhận từ chối",
            _ => "Người nhận không trả lời cuộc gọi",
        };
        out.effects.push(CallEffect::StopMediaAndZrtp);
        out.events.push(CallEvent::CallEnded {
            call_id: st.call_id,
            partner_id: st.partner_id,
            duration_secs: 0,
            reason: CallEndReason::RemoteRejected {
                code: status,
                message: msg.to_string(),
            },
        });
        out.events.push(CallEvent::StateChanged {
            phase: CallPhase::Idle,
            call_id: Some(st.call_id),
        });
        out
    }

    fn on_remote_end(&mut self, act: &str, ev_call_id: i64, now_ms: i64) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if ev_call_id != 0 {
            self.mark_dismissed(ev_call_id);
        }
        let matches = self
            .active
            .as_ref()
            .is_some_and(|st| ev_call_id == 0 || ev_call_id == st.call_id);
        if !matches {
            return out;
        }
        let Some(st) = self.active.take() else {
            return out;
        };
        self.mark_dismissed(st.call_id);
        let duration_secs = match st.connected_at_ms {
            Some(ts) => now_ms.saturating_sub(ts).max(0) as u64 / 1000,
            None => 0,
        };
        let reason = if act == "cancel" {
            CallEndReason::RemoteCancelled
        } else {
            CallEndReason::RemoteEnded
        };
        out.effects.push(CallEffect::StopMediaAndZrtp);
        out.events.push(CallEvent::CallEnded {
            call_id: st.call_id,
            partner_id: st.partner_id,
            duration_secs,
            reason,
        });
        out.events.push(CallEvent::StateChanged {
            phase: CallPhase::Idle,
            call_id: Some(st.call_id),
        });
        out
    }

    fn on_incoming_request(
        &mut self,
        ev: &VoipControlEvent,
        call_id: i64,
        data: &Value,
        now_ms: i64,
    ) -> TransitionOutput {
        let mut out = TransitionOutput::default();
        if call_id == 0 || self.dismissed_by_peer.contains(&call_id) {
            return out;
        }

        // Ignore self-originated `request` echoes (e.g. own outgoing invite echoed over WS/lp_control).
        let uid_n = value_as_string(data.get("uidN"));
        let uid_from = value_as_string(data.get("uidFrom"));
        if self.is_self_id(&uid_n) || self.is_self_id(&uid_from) {
            return out;
        }

        // Reject historical replays older than 90 seconds
        if let Some(raw_ts) = data.get("ts").and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        }) {
            let ev_ms = if (1_000_000_000..10_000_000_000).contains(&raw_ts) {
                raw_ts.saturating_mul(1000)
            } else {
                raw_ts
            };
            if ev_ms > 0 && now_ms.saturating_sub(ev_ms) > STALE_INVITE_MAX_AGE_MS {
                return out;
            }
        }

        let media = ev.media_params();
        let relay_source = if media.is_object() {
            media.clone()
        } else {
            data.clone()
        };

        let caller_id = value_as_nonempty_str(data.get("uidN").or_else(|| data.get("uidFrom")))
            .unwrap_or_default();
        let raw_caller_name = media
            .get("Dname")
            .or_else(|| data.get("Dname"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let caller_name = raw_caller_name
            .clone()
            .unwrap_or_else(|| "Người dùng".to_string());

        let ext_parsed: Value = media
            .get("extendData")
            .or_else(|| data.get("extendData"))
            .and_then(|v| match v {
                Value::String(s) => serde_json::from_str::<Value>(s.trim()).ok(),
                Value::Object(_) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null);
        let raw_call_type = media
            .pointer("/zrtc_config/callType")
            .or_else(|| data.get("callType"))
            .and_then(value_as_i64)
            .unwrap_or(1);
        let is_video = raw_call_type == 3
            || ext_parsed.get("callType").and_then(value_as_i64) == Some(1)
            || media
                .pointer("/zrtc_config/video/enable")
                .and_then(value_as_i64)
                == Some(1);
        let call_type = CallType::from_video_flag(is_video);

        let parsed_from_uid_u32 = data
            .get("uidTo")
            .or_else(|| data.get("toId"))
            .or_else(|| data.get("calleeId"))
            .or_else(|| media.get("reveiverId"))
            .or_else(|| media.get("receiverId"))
            .and_then(value_as_u32);
        let from_uid_u32 = parsed_from_uid_u32.unwrap_or(self.self_uid_u32);
        let parsed_partner_uid_u32 = data
            .get("uidFrom")
            .or_else(|| data.get("fromId"))
            .or_else(|| data.get("callerId"))
            .and_then(value_as_u32);
        let partner_uid_u32 = parsed_partner_uid_u32.unwrap_or_else(|| uid_str_to_u32(&caller_id));

        let sess_id = extract_sess_id(&media, data).unwrap_or_default();
        let first_srv = collect_relay_servers(&relay_source).into_iter().next();
        let init_rtp = first_srv.as_ref().map(|s| s.rtp.clone()).unwrap_or_default();
        let init_rtcp = first_srv
            .as_ref()
            .map(|s| s.rtcp.clone())
            .unwrap_or_else(|| init_rtp.clone());

        // Re-invite for an already-ringing call: upgrade invite payload ONLY when incoming is richer
        // (never allow a stripped `lp_control` stub to clobber a rich WS invite), and refresh all
        // derived invite metadata when upgrading without re-emitting IncomingCall.
        if let Some(st) = self.active.as_mut() {
            if st.call_id == call_id {
                if st.phase == CallPhase::IncomingRinging {
                    let (cur_ext, cur_srvs) = invite_payload_richness(&st.invite_payload);
                    let (new_ext, new_srvs) = invite_payload_richness(&relay_source);
                    let should_upgrade = (!cur_ext && new_ext)
                        || (cur_ext == new_ext && new_srvs > cur_srvs)
                        || (!cur_ext && cur_srvs == 0 && relay_source.is_object());
                    if should_upgrade {
                        let prev_call_type = st.call_type;
                        let prev_partner_name = st.partner_name.clone();
                        st.invite_payload = relay_source;
                        if !init_rtp.is_empty() {
                            st.rtp_ip = init_rtp;
                            st.rtcp_ip = init_rtcp;
                        }
                        if !sess_id.is_empty() {
                            st.sess_id = sess_id;
                        }
                        if let Some(name) = raw_caller_name {
                            st.partner_name = name;
                        }
                        if is_video {
                            st.call_type = CallType::DirectVideo;
                            st.cam_enabled = true;
                            st.peer_cam_enabled = true;
                        }
                        if let Some(u) = parsed_from_uid_u32.filter(|&v| v != 0) {
                            st.from_uid_u32 = u;
                        }
                        if let Some(u) = parsed_partner_uid_u32.filter(|&v| v != 0) {
                            st.partner_uid_u32 = u;
                        }
                        if st.call_type != prev_call_type || st.partner_name != prev_partner_name {
                            out.events.push(CallEvent::StateChanged {
                                phase: st.phase,
                                call_id: Some(st.call_id),
                            });
                        }
                    } else if st.sess_id.is_empty() && !sess_id.is_empty() {
                        st.sess_id = sess_id;
                    }
                    // Prefer 19-digit `uidN` if current partner_id was empty or shorter 32-bit `uidFrom`.
                    if !caller_id.is_empty()
                        && (st.partner_id.is_empty()
                            || (caller_id.len() > st.partner_id.len() && !uid_n.trim().is_empty()))
                    {
                        st.partner_id = caller_id;
                    }
                }
                return out;
            }
            // Busy with another call
            return out;
        }

        // Passive ringing: create state + emit IncomingCall ONLY — do NOT send 407 or probe UDP yet!
        self.active = Some(ActiveCallRecord {
            call_id,
            partner_id: caller_id.clone(),
            partner_name: caller_name.clone(),
            call_type,
            is_caller: false,
            phase: CallPhase::IncomingRinging,
            sess_id,
            rtp_ip: init_rtp,
            rtcp_ip: init_rtcp,
            from_uid_u32,
            partner_uid_u32,
            relay_token: 0,
            mic_muted: false,
            peer_mic_muted: false,
            cam_enabled: call_type.is_video(),
            peer_cam_enabled: call_type.is_video(),
            started_at_ms: now_ms,
            connected_at_ms: None,
            invite_payload: relay_source,
            ringing_sent: false,
            remote_ringing_notified: false,
        });

        out.events.push(CallEvent::IncomingCall {
            call_id,
            caller_id,
            caller_name,
            call_type,
        });
        out.events.push(CallEvent::StateChanged {
            phase: CallPhase::IncomingRinging,
            call_id: Some(call_id),
        });
        out.effects.push(CallEffect::ScheduleIncomingTimeout {
            call_id,
            secs: INCOMING_RING_TIMEOUT_SECS,
        });
        out
    }
}

fn invite_payload_richness(v: &Value) -> (bool, usize) {
    let has_ext = v
        .get("extendData")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    let server_count = collect_relay_servers(v).len();
    (has_ext, server_count)
}

fn extract_sess_id(media: &Value, data: &Value) -> Option<String> {
    media
        .get("sessId")
        .or_else(|| media.get("session"))
        .or_else(|| data.get("session"))
        .or_else(|| data.get("sessId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn value_as_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn value_as_nonempty_str(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn value_as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .map(|n| n as u32)
}

fn value_as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        .or_else(|| v.as_bool().map(i64::from))
}

async fn stop_audio_session_blocking(session: AudioMediaSession) {
    let _ = tokio::task::spawn_blocking(move || session.stop()).await;
}

async fn stop_aux_session_blocking(aux: Arc<dyn AuxiliaryMediaSession>) {
    let _ = tokio::task::spawn_blocking(move || aux.stop()).await;
}

/// Disarms `ZrtpRelaySession::keepalive_task`'s RTP socket receiver before `aux_media_hook` or
/// OS audio startup begins so early video/audio RTP/RTCP packets are never swallowed before
/// `AudioMediaSession` takes ownership of the socket.
struct PendingMediaAttachGuard {
    flag: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    previous: bool,
    handed_off: bool,
}

impl PendingMediaAttachGuard {
    fn acquire(flag: Arc<AtomicBool>, notify: Arc<tokio::sync::Notify>) -> Self {
        let previous = flag.swap(true, Ordering::AcqRel);
        notify.notify_one();
        Self {
            flag,
            notify,
            previous,
            handed_off: false,
        }
    }

    fn hand_off(mut self) {
        self.handed_off = true;
    }
}

impl Drop for PendingMediaAttachGuard {
    fn drop(&mut self) {
        if !self.handed_off {
            self.flag.store(self.previous, Ordering::Release);
            self.notify.notify_one();
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Configuration for [`CallEngine`].
#[derive(Debug, Clone)]
pub struct CallEngineConfig {
    /// Whether to start hardware audio (`cpal` + Opus) on connect. Set `false` for signaling-only tests.
    pub enable_audio: bool,
    pub devices: AudioDeviceSelection,
    /// Optional sanitized wire trace sink (never logs `zpw_sek`/`zpw_enk`/cookies).
    pub trace: Option<TraceSink>,
}

impl Default for CallEngineConfig {
    fn default() -> Self {
        Self {
            enable_audio: true,
            devices: AudioDeviceSelection::default(),
            trace: None,
        }
    }
}

fn next_req_call_id() -> i64 {
    static REQ_CALL_COUNTER: AtomicI64 = AtomicI64::new(0);
    let seq = REQ_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = (now_ms().wrapping_add(seq)).rem_euclid(1_800_000_000) + 100_000_000;
    base.max(1)
}

fn response_error_msg(resp: &Value, fallback: &str) -> String {
    resp.get("msg")
        .or_else(|| resp.get("message"))
        .or_else(|| resp.get("error_message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub(crate) fn format_virtual_audio_warning(telem: &AudioTelemetry) -> Option<String> {
    let mic = telem.active_input_device.as_deref().unwrap_or("Micro ảo");
    let spk = telem.active_output_device.as_deref().unwrap_or("Loa ảo");
    if telem.has_input_device && telem.input_is_virtual && telem.output_is_virtual {
        Some(format!(
            "Đang dùng thiết bị âm thanh ảo ({mic} / {spk}) — cuộc gọi có thể bị mất tiếng 2 chiều. Hãy cắm tai nghe/micro thật và chọn lại trong Thiết bị âm thanh."
        ))
    } else if telem.has_input_device && telem.input_is_virtual {
        Some(format!(
            "Micro hiện tại ({mic}) là thiết bị ảo/loopback — bên kia có thể không nghe thấy giọng bạn. Hãy chọn micro thật trong Thiết bị âm thanh."
        ))
    } else if telem.output_is_virtual {
        Some(format!(
            "Loa hiện tại ({spk}) là thiết bị ảo/loopback — bạn có thể không nghe thấy bên kia nói. Hãy chọn loa/tai nghe thật trong Thiết bị âm thanh."
        ))
    } else {
        None
    }
}

type AuxMediaSlot = Arc<Mutex<Option<(i64, Arc<dyn AuxiliaryMediaSession>)>>>;

/// Live 1-1 VoIP call engine orchestrating signaling, UDP ZRTP relay, and Opus audio pipeline.
pub struct CallEngine {
    transport: Arc<dyn SignalingTransport>,
    config: CallEngineConfig,
    devices: Arc<Mutex<AudioDeviceSelection>>,
    sm: Arc<Mutex<CallStateMachine>>,
    zrtp: Arc<Mutex<Option<(i64, ZrtpRelaySession)>>>,
    media: Arc<Mutex<Option<(i64, AudioMediaSession)>>>,
    aux_media_hook: Option<Arc<dyn AuxiliaryMediaHook>>,
    aux_media: AuxMediaSlot,
    events_tx: broadcast::Sender<CallEvent>,
    #[cfg(test)]
    audio_fault: TestAudioFault,
}

impl CallEngine {
    /// Construct a new 1-1 call engine bound to `session`.
    ///
    /// Immediately rejects with [`CallError::RelayUnsupported`] when `session.relay.enabled == true`.
    pub fn new(session: Session, config: CallEngineConfig) -> CallResult<Self> {
        Self::with_signaling_client(session, SignalingClient::new(), config)
    }

    /// Construct a new 1-1 call engine using a customized [`SignalingClient`] (e.g. `with_zpw_override`).
    pub fn with_signaling_client(
        session: Session,
        client: SignalingClient,
        config: CallEngineConfig,
    ) -> CallResult<Self> {
        let transport = SessionSignalingTransport::new(session, client)?;
        Self::with_transport(Arc::new(transport), config)
    }

    /// Construct a new 1-1 call engine backed by any [`SignalingTransport`] (enables deterministic
    /// mock-signaling integration tests of the full async call lifecycle).
    pub fn with_transport(
        transport: Arc<dyn SignalingTransport>,
        config: CallEngineConfig,
    ) -> CallResult<Self> {
        transport.check_callable()?;
        let uid_str = transport.self_uid();
        let uid_u32 = transport.self_uid_u32();
        let (events_tx, _) = broadcast::channel(128);
        let devices = Arc::new(Mutex::new(config.devices.clone()));
        Ok(Self {
            transport,
            config,
            devices,
            sm: Arc::new(Mutex::new(CallStateMachine::new(uid_str, uid_u32))),
            zrtp: Arc::new(Mutex::new(None)),
            media: Arc::new(Mutex::new(None)),
            aux_media_hook: None,
            aux_media: Arc::new(Mutex::new(None)),
            events_tx,
            #[cfg(test)]
            audio_fault: TestAudioFault::None,
        })
    }

    /// Attach an auxiliary media factory hook (such as `VideoMediaHook` from `teza-video`).
    pub fn with_aux_media_hook(mut self, hook: Arc<dyn AuxiliaryMediaHook>) -> Self {
        self.aux_media_hook = Some(hook);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_transport_and_audio_fault(
        transport: Arc<dyn SignalingTransport>,
        config: CallEngineConfig,
        audio_fault: TestAudioFault,
    ) -> CallResult<Self> {
        let mut engine = Self::with_transport(transport, config)?;
        engine.audio_fault = audio_fault;
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CallEvent> {
        self.events_tx.subscribe()
    }

    pub async fn snapshot(&self) -> Option<CallSnapshot> {
        let mut snap = self.sm.lock().await.snapshot(now_ms())?;
        if !snap.rtp_ip.is_empty() {
            if let Some((cid, z)) = self.zrtp.lock().await.as_ref() {
                if *cid == snap.call_id {
                    let live = z.token_for(&snap.rtp_ip);
                    if live != 0 {
                        snap.relay_token = live;
                    }
                }
            }
        }
        Some(snap)
    }

    pub async fn phase(&self) -> CallPhase {
        self.sm.lock().await.phase()
    }

    pub async fn audio_telemetry(&self) -> Option<AudioTelemetry> {
        self.media.lock().await.as_ref().map(|(_, m)| m.telemetry())
    }

    /// Diagnostic helper: returns `true` when an active UDP ZRTP relay session is currently held.
    pub async fn has_active_zrtp(&self) -> bool {
        self.zrtp.lock().await.is_some()
    }

    /// Diagnostic helper: returns the `call_id` owning the currently active ZRTP session, if any.
    pub async fn active_zrtp_call_id(&self) -> Option<i64> {
        self.zrtp.lock().await.as_ref().map(|(cid, _)| *cid)
    }

    /// Diagnostic helper: returns `true` when an active audio media thread is currently running.
    pub async fn has_active_media(&self) -> bool {
        self.media.lock().await.is_some()
    }

    /// Diagnostic helper: returns `true` when an auxiliary media session (e.g. video) is attached.
    pub async fn has_active_aux_media(&self) -> bool {
        self.aux_media.lock().await.is_some()
    }

    fn emit_events(&self, events: Vec<CallEvent>) {
        for ev in events {
            let _ = self.events_tx.send(ev);
        }
    }

    fn report_signaling_error(&self, call_id: i64, command: u16, label: &str, err: CallError) {
        let code = match &err {
            CallError::Signaling { code, .. } | CallError::SessionExpired { code } => Some(*code),
            _ => None,
        };
        let message = format!("{label} thất bại: {err}");
        emit_trace(self.config.trace.as_ref(), || {
            format!("[ENG SIG WARN] call_id={call_id} cmd={command} code={code:?} msg={message}")
        });
        eprintln!("[teza-call][engine] call_id={call_id} cmd={command}: {message}");
        self.emit_events(vec![CallEvent::SignalingWarning {
            call_id,
            command,
            code,
            message,
        }]);
    }

    async fn stop_media_and_zrtp(&self) {
        let old_media = self.media.lock().await.take();
        if let Some((_, m)) = old_media {
            stop_audio_session_blocking(m).await;
        }
        let old_aux = self.aux_media.lock().await.take();
        if let Some((_, aux)) = old_aux {
            stop_aux_session_blocking(aux).await;
        }
        let old_zrtp = self.zrtp.lock().await.take();
        if let Some((_, z)) = old_zrtp {
            z.shutdown().await;
        }
    }

    async fn stop_media_and_zrtp_for_call(&self, call_id: i64) {
        let old_media = {
            let mut g = self.media.lock().await;
            if g.as_ref().is_some_and(|(cid, _)| *cid == call_id) {
                g.take()
            } else {
                None
            }
        };
        if let Some((_, m)) = old_media {
            stop_audio_session_blocking(m).await;
        }
        let old_aux = {
            let mut g = self.aux_media.lock().await;
            if g.as_ref().is_some_and(|(cid, _)| *cid == call_id) {
                g.take()
            } else {
                None
            }
        };
        if let Some((_, aux)) = old_aux {
            stop_aux_session_blocking(aux).await;
        }
        let old_zrtp = {
            let mut g = self.zrtp.lock().await;
            if g.as_ref().is_some_and(|(cid, _)| *cid == call_id) {
                g.take()
            } else {
                None
            }
        };
        if let Some((_, z)) = old_zrtp {
            z.shutdown().await;
        }
    }

    async fn apply_transition(&self, out: TransitionOutput) {
        let ended_call_id = out.events.iter().find_map(|ev| match ev {
            CallEvent::CallEnded { call_id, .. } => Some(*call_id),
            _ => None,
        });
        let media_call_id = out.events.iter().find_map(|ev| match ev {
            CallEvent::MediaStateChanged { call_id, .. }
            | CallEvent::CameraStateChanged { call_id, .. } => Some(*call_id),
            _ => None,
        });
        self.execute_effects(out.effects, ended_call_id, media_call_id)
            .await;
        self.emit_events(out.events);
    }

    async fn execute_effects(
        &self,
        effects: Vec<CallEffect>,
        ended_call_id: Option<i64>,
        media_call_id: Option<i64>,
    ) {
        for eff in effects {
            match eff {
                CallEffect::StopMediaAndZrtp => {
                    if let Some(cid) = ended_call_id {
                        self.stop_media_and_zrtp_for_call(cid).await;
                    } else {
                        self.stop_media_and_zrtp().await;
                    }
                }
                CallEffect::SendAnswerAck408 { callee_id, call_id } => {
                    if let Err(err) = self.transport.answer_ack_408(&callee_id, call_id).await {
                        self.report_signaling_error(call_id, 408, "answerack(408)", err);
                    }
                }
                CallEffect::SendCancel405 {
                    to_id,
                    call_id,
                    call_type,
                } => {
                    if let Err(err) = self.transport.cancel_405(&to_id, call_id, call_type).await {
                        self.report_signaling_error(call_id, 405, "cancel(405)", err);
                    }
                }
                CallEffect::SendAnswerDecline402 {
                    caller_id,
                    call_id,
                    rtp_address,
                    rtcp_address,
                    session_id,
                } => {
                    let params = AnswerParams::decline_with_media(
                        caller_id,
                        call_id,
                        Some(rtp_address),
                        Some(rtcp_address),
                        Some(session_id),
                    );
                    if let Err(err) = self.transport.answer_402(&params).await {
                        self.report_signaling_error(call_id, 402, "decline(402)", err);
                    }
                }
                CallEffect::SendEndCall409 { uid_to, call_id } => {
                    if let Err(err) = self.transport.end_call_409(&uid_to, call_id).await {
                        self.report_signaling_error(call_id, 409, "endcall(409)", err);
                    }
                }
                CallEffect::SendLogEndCall406 {
                    call_id,
                    partner_id,
                    duration_secs,
                    role,
                } => {
                    if let Err(err) = self
                        .transport
                        .log_end_call_406(&LogEndCallParams {
                            call_id,
                            partner_id,
                            status: 0,
                            log_data: "{}".to_string(),
                            duration_secs,
                            role,
                        })
                        .await
                    {
                        self.report_signaling_error(call_id, 406, "logendcall(406)", err);
                    }
                }
                CallEffect::SendMediaConf12006 {
                    callee_id,
                    call_id,
                    is_on_mic,
                    is_on_cam,
                } => {
                    if let Err(err) = self
                        .transport
                        .set_media_conf_12006(&MediaConfParams {
                            callee_id,
                            call_id,
                            is_on_mic,
                            is_on_cam,
                            data: String::new(),
                        })
                        .await
                    {
                        self.report_signaling_error(call_id, 12006, "conf(12006)", err);
                    }
                }
                CallEffect::ApplyLocalMute(muted) => {
                    if let Some((cid, ref m)) = *self.media.lock().await {
                        if media_call_id.is_none_or(|target| target == cid) {
                            m.set_mic_muted(muted);
                        }
                    }
                }
                CallEffect::ApplyLocalCam(enabled) => {
                    if let Some((cid, ref aux)) = *self.aux_media.lock().await {
                        if media_call_id.is_none_or(|target| target == cid) {
                            aux.set_cam_enabled(enabled);
                        }
                    }
                }
                CallEffect::RequestRemoteKeyframe { call_id } => {
                    if let Some((cid, ref aux)) = *self.aux_media.lock().await {
                        if cid == call_id {
                            aux.request_remote_keyframe();
                        }
                    }
                }
                CallEffect::StartCallerMedia {
                    call_id,
                    rtp_ip,
                    rtcp_ip,
                    from_uid_u32,
                    sess_id,
                } => {
                    self.start_audio_on_active_zrtp(
                        call_id,
                        &rtp_ip,
                        &rtcp_ip,
                        from_uid_u32,
                        &sess_id,
                    )
                    .await;
                }
                CallEffect::ScheduleIncomingTimeout { call_id, secs } => {
                    let sm_c = self.sm.clone();
                    let tx_c = self.events_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(secs)).await;
                        let out = sm_c.lock().await.handle_incoming_ring_timeout(call_id);
                        for ev in out.events {
                            let _ = tx_c.send(ev);
                        }
                    });
                }
            }
        }
    }

    async fn start_audio_on_active_zrtp(
        &self,
        call_id: i64,
        rtp_ip: &str,
        rtcp_ip: &str,
        from_uid_u32: u32,
        sess_id: &str,
    ) {
        let Ok(rtp_target) = rtp_ip.parse::<SocketAddr>() else {
            let msg = format!("Địa chỉ RTP relay không hợp lệ: `{rtp_ip}`");
            eprintln!("[teza-call][engine] call_id={call_id}: {msg}");
            self.emit_events(vec![CallEvent::MediaWarning { call_id, message: msg }]);
            return;
        };
        let rtcp_target = rtcp_ip.parse::<SocketAddr>().ok();
        let distinct_rtcp = rtcp_target.is_some_and(|rt| rt != rtp_target);

        let (
            token,
            live_token_arc,
            live_rtcp_token_arc,
            media_attached,
            media_attached_notify,
            rtp_sock,
            std_rtp_sock,
            std_rtcp_sock,
            rtcp_inbox,
        ) = {
            let zrtp_guard = self.zrtp.lock().await;
            let Some((z_call_id, zrtp_sess)) = zrtp_guard.as_ref() else {
                return;
            };
            if *z_call_id != call_id {
                return;
            }
            zrtp_sess
                .ensure_target_registered(RelayServer::new(rtp_ip, rtcp_ip))
                .await;
            (
                zrtp_sess.token_for(rtp_ip),
                zrtp_sess.token_handle_for(rtp_ip),
                distinct_rtcp.then(|| zrtp_sess.token_handle_for(rtcp_ip)),
                zrtp_sess.media_attached_flag(),
                zrtp_sess.media_attached_notify(),
                zrtp_sess.rtp_socket(),
                zrtp_sess.std_rtp_socket(),
                zrtp_sess.std_rtcp_socket(),
                zrtp_sess.rtcp_inbox(),
            )
        };

        let (
            call_type,
            is_caller,
            partner_uid_u32,
            initial_mic_muted,
            initial_cam_enabled,
            still_active,
        ) = {
            let mut sm_g = self.sm.lock().await;
            if let Some(st) = sm_g.active_mut() {
                if st.call_id == call_id {
                    st.relay_token = token;
                    (
                        st.call_type,
                        st.is_caller,
                        st.partner_uid_u32,
                        st.mic_muted,
                        st.cam_enabled,
                        matches!(st.phase, CallPhase::Connected | CallPhase::Connecting),
                    )
                } else {
                    (CallType::DirectVoice, false, 0, false, false, false)
                }
            } else {
                (CallType::DirectVoice, false, 0, false, false, false)
            }
        };
        if !still_active {
            return;
        }

        // Send immediate Ping to lock in the selected relay address before media begins when
        // this server already acknowledged InitZRTP.
        if token != 0 {
            let ping = crate::zrtp::build_zrtp_ping_packet(from_uid_u32, token, sess_id);
            let _ = rtp_sock.send_to(&ping, rtp_target).await;
        }

        let existing_aux = {
            let g = self.aux_media.lock().await;
            g.as_ref()
                .and_then(|(cid, aux)| (*cid == call_id).then(|| aux.clone()))
        };
        if !self.config.enable_audio && existing_aux.is_none() && self.aux_media_hook.is_none() {
            return;
        }
        // Disarm `keepalive_task`'s RTP socket receiver BEFORE `aux_media_hook.on_media_start`
        // transmits or receives initial video/RTCP packets so those datagrams stay queued on the
        // UDP socket until `AudioMediaSession` starts draining it.
        let attach_guard =
            PendingMediaAttachGuard::acquire(media_attached.clone(), media_attached_notify.clone());
        tokio::task::yield_now().await;

        let mut created_aux = false;
        let aux_session = match existing_aux {
            Some(aux) => Some(aux),
            None => {
                let created = match self.aux_media_hook.clone() {
                    Some(hook) => {
                        let ctx = MediaTransportContext {
                            call_id,
                            call_type,
                            is_caller,
                            from_uid_u32,
                            partner_uid_u32,
                            ssrc: from_uid_u32,
                            rtp_target,
                            rtcp_target,
                            relay_token: token,
                            live_relay_token: Some(live_token_arc.clone()),
                            live_rtcp_relay_token: live_rtcp_token_arc.clone(),
                            udp_rtp_sock: std_rtp_sock.clone(),
                            udp_rtcp_sock: Some(std_rtcp_sock.clone()),
                            initial_cam_enabled,
                        };
                        tokio::task::spawn_blocking(move || hook.on_media_start(ctx))
                            .await
                            .ok()
                            .flatten()
                    }
                    None => None,
                };
                created_aux = created.is_some();
                created
            }
        };

        if !self.config.enable_audio && aux_session.is_none() {
            return;
        }
        let selected_devices = self.devices.lock().await.clone();
        let make_cfg = || AudioMediaConfig {
            rtp_target,
            rtcp_target,
            ssrc: from_uid_u32,
            relay_token: token,
            live_relay_token: Some(live_token_arc.clone()),
            live_rtcp_relay_token: live_rtcp_token_arc.clone(),
            media_attached: Some(media_attached.clone()),
            media_attached_notify: Some(media_attached_notify.clone()),
            initial_mic_muted,
            udp_rtp_sock: std_rtp_sock.clone(),
            udp_rtcp_sock: Some(std_rtcp_sock.clone()),
            rtcp_inbox: Some(rtcp_inbox.clone()),
            devices: selected_devices.clone(),
            aux_session: aux_session.clone(),
        };

        let (start_res, pump_only) = if self.config.enable_audio {
            let cfg = make_cfg();
            #[cfg(test)]
            let fault = self.audio_fault;
            let res = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                {
                    AudioMediaSession::start_with_fault(cfg, fault)
                }
                #[cfg(not(test))]
                {
                    AudioMediaSession::start(cfg)
                }
            })
            .await
            .map_err(|e| CallError::Audio(format!("Lỗi tác vụ khởi động audio: {e}")))
            .and_then(|r| r);
            match res {
                Ok(s) => (Ok(s), false),
                Err(err) if aux_session.is_some() => {
                    let msg = format!("Không khởi động được audio media: {err}");
                    eprintln!("[teza-call][engine] call_id={call_id}: {msg}");
                    self.emit_events(vec![CallEvent::MediaWarning {
                        call_id,
                        message: msg,
                    }]);
                    media_attached.store(true, Ordering::Release);
                    media_attached_notify.notify_one();
                    tokio::task::yield_now().await;
                    let pump_cfg = make_cfg();
                    let pump_res = tokio::task::spawn_blocking(move || {
                        AudioMediaSession::start_aux_pump_only(pump_cfg)
                    })
                    .await
                    .map_err(|e| CallError::Audio(format!("Lỗi tác vụ khởi động UDP aux pump: {e}")))
                    .and_then(|r| r);
                    (pump_res, true)
                }
                Err(err) => (Err(err), false),
            }
        } else {
            let pump_cfg = make_cfg();
            let pump_res = tokio::task::spawn_blocking(move || {
                AudioMediaSession::start_aux_pump_only(pump_cfg)
            })
            .await
            .map_err(|e| CallError::Audio(format!("Lỗi tác vụ khởi động UDP aux pump: {e}")))
            .and_then(|r| r);
            (pump_res, true)
        };

        match start_res {
            Ok(session) => {
                attach_guard.hand_off();
                let telem = session.telemetry();
                let receive_only = !pump_only && !telem.has_input_device;
                let virtual_warn = (!pump_only)
                    .then(|| format_virtual_audio_warning(&telem))
                    .flatten();
                // Re-verify the call wasn't ended or replaced while OS audio devices were opening,
                // re-syncing latest mute/cam flags and storing `(call_id, session)` / `(call_id, aux)`
                // atomically under `sm` so toggles made during `Connecting -> Connected` are never lost.
                let (prev, prev_aux) = {
                    let sm_g = self.sm.lock().await;
                    let live_flags = sm_g.active().and_then(|st| {
                        (st.call_id == call_id
                            && matches!(st.phase, CallPhase::Connected | CallPhase::Connecting))
                        .then_some((st.mic_muted, st.cam_enabled))
                    });
                    let Some((cur_mic_muted, cur_cam_enabled)) = live_flags else {
                        drop(sm_g);
                        stop_audio_session_blocking(session).await;
                        if created_aux {
                            if let Some(aux) = aux_session {
                                stop_aux_session_blocking(aux).await;
                            }
                        }
                        return;
                    };
                    session.set_mic_muted(cur_mic_muted);
                    if let Some(ref aux) = aux_session {
                        aux.set_cam_enabled(cur_cam_enabled);
                    }
                    let prev_m = self.media.lock().await.replace((call_id, session));
                    let prev_aux_slot = if created_aux {
                        if let Some(aux) = aux_session.clone() {
                            self.aux_media.lock().await.replace((call_id, aux))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    (prev_m, prev_aux_slot)
                };
                if let Some((_, old)) = prev {
                    stop_audio_session_blocking(old).await;
                }
                if let Some((_, old_aux)) = prev_aux {
                    stop_aux_session_blocking(old_aux).await;
                }
                if receive_only {
                    let msg =
                        "Không phát hiện micro — đang chạy chế độ chỉ nghe (receive-only)."
                            .to_string();
                    eprintln!("[teza-call][engine] call_id={call_id}: {msg}");
                    self.emit_events(vec![CallEvent::MediaWarning {
                        call_id,
                        message: msg,
                    }]);
                }
                if let Some(msg) = virtual_warn {
                    eprintln!("[teza-call][engine] call_id={call_id}: {msg}");
                    self.emit_events(vec![CallEvent::MediaWarning {
                        call_id,
                        message: msg,
                    }]);
                }
            }
            Err(err) => {
                if created_aux {
                    if let Some(aux) = aux_session {
                        stop_aux_session_blocking(aux).await;
                    }
                }
                let msg = format!("Không khởi động được audio media: {err}");
                eprintln!("[teza-call][engine] call_id={call_id}: {msg}");
                self.emit_events(vec![CallEvent::MediaWarning {
                    call_id,
                    message: msg,
                }]);
            }
        }
    }

    /// Initiate an outgoing 1-1 call (`401` -> UDP ZRTP probe -> `416`).
    pub async fn make_call(
        &self,
        partner_id: impl Into<String>,
        partner_name: impl Into<String>,
        call_type: CallType,
    ) -> CallResult<i64> {
        self.transport.check_callable()?;
        let partner_id = partner_id.into();
        let partner_name = partner_name.into();
        let req_call_id = next_req_call_id();

        let begin_out = self.sm.lock().await.begin_outgoing(
            req_call_id,
            partner_id.clone(),
            partner_name,
            call_type,
            now_ms(),
        )?;
        self.emit_events(begin_out.events);

        // Step 1: 401 requestcall
        let resp_401 = match self
            .transport
            .request_call_401(&RequestCallParams::initial(
                partner_id.clone(),
                req_call_id,
                call_type,
            ))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                let out = self
                    .sm
                    .lock()
                    .await
                    .fail_outgoing(req_call_id, CallEndReason::Error(e.to_string()));
                self.apply_transition(out).await;
                return Err(e);
            }
        };

        let status_401 = resp_401
            .get("status")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .unwrap_or(0);
        if status_401 != 0 {
            let msg = response_error_msg(&resp_401, "Không thể khởi tạo cuộc gọi");
            let out = self.sm.lock().await.fail_outgoing(
                req_call_id,
                CallEndReason::RemoteRejected {
                    code: status_401,
                    message: msg.clone(),
                },
            );
            self.apply_transition(out).await;
            return Err(CallError::Signaling {
                code: status_401,
                message: msg,
            });
        }

        let real_call_id = resp_401
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or(req_call_id);
        let from_uid_u32 = resp_401
            .get("fromId")
            .or_else(|| resp_401.get("callerId"))
            .and_then(value_as_u32)
            .unwrap_or_else(|| self.transport.self_uid_u32());
        let partner_uid_u32 = resp_401
            .get("toId")
            .or_else(|| resp_401.get("calleeId"))
            .and_then(value_as_u32)
            .unwrap_or_else(|| uid_str_to_u32(&partner_id));
        let mut sess_id = resp_401
            .get("sessId")
            .or_else(|| resp_401.get("session"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if sess_id.is_empty() {
            match generate_call_session() {
                Ok(s) => sess_id = s,
                Err(e) => {
                    let out = self
                        .sm
                        .lock()
                        .await
                        .fail_outgoing(req_call_id, CallEndReason::Error(e.to_string()));
                    self.apply_transition(out).await;
                    return Err(e);
                }
            }
        }

        // Immediately bind `real_call_id` into state machine so any concurrent cancel sends `405`
        // with `real_call_id` and any early WS `ring_ring`/`answer` matches before `416` returns.
        let bound = self.sm.lock().await.bind_outgoing_401(
            req_call_id,
            real_call_id,
            from_uid_u32,
            partner_uid_u32,
            sess_id.clone(),
            resp_401.clone(),
        );
        if !bound {
            // Cancelled concurrently while awaiting `401`: also send best-effort 405 for `real_call_id`.
            if real_call_id != req_call_id {
                if let Err(err) = self
                    .transport
                    .cancel_405(&partner_id, real_call_id, call_type)
                    .await
                {
                    self.report_signaling_error(real_call_id, 405, "cancel(405)", err);
                }
            }
            return Err(CallError::InvalidState(
                "Cuộc gọi đã bị hủy trong lúc khởi tạo".to_string(),
            ));
        }

        // Step 2: UDP ZRTP relay probe & keepalive
        let zrtp_sess = match ZrtpRelaySession::start_from_payload_with_trace(
            &resp_401,
            from_uid_u32,
            partner_uid_u32,
            real_call_id as u32,
            true,
            &sess_id,
            self.config.trace.clone(),
        )
        .await
        {
            Ok(z) => z,
            Err(e) => {
                let out = self
                    .sm
                    .lock()
                    .await
                    .fail_outgoing(real_call_id, CallEndReason::Error(e.to_string()));
                self.apply_transition(out).await;
                return Err(e);
            }
        };

        let probe_snap = zrtp_sess.probe().clone();
        let invite = build_caller_invite_fields(
            &resp_401,
            call_type.is_video(),
            Some(probe_snap.local_port),
            Some(&probe_snap),
        );
        let effective_sess = if invite.sess_id.is_empty() {
            sess_id
        } else {
            invite.sess_id
        };
        let actual_token = zrtp_sess.token_for(&invite.rtp_ip);

        // Store ZRTP + relay metadata atomically under `sm` -> `zrtp` BEFORE `416` fires so an
        // early WS `answer` arriving during `416` already has sockets ready, and a stale dial can
        // never clobber a newer call's ZRTP handle.
        let prev_zrtp = {
            let mut sm_g = self.sm.lock().await;
            let Some(st) = sm_g.active_mut() else {
                drop(sm_g);
                zrtp_sess.shutdown().await;
                return Err(CallError::InvalidState(
                    "Cuộc gọi đã bị hủy trong lúc dò relay UDP".to_string(),
                ));
            };
            if !(st.is_caller
                && st.call_id == real_call_id
                && matches!(st.phase, CallPhase::Dialing | CallPhase::RingingOutgoing))
            {
                drop(sm_g);
                zrtp_sess.shutdown().await;
                return Err(CallError::InvalidState(
                    "Cuộc gọi đã bị hủy trong lúc dò relay UDP".to_string(),
                ));
            }
            st.sess_id = effective_sess.clone();
            st.rtp_ip = invite.rtp_ip.clone();
            st.rtcp_ip = invite.rtcp_ip.clone();
            st.relay_token = actual_token;
            self.zrtp.lock().await.replace((real_call_id, zrtp_sess))
        };
        if let Some((_, old)) = prev_zrtp {
            old.shutdown().await;
        }

        // Step 3: 416 request (media + ringing)
        let req_416 = RequestMediaParams {
            callee_id: partner_id,
            call_id: real_call_id,
            rtp_address: invite.rtp_ip.clone(),
            rtcp_address: invite.rtcp_ip.clone(),
            codec: invite.codec,
            session: effective_sess.clone(),
            extend_data: Some(invite.extend_data),
        };
        let resp_416 = match self.transport.request_416(&req_416).await {
            Ok(v) => v,
            Err(e) => {
                let (out, already_live) = {
                    let mut sm_g = self.sm.lock().await;
                    let already_live = sm_g.active().is_some_and(|st| {
                        st.is_caller
                            && st.call_id == real_call_id
                            && matches!(st.phase, CallPhase::RingingOutgoing | CallPhase::Connected)
                    });
                    let out =
                        sm_g.fail_outgoing(real_call_id, CallEndReason::Error(e.to_string()));
                    (out, already_live)
                };
                self.apply_transition(out).await;
                if already_live {
                    return Ok(real_call_id);
                }
                self.stop_media_and_zrtp_for_call(real_call_id).await;
                return Err(e);
            }
        };
        let status_416 = resp_416
            .get("status")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .unwrap_or(0);
        if status_416 != 0 {
            let msg = response_error_msg(&resp_416, "Không thể đổ chuông tới người nhận");
            let (out, already_live) = {
                let mut sm_g = self.sm.lock().await;
                let already_live = sm_g.active().is_some_and(|st| {
                    st.is_caller
                        && st.call_id == real_call_id
                        && matches!(st.phase, CallPhase::RingingOutgoing | CallPhase::Connected)
                });
                let out = sm_g.fail_outgoing(
                    real_call_id,
                    CallEndReason::RemoteRejected {
                        code: status_416,
                        message: msg.clone(),
                    },
                );
                (out, already_live)
            };
            self.apply_transition(out).await;
            if already_live {
                return Ok(real_call_id);
            }
            self.stop_media_and_zrtp_for_call(real_call_id).await;
            return Err(CallError::Signaling {
                code: status_416,
                message: msg,
            });
        }

        // Final atomic phase check + transition under a single `sm` lock after `416` returns:
        // - Still `Dialing` -> transition to `RingingOutgoing` (no TOCTOU window!).
        // - Already `RingingOutgoing` / `Connected` (early WS event during `416`) -> keep it!
        // - Absent or different call -> cancelled concurrently; tear down ONLY `real_call_id`.
        let post_result = {
            let mut sm_g = self.sm.lock().await;
            let phase = sm_g
                .active()
                .filter(|st| st.is_caller && st.call_id == real_call_id)
                .map(|st| st.phase);
            match phase {
                Some(CallPhase::Dialing) => {
                    let ring_out = sm_g.mark_outgoing_ringing(
                        real_call_id,
                        from_uid_u32,
                        partner_uid_u32,
                        effective_sess,
                        invite.rtp_ip,
                        invite.rtcp_ip,
                        actual_token,
                    );
                    Ok(Some(ring_out.events))
                }
                Some(CallPhase::RingingOutgoing | CallPhase::Connected) => Ok(None),
                _ => Err(()),
            }
        };
        match post_result {
            Ok(Some(events)) => {
                self.emit_events(events);
                Ok(real_call_id)
            }
            Ok(None) => Ok(real_call_id),
            Err(()) => {
                self.stop_media_and_zrtp_for_call(real_call_id).await;
                Err(CallError::InvalidState(
                    "Cuộc gọi đã bị hủy trước khi đổ chuông".to_string(),
                ))
            }
        }
    }

    /// Accept (`accept == true`) or decline (`accept == false`) the currently ringing incoming call.
    pub async fn answer_call(&self, accept: bool) -> CallResult<()> {
        self.answer_call_by_id(None, accept).await
    }

    /// Accept (`accept == true`) or decline (`accept == false`) a specific incoming `call_id`
    /// (or whichever call is currently ringing when `target_call_id == None`).
    pub async fn answer_call_by_id(
        &self,
        target_call_id: Option<i64>,
        accept: bool,
    ) -> CallResult<()> {
        self.transport.check_callable()?;
        if !accept {
            let out = self.sm.lock().await.decline_incoming(target_call_id);
            self.apply_transition(out).await;
            return Ok(());
        }

        let Some((rec, conn_out)) = self.sm.lock().await.begin_accept_incoming(target_call_id)?
        else {
            // Already dismissed by another device: do nothing!
            return Ok(());
        };
        self.emit_events(conn_out.events);

        // Passive ringing -> send 407 ringing ONLY now that the user explicitly accepted
        if let Err(err) = self
            .transport
            .ring_ring_407(&rec.partner_id, rec.call_id)
            .await
        {
            self.report_signaling_error(rec.call_id, 407, "ringring(407)", err);
        }

        let zrtp_sess = match ZrtpRelaySession::start_from_payload_with_trace(
            &rec.invite_payload,
            rec.from_uid_u32,
            rec.partner_uid_u32,
            rec.call_id as u32,
            false,
            &rec.sess_id,
            self.config.trace.clone(),
        )
        .await
        {
            Ok(z) => z,
            Err(e) => {
                let out = self
                    .sm
                    .lock()
                    .await
                    .fail_connecting(rec.call_id, CallEndReason::Error(e.to_string()));
                self.apply_transition(out).await;
                self.stop_media_and_zrtp_for_call(rec.call_id).await;
                return Err(e);
            }
        };

        // Guard against race: if mobile answered/ended during the 407 + 280ms probe window,
        // shut down UDP cleanly and NEVER start audio or send 402!
        let still_connecting = {
            let sm_g = self.sm.lock().await;
            !sm_g.is_dismissed_by_peer(rec.call_id)
                && sm_g.active().is_some_and(|st| {
                    !st.is_caller
                        && st.call_id == rec.call_id
                        && st.phase == CallPhase::Connecting
                })
        };
        if !still_connecting {
            zrtp_sess.shutdown().await;
            return Ok(());
        }

        let probe_snap = zrtp_sess.probe().clone();
        let inv_fields = build_caller_invite_fields(
            &rec.invite_payload,
            rec.call_type.is_video(),
            Some(probe_snap.local_port),
            Some(&probe_snap),
        );
        let candidate_servers = collect_relay_servers(&rec.invite_payload);
        let selected_srv = probe_snap
            .selected_rtp
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|sel| candidate_servers.iter().find(|s| s.rtp == sel))
            .or_else(|| candidate_servers.first());
        let rtp_ip = selected_srv
            .map(|s| s.rtp.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| probe_snap.selected_rtp.clone().filter(|s| !s.is_empty()))
            .or_else(|| (!inv_fields.rtp_ip.is_empty()).then_some(inv_fields.rtp_ip))
            .unwrap_or_else(|| rec.rtp_ip.clone());
        let rtcp_ip = selected_srv
            .map(|s| s.rtcp.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| (!inv_fields.rtcp_ip.is_empty()).then_some(inv_fields.rtcp_ip))
            .or_else(|| (!rec.rtcp_ip.is_empty()).then_some(rec.rtcp_ip.clone()))
            .unwrap_or_else(|| rtp_ip.clone());
        let sess_id = if !rec.sess_id.is_empty() {
            rec.sess_id.clone()
        } else {
            inv_fields.sess_id
        };
        let callee_ext = build_callee_extend_data(
            &rtp_ip,
            &rtcp_ip,
            rec.call_type.is_video(),
            Some(probe_snap.local_port),
            Some(&probe_snap),
        );
        let actual_token = zrtp_sess.token_for(&rtp_ip);

        // Atomically verify `rec.call_id` is still connecting and store `(rec.call_id, zrtp_sess)`
        // under `sm` -> `zrtp` so a stale accept can never overwrite a newer call's ZRTP handle.
        let prev_zrtp = {
            let sm_g = self.sm.lock().await;
            let live = !sm_g.is_dismissed_by_peer(rec.call_id)
                && sm_g.active().is_some_and(|st| {
                    !st.is_caller
                        && st.call_id == rec.call_id
                        && st.phase == CallPhase::Connecting
                });
            if !live {
                drop(sm_g);
                zrtp_sess.shutdown().await;
                return Ok(());
            }
            self.zrtp.lock().await.replace((rec.call_id, zrtp_sess))
        };
        if let Some((_, old)) = prev_zrtp {
            old.shutdown().await;
        }

        let ans_params = AnswerParams {
            caller_id: rec.partner_id.clone(),
            call_id: rec.call_id,
            status: 0,
            codec: Some(OPUS_16K_CODEC_JSON.to_string()),
            rtp_address: Some(rtp_ip.clone()),
            rtcp_address: Some(rtcp_ip.clone()),
            session_id: Some(sess_id.clone()),
            extend_data: Some(callee_ext),
            imei: None,
        };
        let resp_402 = match self.transport.answer_402(&ans_params).await {
            Ok(v) => v,
            Err(e) => {
                let out = self
                    .sm
                    .lock()
                    .await
                    .fail_connecting(rec.call_id, CallEndReason::Error(e.to_string()));
                self.apply_transition(out).await;
                self.stop_media_and_zrtp_for_call(rec.call_id).await;
                return Err(e);
            }
        };
        let status_402 = resp_402
            .get("status")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .unwrap_or(0);
        if status_402 != 0 {
            let msg = response_error_msg(&resp_402, "Không thể trả lời cuộc gọi");
            let out = self.sm.lock().await.fail_connecting(
                rec.call_id,
                CallEndReason::RemoteRejected {
                    code: status_402,
                    message: msg.clone(),
                },
            );
            self.apply_transition(out).await;
            self.stop_media_and_zrtp_for_call(rec.call_id).await;
            return Err(CallError::Signaling {
                code: status_402,
                message: msg,
            });
        }

        let done_out = self.sm.lock().await.complete_callee_connected(
            rec.call_id,
            sess_id.clone(),
            rtp_ip.clone(),
            rtcp_ip.clone(),
            actual_token,
            now_ms(),
        );
        let transitioned = done_out
            .events
            .iter()
            .any(|e| matches!(e, CallEvent::Connected { .. }));
        if !transitioned {
            // Dismissed/cancelled while `402` was in flight: tear down ONLY `rec.call_id` so a
            // newer active call's ZRTP/media session is never disturbed.
            self.stop_media_and_zrtp_for_call(rec.call_id).await;
            return Ok(());
        }

        self.start_audio_on_active_zrtp(rec.call_id, &rtp_ip, &rtcp_ip, rec.from_uid_u32, &sess_id)
            .await;
        let still_connected = self.sm.lock().await.active().is_some_and(|st| {
            st.call_id == rec.call_id && st.phase == CallPhase::Connected
        });
        if still_connected {
            self.emit_events(done_out.events);
        }
        Ok(())
    }

    /// Toggle local microphone mute and notify the peer via `12006` `/api/voicecall/conf`.
    pub async fn set_mic_muted(&self, muted: bool) -> CallResult<()> {
        self.transport.check_callable()?;
        let out = self.sm.lock().await.set_mic_muted(muted);
        self.apply_transition(out).await;
        Ok(())
    }

    /// Toggle local camera transmission and notify the peer via `12006` `/api/voicecall/conf`.
    pub async fn set_cam_enabled(&self, enabled: bool) -> CallResult<()> {
        self.transport.check_callable()?;
        let out = self.sm.lock().await.set_cam_enabled(enabled);
        self.apply_transition(out).await;
        Ok(())
    }

    /// End or cancel the current call cleanly.
    pub async fn end_call(&self) -> CallResult<()> {
        self.transport.check_callable()?;
        let out = self.sm.lock().await.end_call(now_ms());
        self.apply_transition(out).await;
        Ok(())
    }

    /// Return the currently configured audio input/output device selection.
    pub async fn devices(&self) -> AudioDeviceSelection {
        self.devices.lock().await.clone()
    }

    /// Update the preferred audio input/output devices.
    ///
    /// If a call is currently `Connected` with an active audio media session, the audio stream
    /// restarts immediately using the newly selected devices while preserving the live ZRTP relay
    /// session and current microphone mute state.
    pub async fn set_devices(&self, devices: AudioDeviceSelection) {
        *self.devices.lock().await = devices;
        let restart_params = {
            let sm_g = self.sm.lock().await;
            sm_g.active().and_then(|st| {
                (st.phase == CallPhase::Connected && !st.rtp_ip.is_empty()).then(|| {
                    (
                        st.call_id,
                        st.rtp_ip.clone(),
                        st.rtcp_ip.clone(),
                        st.from_uid_u32,
                        st.sess_id.clone(),
                    )
                })
            })
        };
        if let Some((call_id, rtp_ip, rtcp_ip, from_uid_u32, sess_id)) = restart_params {
            let old_media = {
                let mut g = self.media.lock().await;
                if g.as_ref().is_some_and(|(cid, _)| *cid == call_id) {
                    g.take()
                } else {
                    None
                }
            };
            if let Some((_, m)) = old_media {
                stop_audio_session_blocking(m).await;
                self.start_audio_on_active_zrtp(call_id, &rtp_ip, &rtcp_ip, from_uid_u32, &sess_id)
                    .await;
            }
        }
    }

    /// Force-stop any active call, ZRTP relay, and local audio streams without sending HTTP
    /// signaling requests (used when shutting down, logging out, or switching into Relay mode).
    pub async fn force_stop(&self, reason: Option<CallEndReason>) {
        let ended = {
            let mut sm_g = self.sm.lock().await;
            let taken = sm_g.active.take();
            if let Some(ref st) = taken {
                sm_g.mark_dismissed(st.call_id);
            }
            taken
        };
        self.stop_media_and_zrtp().await;
        if let Some(st) = ended {
            let duration_secs = match st.connected_at_ms {
                Some(ts) => now_ms().saturating_sub(ts).max(0) as u64 / 1000,
                None => 0,
            };
            self.emit_events(vec![
                CallEvent::CallEnded {
                    call_id: st.call_id,
                    partner_id: st.partner_id,
                    duration_secs,
                    reason: reason.unwrap_or(CallEndReason::LocalHangup),
                },
                CallEvent::StateChanged {
                    phase: CallPhase::Idle,
                    call_id: Some(st.call_id),
                },
            ]);
        }
    }

    /// Feed a parsed [`VoipControlEvent`] (from `zalo-core` WS or `lp_control` polling) into the engine.
    pub async fn handle_voip_event(&self, ev: &VoipControlEvent) -> CallResult<()> {
        self.transport.check_callable()?;
        let (out, next_phase) = {
            let mut sm_g = self.sm.lock().await;
            let out = sm_g.handle_voip_event(ev, now_ms());
            (out, sm_g.phase())
        };
        emit_trace(self.config.trace.as_ref(), || {
            format!(
                "[ENG VOIP] act={} call_id={} status={:?} -> phase={next_phase:?} events={} effects={} data={}",
                ev.act,
                ev.call_id(),
                ev.status(),
                out.events.len(),
                out.effects.len(),
                format_trace_json(&ev.data, 400)
            )
        });
        self.apply_transition(out).await;
        Ok(())
    }
}

/// Convenience helper for converting a raw JSON VoIP event `{ act, data }` into [`VoipControlEvent`].
pub fn voip_event_from_parts(act: impl Into<String>, data: Value) -> VoipControlEvent {
    VoipControlEvent {
        act: act.into(),
        data,
    }
}

/// Helper to build a synthetic `request` VoIP event JSON payload (useful in tests & `call_lab`).
pub fn make_test_invite_event(call_id: i64, caller_id: &str, caller_name: &str, ts_ms: i64) -> VoipControlEvent {
    VoipControlEvent {
        act: "request".to_string(),
        data: json!({
            "callId": call_id,
            "uidN": caller_id,
            "uidFrom": caller_id,
            "Dname": caller_name,
            "ts": ts_ms,
            "params": r#"{"sessId":"test_sess","rtpIP":"127.0.0.1:4200","Dname":"Test Caller"}"#,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zalo_core::relay::{RelayConfig, RelayMode};

    #[test]
    fn outgoing_call_happy_path_dial_ring_answer_mute_end() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_000_000_i64;

        let o1 = sm
            .begin_outgoing(464_564_007, "2000".into(), "Alice".into(), CallType::DirectVoice, t0)
            .unwrap();
        assert_eq!(sm.phase(), CallPhase::Dialing);
        assert!(matches!(
            o1.events.first(),
            Some(CallEvent::StateChanged { phase: CallPhase::Dialing, .. })
        ));

        // Cannot start a second call concurrently
        assert!(sm
            .begin_outgoing(999, "3000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .is_err());

        let o2 = sm.mark_outgoing_ringing(
            464_564_007,
            1000,
            2000,
            "sess_1".into(),
            "42.113.206.4:10490".into(),
            "42.113.206.4:12490".into(),
            0xABCD,
        );
        assert_eq!(sm.phase(), CallPhase::RingingOutgoing);
        assert!(!o2.events.is_empty());

        // Remote ring_ring
        let ring_ev = voip_event_from_parts("ring_ring", json!({ "callId": 464_564_007 }));
        let o_ring = sm.handle_voip_event(&ring_ev, t0 + 500);
        assert!(o_ring
            .events
            .iter()
            .any(|e| matches!(e, CallEvent::RemoteRinging { call_id: 464_564_007 })));

        // Benign early status=5 with extendData is ignored
        let s5_ev = voip_event_from_parts(
            "answer",
            json!({
                "callId": 464_564_007,
                "status": 5,
                "params": r#"{"extendData":"{\"callType\":0}"}"#
            }),
        );
        let o_s5 = sm.handle_voip_event(&s5_ev, t0 + 1000);
        assert_eq!(sm.phase(), CallPhase::RingingOutgoing);
        assert!(o_s5.events.is_empty());

        // Callee answers (status=0) and redirects to rtpSerIp
        let ans_ev = voip_event_from_parts(
            "answer",
            json!({
                "callId": 464_564_007,
                "status": 0,
                "params": r#"{"rtpSerIp":"171.244.128.62:4200"}"#
            }),
        );
        let o_ans = sm.handle_voip_event(&ans_ev, t0 + 2000);
        assert_eq!(sm.phase(), CallPhase::Connected);
        assert!(o_ans.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendAnswerAck408 { callee_id, call_id: 464_564_007 } if callee_id == "2000"
        )));
        assert!(o_ans.effects.iter().any(|e| matches!(
            e,
            CallEffect::StartCallerMedia { rtp_ip, .. } if rtp_ip == "171.244.128.62:4200"
        )));

        // Toggle mute
        let o_mute = sm.set_mic_muted(true);
        assert!(o_mute.effects.contains(&CallEffect::ApplyLocalMute(true)));
        assert!(o_mute.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendMediaConf12006 { is_on_mic: false, is_on_cam: false, .. }
        )));

        // Hangup after 15 seconds sends 409 + 406 with role=1 (caller)
        let o_end = sm.end_call(t0 + 17_000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o_end.effects.contains(&CallEffect::StopMediaAndZrtp));
        assert!(o_end.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendEndCall409 { uid_to, call_id: 464_564_007 } if uid_to == "2000"
        )));
        assert!(o_end.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendLogEndCall406 { duration_secs: 15, role: 1, .. }
        )));
    }

    #[test]
    fn outgoing_cancel_before_connect_sends_405_not_409() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_000_000_i64;
        sm.begin_outgoing(555, "2000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .unwrap();
        let out = sm.end_call(t0 + 3000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(out.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendCancel405 { to_id, call_id: 555, call_type: CallType::DirectVoice }
                if to_id == "2000"
        )));
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendEndCall409 { .. })));
    }

    #[test]
    fn incoming_passive_ringing_benign_status5_and_multi_device_dismissal_never_sends_402() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let now = 1_700_000_000_000_i64;

        // 1. Incoming request enters passive ringing (NO 407, NO 402)
        let invite = make_test_invite_event(998_877, "1000000000000000002", "Caller", now);
        let o_inv = sm.handle_voip_event(&invite, now);
        assert_eq!(sm.phase(), CallPhase::IncomingRinging);
        assert!(o_inv.events.iter().any(|e| matches!(
            e,
            CallEvent::IncomingCall { call_id: 998_877, .. }
        )));
        assert!(!o_inv
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendAnswerDecline402 { .. })));

        // 2. Benign status=5 while ringing MUST NOT dismiss or connect
        let s5 = voip_event_from_parts(
            "answer",
            json!({ "callId": 998_877, "status": 5, "uidFrom": "1000000000000000002" }),
        );
        let o_s5 = sm.handle_voip_event(&s5, now + 500);
        assert_eq!(sm.phase(), CallPhase::IncomingRinging);
        assert!(o_s5.events.is_empty());

        // 3. Answered on mobile (status=0) dismisses ringing and records `dismissed_by_peer`
        let mobile_ans = voip_event_from_parts(
            "answer",
            json!({ "callId": 998_877, "status": 0, "uidFrom": "1000000000000000002" }),
        );
        let o_dis = sm.handle_voip_event(&mobile_ans, now + 1500);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(sm.is_dismissed_by_peer(998_877));
        assert!(o_dis.events.iter().any(|e| matches!(
            e,
            CallEvent::DismissedByOtherDevice {
                call_id: 998_877,
                answered_elsewhere: true
            }
        )));
        assert!(!o_dis
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendAnswerDecline402 { .. })));

        // 4. Subsequent local decline OR accept OR re-invite for dismissed call_id MUST NOT send 402 or re-ring!
        let o_late_decline = sm.decline_incoming(Some(998_877));
        assert!(o_late_decline.effects.is_empty());
        assert!(o_late_decline.events.is_empty());

        let late_accept = sm.begin_accept_incoming(Some(998_877)).unwrap();
        assert!(late_accept.is_none());

        let o_reinvite = sm.handle_voip_event(&invite, now + 2000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o_reinvite.events.is_empty());
    }

    #[test]
    fn incoming_local_decline_sends_402_when_not_dismissed_and_stale_invite_ignored() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let now = 1_700_000_100_000_i64;

        // Stale invite (> 90s old) is ignored
        let stale = make_test_invite_event(111, "2000", "Old", now - 120_000);
        let o_stale = sm.handle_voip_event(&stale, now);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o_stale.events.is_empty());

        // Fresh invite -> local decline DOES send 402
        let fresh = make_test_invite_event(222, "2000", "Fresh", now);
        let _ = sm.handle_voip_event(&fresh, now);
        assert_eq!(sm.phase(), CallPhase::IncomingRinging);

        let o_dec = sm.decline_incoming(Some(222));
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o_dec.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendAnswerDecline402 { caller_id, call_id: 222, .. } if caller_id == "2000"
        )));
    }

    #[test]
    fn incoming_ring_timeout_dismisses_without_sending_402() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let now = 1_700_000_000_000_i64;
        let fresh = make_test_invite_event(333, "2000", "Caller", now);
        let _ = sm.handle_voip_event(&fresh, now);
        let o_to = sm.handle_incoming_ring_timeout(333);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(sm.is_dismissed_by_peer(333));
        assert!(!o_to
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendAnswerDecline402 { .. })));
        assert!(o_to.events.iter().any(|e| matches!(
            e,
            CallEvent::CallEnded {
                call_id: 333,
                reason: CallEndReason::RingTimeout,
                ..
            }
        )));
    }

    #[test]
    fn call_engine_rejects_relay_enabled_session() {
        let mut sess = Session::new().unwrap();
        sess.set_zpw_sek("1.1000.tok".into());
        sess.set_zpw_enk("0123456789abcdef0123456789abcdef");
        sess.set_relay(RelayConfig {
            enabled: true,
            mode: RelayMode::Http,
            host: "10.0.0.1".into(),
            port: 8080,
            username: String::new(),
            password: String::new(),
            route_webview_media: true,
        })
        .unwrap();

        let err = match CallEngine::new(sess, CallEngineConfig { enable_audio: false, ..Default::default() }) {
            Ok(_) => panic!("expected RelayUnsupported error"),
            Err(e) => e,
        };
        assert!(err.is_relay_unsupported());
    }

    #[tokio::test]
    async fn call_engine_async_passive_ringing_and_multi_device_dismissal() {
        let mut sess = Session::new().unwrap();
        sess.set_zpw_sek("1.1000.tok".into());
        sess.set_zpw_enk("0123456789abcdef0123456789abcdef");
        sess.profile.uid = "1000".into();

        let engine = CallEngine::new(
            sess,
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rx = engine.subscribe();

        // 1. Incoming request arrives -> passive ringing
        let invite = make_test_invite_event(777_888, "2000000000000000002", "Alice", now_ms());
        engine.handle_voip_event(&invite).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::IncomingRinging);

        // 2. Benign status=5 -> stays IncomingRinging
        let s5 = voip_event_from_parts(
            "answer",
            json!({ "callId": 777_888, "status": 5, "uidFrom": "2000000000000000002" }),
        );
        engine.handle_voip_event(&s5).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::IncomingRinging);

        // 3. Answered on mobile -> Idle + DismissedByOtherDevice
        let mobile = voip_event_from_parts(
            "answer",
            json!({ "callId": 777_888, "status": 0, "uidFrom": "2000000000000000002" }),
        );
        engine.handle_voip_event(&mobile).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);

        // 4. Subsequent decline / end_call is a pure local no-op (never sends 402!)
        engine.answer_call(false).await.unwrap();
        engine.end_call().await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);

        // 5. Re-invite for dismissed call_id is ignored
        engine.handle_voip_event(&invite).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);

        let mut saw_incoming = false;
        let mut saw_dismissed = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                CallEvent::IncomingCall { call_id: 777_888, .. } => saw_incoming = true,
                CallEvent::DismissedByOtherDevice {
                    call_id: 777_888,
                    answered_elsewhere: true,
                } => saw_dismissed = true,
                _ => {}
            }
        }
        assert!(saw_incoming);
        assert!(saw_dismissed);
    }

    #[test]
    fn outgoing_rejection_and_remote_end_transition_to_idle() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_000_000_i64;
        sm.begin_outgoing(600, "2000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .unwrap();
        let _ = sm.mark_outgoing_ringing(
            600,
            1000,
            2000,
            "s".into(),
            "1.2.3.4:1000".into(),
            "1.2.3.4:1001".into(),
            1,
        );
        let rej = voip_event_from_parts("answer", json!({ "callId": 600, "status": 3 }));
        let o_rej = sm.handle_voip_event(&rej, t0 + 1000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o_rej.events.iter().any(|e| matches!(
            e,
            CallEvent::CallEnded {
                call_id: 600,
                reason: CallEndReason::RemoteRejected { code: 3, .. },
                ..
            }
        )));
    }

    #[test]
    fn self_device_answer_while_idle_never_rings_ghost_call_and_dismisses_late_request() {
        let mut sm = CallStateMachine::new("1000000000000000001", 435_550_501);
        let now = 1_700_000_000_000_i64;

        // Our own mobile answers a call before desktop ever sees `request`, sending our 19-digit
        // UID in BOTH `uidN` and `uidFrom`. Must NOT synthesize a ghost call from ourselves!
        let self_ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 888_111,
                "status": 0,
                "uidN": "1000000000000000001",
                "uidFrom": "1000000000000000001",
                "params": r#"{"sessId":"abc","extendData":"{}"}"#
            }),
        );
        let o1 = sm.handle_voip_event(&self_ans, now);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o1.events.is_empty());
        assert!(sm.is_dismissed_by_peer(888_111));

        // A late/reordered `request` for that same call must also be ignored!
        let late_req = make_test_invite_event(888_111, "2000000000000000002", "Alice", now);
        let o2 = sm.handle_voip_event(&late_req, now + 200);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(o2.events.is_empty());
    }

    #[test]
    fn locally_declined_and_ended_calls_ignore_duplicate_request_replays() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let now = 1_700_000_000_000_i64;

        let inv = make_test_invite_event(444_555, "2000", "Alice", now);
        let _ = sm.handle_voip_event(&inv, now);
        assert_eq!(sm.phase(), CallPhase::IncomingRinging);

        // Local decline sends 402 once and marks call_id dismissed
        let dec = sm.decline_incoming(Some(444_555));
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(dec
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendAnswerDecline402 { call_id: 444_555, .. })));

        // Duplicate <90s request replay MUST NOT re-ring!
        let dup = sm.handle_voip_event(&inv, now + 1000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(dup.events.is_empty());
    }

    #[test]
    fn caller_answer_preserves_distinct_rtcp_and_passive_ringing_skips_12006_on_mute() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_000_000_i64;

        // 1. Passive incoming ringing: toggling mute must NOT send 12006 to server
        let inv = make_test_invite_event(101, "2000", "Caller", t0);
        let _ = sm.handle_voip_event(&inv, t0);
        let o_mute_passive = sm.set_mic_muted(true);
        assert!(o_mute_passive.effects.contains(&CallEffect::ApplyLocalMute(true)));
        assert!(!o_mute_passive
            .effects
            .iter()
            .any(|e| matches!(e, CallEffect::SendMediaConf12006 { .. })));
        let _ = sm.decline_incoming(Some(101));

        // 2. Caller answer carrying `rtpSerIp` matching a 401 server preserves its distinct RTCP port
        sm.begin_outgoing(200, "2000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .unwrap();
        let resp_401 = json!({
            "id": 202,
            "servers": [
                { "rtpaddr": "42.113.206.4:10490", "rtcpaddr": "42.113.206.4:12490" }
            ]
        });
        assert!(sm.bind_outgoing_401(200, 202, 1000, 2000, "sess".into(), resp_401));
        let _ = sm.mark_outgoing_ringing(
            202,
            1000,
            2000,
            "sess".into(),
            "42.113.206.4:10490".into(),
            "42.113.206.4:12490".into(),
            0x99,
        );
        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 202,
                "status": 0,
                "params": r#"{"rtpSerIp":"42.113.206.4:10490"}"#
            }),
        );
        let o_ans = sm.handle_voip_event(&ans, t0 + 1000);
        assert!(o_ans.effects.iter().any(|e| matches!(
            e,
            CallEffect::StartCallerMedia { rtp_ip, rtcp_ip, .. }
                if rtp_ip == "42.113.206.4:10490" && rtcp_ip == "42.113.206.4:12490"
        )));
    }

    // -----------------------------------------------------------------------
    // Mock SignalingTransport + real localhost UDP ZRTP relay integration tests
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone)]
    enum RecordedSignal {
        RequestCall401(i64, String),
        Request416(i64, String, String, Option<String>),
        RingRing407(i64, String),
        Answer402 {
            call_id: i64,
            status: i64,
            has_extend: bool,
            rtp_address: Option<String>,
            rtcp_address: Option<String>,
            session_id: Option<String>,
            has_codec: bool,
        },
        AnswerAck408(i64, String),
        Cancel405(i64, String),
        EndCall409(i64, String),
        LogEndCall406 { call_id: i64, role: u8 },
        MediaConf12006 { call_id: i64, is_on_mic: bool },
    }

    struct MockSignalingTransport {
        uid: String,
        uid_u32: u32,
        log: Arc<std::sync::Mutex<Vec<RecordedSignal>>>,
        resp_401: std::sync::Mutex<VecDeque<Value>>,
        resp_416: std::sync::Mutex<CallResult<Value>>,
        resp_402: std::sync::Mutex<CallResult<Value>>,
        resp_12006: std::sync::Mutex<CallResult<Value>>,
        delay_416: Duration,
        delay_416_by_call: std::sync::Mutex<std::collections::HashMap<i64, Duration>>,
        delay_407: Duration,
        delay_402: Duration,
        delay_402_by_call: std::sync::Mutex<std::collections::HashMap<i64, Duration>>,
    }

    impl MockSignalingTransport {
        fn new(resp_401: Value) -> Self {
            let mut q = VecDeque::new();
            q.push_back(resp_401);
            Self {
                uid: "1000".into(),
                uid_u32: 1000,
                log: Arc::new(std::sync::Mutex::new(Vec::new())),
                resp_401: std::sync::Mutex::new(q),
                resp_416: std::sync::Mutex::new(Ok(json!({ "status": 0 }))),
                resp_402: std::sync::Mutex::new(Ok(json!({ "status": 0 }))),
                resp_12006: std::sync::Mutex::new(Ok(json!({ "status": 0 }))),
                delay_416: Duration::ZERO,
                delay_416_by_call: std::sync::Mutex::new(std::collections::HashMap::new()),
                delay_407: Duration::ZERO,
                delay_402: Duration::ZERO,
                delay_402_by_call: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }

        fn push_resp_401(&self, resp: Value) {
            self.resp_401.lock().unwrap().push_back(resp);
        }

        fn recorded(&self) -> Vec<RecordedSignal> {
            self.log.lock().unwrap().clone()
        }
    }

    impl crate::signaling::SignalingTransport for MockSignalingTransport {
        fn check_callable(&self) -> CallResult<()> {
            Ok(())
        }

        fn self_uid(&self) -> String {
            self.uid.clone()
        }

        fn self_uid_u32(&self) -> u32 {
            self.uid_u32
        }

        fn request_call_401<'a>(
            &'a self,
            params: &'a RequestCallParams,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            let resp = {
                let mut q = self.resp_401.lock().unwrap();
                if q.len() > 1 {
                    q.pop_front().unwrap()
                } else {
                    q.front().cloned().unwrap_or(Value::Null)
                }
            };
            self.log.lock().unwrap().push(RecordedSignal::RequestCall401(
                params.call_id,
                params.callee_id.clone(),
            ));
            Box::pin(async move { Ok(resp) })
        }

        fn request_416<'a>(
            &'a self,
            params: &'a RequestMediaParams,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log.lock().unwrap().push(RecordedSignal::Request416(
                params.call_id,
                params.rtp_address.clone(),
                params.rtcp_address.clone(),
                params.extend_data.clone(),
            ));
            let delay = self
                .delay_416_by_call
                .lock()
                .unwrap()
                .get(&params.call_id)
                .copied()
                .unwrap_or(self.delay_416);
            let result = match self.resp_416.lock().unwrap().as_ref() {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(CallError::Network(e.to_string())),
            };
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                result
            })
        }

        fn ring_ring_407<'a>(
            &'a self,
            caller_id: &'a str,
            call_id: i64,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log
                .lock()
                .unwrap()
                .push(RecordedSignal::RingRing407(call_id, caller_id.to_string()));
            let delay = self.delay_407;
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Ok(json!({ "status": 0 }))
            })
        }

        fn answer_402<'a>(
            &'a self,
            params: &'a AnswerParams,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log.lock().unwrap().push(RecordedSignal::Answer402 {
                call_id: params.call_id,
                status: params.status,
                has_extend: params.extend_data.is_some(),
                rtp_address: params.rtp_address.clone(),
                rtcp_address: params.rtcp_address.clone(),
                session_id: params.session_id.clone(),
                has_codec: params.codec.is_some(),
            });
            let delay = self
                .delay_402_by_call
                .lock()
                .unwrap()
                .get(&params.call_id)
                .copied()
                .unwrap_or(self.delay_402);
            let result = match self.resp_402.lock().unwrap().as_ref() {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(CallError::Network(e.to_string())),
            };
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                result
            })
        }

        fn answer_ack_408<'a>(
            &'a self,
            callee_id: &'a str,
            call_id: i64,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log
                .lock()
                .unwrap()
                .push(RecordedSignal::AnswerAck408(call_id, callee_id.to_string()));
            Box::pin(async { Ok(json!({ "status": 0 })) })
        }

        fn cancel_405<'a>(
            &'a self,
            to_id: &'a str,
            call_id: i64,
            _call_type: CallType,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log
                .lock()
                .unwrap()
                .push(RecordedSignal::Cancel405(call_id, to_id.to_string()));
            Box::pin(async { Ok(json!({ "status": 0 })) })
        }

        fn end_call_409<'a>(
            &'a self,
            uid_to: &'a str,
            call_id: i64,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log
                .lock()
                .unwrap()
                .push(RecordedSignal::EndCall409(call_id, uid_to.to_string()));
            Box::pin(async { Ok(json!({ "status": 0 })) })
        }

        fn log_end_call_406<'a>(
            &'a self,
            params: &'a LogEndCallParams,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log.lock().unwrap().push(RecordedSignal::LogEndCall406 {
                call_id: params.call_id,
                role: params.role,
            });
            Box::pin(async { Ok(json!({ "status": 0 })) })
        }

        fn set_media_conf_12006<'a>(
            &'a self,
            params: &'a MediaConfParams,
        ) -> crate::signaling::BoxFuture<'a, CallResult<Value>> {
            self.log
                .lock()
                .unwrap()
                .push(RecordedSignal::MediaConf12006 {
                    call_id: params.call_id,
                    is_on_mic: params.is_on_mic,
                });
            let res = match self.resp_12006.lock().unwrap().as_ref() {
                Ok(v) => Ok(v.clone()),
                Err(CallError::Signaling { code, message }) => Err(CallError::Signaling {
                    code: *code,
                    message: message.clone(),
                }),
                Err(e) => Err(CallError::Network(e.to_string())),
            };
            Box::pin(async move { res })
        }
    }

    async fn spawn_mock_zrtp_relay() -> (
        String,
        String,
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ) {
        let rtp = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let rtcp = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let rtp_s = rtp.local_addr().unwrap().to_string();
        let rtcp_s = rtcp.local_addr().unwrap().to_string();
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut b1 = [0u8; 1500];
            let mut b2 = [0u8; 1500];
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = rtp.recv_from(&mut b1) => {
                        if n >= 5 && b1[0] == crate::zrtp::RELAY_WRAP_AUDIO_RTP {
                            // Loop back wrapped audio RTP as 1-byte 0x04 relay-downstream RTP
                            let mut down = Vec::with_capacity(1 + n - 5);
                            down.push(0x04);
                            down.extend_from_slice(&b1[5..n]);
                            let _ = rtp.send_to(&down, peer).await;
                            continue;
                        }
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([b1[18], b1[19]]);
                        if cmd == crate::zrtp::ZRTP_CMD_ECHO {
                            let mut rep = b1[..n].to_vec();
                            rep[0] = crate::zrtp::ZRTP_MAGIC_RESP;
                            let _ = rtp.send_to(&rep, peer).await;
                        } else if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            let cid = u32::from_le_bytes(b1[21..25].try_into().unwrap());
                            let rep = crate::zrtp::encode_zrtp_init_response(
                                cmd,
                                0,
                                cid,
                                0x4242_1111,
                                Some(("203.0.113.5", peer.port())),
                            );
                            let _ = rtp.send_to(&rep, peer).await;
                        }
                    }
                    Ok((n, peer)) = rtcp.recv_from(&mut b2) => {
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([b2[18], b2[19]]);
                        if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            let cid = u32::from_le_bytes(b2[21..25].try_into().unwrap());
                            let rep = crate::zrtp::encode_zrtp_init_response(
                                cmd,
                                0,
                                cid,
                                0x4242_1111,
                                None,
                            );
                            let _ = rtcp.send_to(&rep, peer).await;
                        }
                    }
                }
            }
        });
        (rtp_s, rtcp_s, stop_tx, handle)
    }

    #[tokio::test]
    async fn call_engine_make_call_full_lifecycle_real_udp_relay_and_distinct_rtcp() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 910_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "server_sess_910001",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine = CallEngine::with_transport(
            transport.clone(),
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();

        let call_id = engine
            .make_call("2000", "Alice", CallType::DirectVoice)
            .await
            .unwrap();
        assert_eq!(call_id, 910_001);
        assert_eq!(engine.phase().await, CallPhase::RingingOutgoing);
        assert!(engine.has_active_zrtp().await);

        let snap = engine.snapshot().await.unwrap();
        assert_eq!(snap.relay_token, 0x4242_1111);
        assert_eq!(snap.rtp_ip, rtp_addr);
        assert_eq!(snap.rtcp_ip, rtcp_addr);

        // Callee answers with rtpSerIp matching our 401 server -> must preserve distinct rtcp_ip
        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 910_001,
                "status": 0,
                "params": json!({ "rtpSerIp": rtp_addr }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Connected);
        let snap_conn = engine.snapshot().await.unwrap();
        assert_eq!(snap_conn.rtcp_ip, rtcp_addr);

        engine.set_mic_muted(true).await.unwrap();
        engine.end_call().await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);
        assert!(!engine.has_active_zrtp().await);

        let rec = transport.recorded();
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::RequestCall401(req_id, p) if *req_id > 0 && p == "2000")));
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::Request416(910_001, rtp, rtcp, Some(_)) if rtp == &rtp_addr && rtcp == &rtcp_addr)));
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::AnswerAck408(910_001, p) if p == "2000")));
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::MediaConf12006 { call_id: 910_001, is_on_mic: false })));
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::EndCall409(910_001, p) if p == "2000")));
        assert!(rec.iter().any(|r| matches!(r, RecordedSignal::LogEndCall406 { call_id: 910_001, role: 1 })));

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_make_call_concurrent_cancel_sends_405_with_real_call_id_and_cleans_zrtp() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let mut mock = MockSignalingTransport::new(json!({
            "id": 920_002,
            "status": 0,
            "sessId": "sess_920002",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        }));
        mock.delay_416 = Duration::from_millis(220);
        let transport = Arc::new(mock);

        let engine = Arc::new(
            CallEngine::with_transport(
                transport.clone(),
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        let eng_cancel = engine.clone();
        let cancel_task = tokio::spawn(async move {
            // Wait until 401 has bound real_call_id and 416 is in flight, then cancel
            tokio::time::sleep(Duration::from_millis(360)).await;
            eng_cancel.end_call().await.unwrap();
        });

        let res = engine
            .make_call("2000", "Bob", CallType::DirectVoice)
            .await;
        cancel_task.await.unwrap();

        assert!(res.is_err(), "expected make_call to abort on concurrent cancel");
        assert_eq!(engine.phase().await, CallPhase::Idle);
        assert!(!engine.has_active_zrtp().await);

        let rec = transport.recorded();
        assert!(
            rec.iter()
                .any(|r| matches!(r, RecordedSignal::RequestCall401(req_id, p) if *req_id != 920_002 && p == "2000")),
            "expected RequestCall401 with client-generated req_call_id distinct from 920_002"
        );
        assert!(
            rec.iter()
                .any(|r| matches!(r, RecordedSignal::Cancel405(920_002, p) if p == "2000")),
            "expected Cancel405 with server-assigned real_call_id 920_002, got {rec:?}"
        );

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_make_call_416_failure_rolls_back_state_and_stops_zrtp() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let mock = MockSignalingTransport::new(json!({
            "id": 930_003,
            "status": 0,
            "sessId": "sess_930003",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        }));
        *mock.resp_416.lock().unwrap() = Ok(json!({ "status": 3, "msg": "busy" }));
        let transport = Arc::new(mock);

        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();

        let err = engine
            .make_call("2000", "Charlie", CallType::DirectVoice)
            .await
            .unwrap_err();
        assert!(matches!(err, CallError::Signaling { code: 3, .. }));
        assert_eq!(engine.phase().await, CallPhase::Idle);
        assert!(!engine.has_active_zrtp().await);

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_answer_call_happy_path_and_race_dismissals_never_leak_or_send_402() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;

        // Case A: Happy path incoming accept (407 -> UDP ZRTP probe -> 402 -> Connected -> 409+406)
        let transport_a = Arc::new(MockSignalingTransport::new(json!({})));
        let engine_a = CallEngine::with_transport(
            transport_a.clone(),
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let invite_a = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 940_001,
                "uidN": "2000",
                "uidFrom": "2000",
                "Dname": "Alice",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_in_1",
                    "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                }).to_string()
            }),
        };
        engine_a.handle_voip_event(&invite_a).await.unwrap();
        assert_eq!(engine_a.phase().await, CallPhase::IncomingRinging);

        // Stale call_id mismatch is cleanly rejected without disturbing ringing call
        assert!(engine_a.answer_call_by_id(Some(111_111), true).await.is_err());
        assert_eq!(engine_a.phase().await, CallPhase::IncomingRinging);

        engine_a.answer_call_by_id(Some(940_001), true).await.unwrap();
        assert_eq!(engine_a.phase().await, CallPhase::Connected);
        assert!(engine_a.has_active_zrtp().await);
        assert_eq!(engine_a.snapshot().await.unwrap().relay_token, 0x4242_1111);

        engine_a.end_call().await.unwrap();
        assert_eq!(engine_a.phase().await, CallPhase::Idle);
        assert!(!engine_a.has_active_zrtp().await);
        let rec_a = transport_a.recorded();
        assert!(rec_a.iter().any(|r| matches!(r, RecordedSignal::RingRing407(940_001, p) if p == "2000")));
        assert!(rec_a.iter().any(|r| matches!(r, RecordedSignal::Answer402 { call_id: 940_001, status: 0, has_extend: true, .. })));
        assert!(rec_a.iter().any(|r| matches!(r, RecordedSignal::EndCall409(940_001, _))));
        assert!(rec_a.iter().any(|r| matches!(r, RecordedSignal::LogEndCall406 { call_id: 940_001, role: 0 })));

        // Case B: Mobile answers DURING 407 + ZRTP probe window -> ZERO 402 sent & ZERO ZRTP leak!
        let mut mock_b = MockSignalingTransport::new(json!({}));
        mock_b.delay_407 = Duration::from_millis(120);
        let transport_b = Arc::new(mock_b);
        let engine_b = Arc::new(
            CallEngine::with_transport(
                transport_b.clone(),
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let invite_b = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 940_002,
                "uidN": "2000",
                "uidFrom": "2000",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_in_2",
                    "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                }).to_string()
            }),
        };
        engine_b.handle_voip_event(&invite_b).await.unwrap();
        let eng_b_mob = engine_b.clone();
        let mob_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mob_ans = voip_event_from_parts(
                "answer",
                json!({ "callId": 940_002, "status": 0, "uidFrom": "2000" }),
            );
            eng_b_mob.handle_voip_event(&mob_ans).await.unwrap();
        });
        engine_b.answer_call_by_id(Some(940_002), true).await.unwrap();
        mob_task.await.unwrap();

        assert_eq!(engine_b.phase().await, CallPhase::Idle);
        assert!(!engine_b.has_active_zrtp().await);
        assert!(!engine_b.has_active_media().await);
        assert!(
            !transport_b
                .recorded()
                .iter()
                .any(|r| matches!(r, RecordedSignal::Answer402 { .. })),
            "must NEVER send 402 after mobile answered during 407/probe!"
        );

        // Case C: Caller cancels WHILE 402 is in flight -> ZRTP/media cleaned up (no Idle leak!)
        let mut mock_c = MockSignalingTransport::new(json!({}));
        mock_c.delay_402 = Duration::from_millis(120);
        let transport_c = Arc::new(mock_c);
        let engine_c = Arc::new(
            CallEngine::with_transport(
                transport_c,
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let invite_c = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 940_003,
                "uidN": "2000",
                "uidFrom": "2000",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_in_3",
                    "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                }).to_string()
            }),
        };
        engine_c.handle_voip_event(&invite_c).await.unwrap();
        let eng_c_cancel = engine_c.clone();
        let cancel_task = tokio::spawn(async move {
            // Wait until 407 + 280ms probe finishes and 402 is in flight
            tokio::time::sleep(Duration::from_millis(340)).await;
            let remote_cancel = voip_event_from_parts("cancel", json!({ "callId": 940_003 }));
            eng_c_cancel.handle_voip_event(&remote_cancel).await.unwrap();
        });
        engine_c.answer_call_by_id(Some(940_003), true).await.unwrap();
        cancel_task.await.unwrap();

        assert_eq!(engine_c.phase().await, CallPhase::Idle);
        assert!(!engine_c.has_active_zrtp().await);
        assert!(!engine_c.has_active_media().await);

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_live_audio_media_preserves_pre_connect_mute_over_udp_relay() {
        // Exercises `AudioMediaSession` + `ZrtpRelaySession` + `CallEngine` simultaneously on real
        // OS audio streams when at least an output device is present (receive-only fallback allows
        // speaker-only machines; gracefully no-ops on fully headless runners).
        let has_output = crate::audio::list_output_devices().is_ok_and(|v| !v.is_empty());
        if !has_output {
            return;
        }

        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 950_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_live_audio",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: true,
                devices: AudioDeviceSelection::default(),
                trace: None,
            },
        )
        .unwrap();

        let cid = engine
            .make_call("2000", "Alice", CallType::DirectVoice)
            .await
            .unwrap();
        assert_eq!(cid, 950_001);

        // Mute BEFORE callee answers: audio session must start muted from frame 0!
        engine.set_mic_muted(true).await.unwrap();

        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 950_001,
                "status": 0,
                "params": json!({ "rtpSerIp": rtp_addr }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Connected);
        assert!(engine.has_active_media().await);

        // Let real WASAPI + Opus + UDP relay run for 250ms
        tokio::time::sleep(Duration::from_millis(250)).await;
        let telem = engine.audio_telemetry().await.expect("live audio telemetry");
        assert!(telem.mic_muted, "pre-connect mute was lost when AudioMediaSession started!");
        assert_eq!(telem.mic_level_permille, 0);
        assert!(
            telem.tx_packets >= 5,
            "expected >=5 live RTP packets sent, got {}",
            telem.tx_packets
        );
        assert!(
            telem.rx_packets >= 3,
            "expected >=3 relayed RTP packets received back, got {}",
            telem.rx_packets
        );

        engine.end_call().await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);
        assert!(!engine.has_active_media().await);
        assert!(!engine.has_active_zrtp().await);

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_answer_402_nonzero_status_fails_cleanly_without_connecting() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;

        for (raw_status, expected_code) in [(json!(-101), -101_i64), (json!("3"), 3_i64)] {
            let mock = MockSignalingTransport::new(json!({}));
            *mock.resp_402.lock().unwrap() = Ok(json!({
                "status": raw_status,
                "msg": "call already ended"
            }));
            let transport = Arc::new(mock);
            let engine = CallEngine::with_transport(
                transport,
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut rx = engine.subscribe();

            let cid = 960_000 + expected_code.unsigned_abs() as i64;
            let invite = VoipControlEvent {
                act: "request".into(),
                data: json!({
                    "callId": cid,
                    "uidN": "2000",
                    "uidFrom": "2000",
                    "Dname": "Alice",
                    "ts": now_ms(),
                    "params": json!({
                        "sessId": "sess_402_fail",
                        "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                    }).to_string()
                }),
            };
            engine.handle_voip_event(&invite).await.unwrap();
            assert_eq!(engine.phase().await, CallPhase::IncomingRinging);

            let err = engine.answer_call_by_id(Some(cid), true).await.unwrap_err();
            assert!(
                matches!(err, CallError::Signaling { code, .. } if code == expected_code),
                "expected Signaling({expected_code}), got {err:?}"
            );
            assert_eq!(engine.phase().await, CallPhase::Idle);
            assert!(!engine.has_active_zrtp().await);
            assert!(!engine.has_active_media().await);

            let mut saw_connected = false;
            let mut saw_rejected = false;
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    CallEvent::Connected { .. } => saw_connected = true,
                    CallEvent::CallEnded {
                        call_id,
                        reason: CallEndReason::RemoteRejected { code, .. },
                        ..
                    } if call_id == cid && code == expected_code => saw_rejected = true,
                    _ => {}
                }
            }
            assert!(!saw_connected, "non-zero 402 status must NEVER transition to Connected");
            assert!(saw_rejected, "non-zero 402 status must emit CallEnded(RemoteRejected)");
        }

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_stale_make_call_abort_does_not_kill_newer_call_zrtp() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let mock = MockSignalingTransport::new(json!({
            "id": 970_001,
            "status": 0,
            "sessId": "sess_call_1",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        }));
        mock.push_resp_401(json!({
            "id": 970_002,
            "status": 0,
            "sessId": "sess_call_2",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        }));
        mock.delay_416_by_call
            .lock()
            .unwrap()
            .insert(970_001, Duration::from_millis(550));
        let transport = Arc::new(mock);

        let engine = Arc::new(
            CallEngine::with_transport(
                transport,
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        // Start Call #1 whose 416 will stay in flight for 550ms
        let eng1 = engine.clone();
        let call1_task = tokio::spawn(async move {
            eng1.make_call("2000", "Call1", CallType::DirectVoice).await
        });

        // Wait until Call #1 has stored its ZRTP handle and entered 416
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if engine.active_zrtp_call_id().await == Some(970_001) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert_eq!(engine.active_zrtp_call_id().await, Some(970_001));

        // Cancel Call #1 and immediately start Call #2 while Call #1's 416 is still in flight
        engine.end_call().await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);

        let cid2 = engine
            .make_call("3000", "Call2", CallType::DirectVoice)
            .await
            .unwrap();
        assert_eq!(cid2, 970_002);
        assert_eq!(engine.phase().await, CallPhase::RingingOutgoing);
        assert_eq!(engine.active_zrtp_call_id().await, Some(970_002));

        // Wait for stale Call #1's 416 to return and run its stale-abort path
        let res1 = call1_task.await.unwrap();
        assert!(res1.is_err(), "stale Call #1 should return InvalidState");

        // Call #2 MUST STILL BE ALIVE with its own ZRTP session intact!
        assert_eq!(engine.phase().await, CallPhase::RingingOutgoing);
        assert_eq!(engine.snapshot().await.unwrap().call_id, 970_002);
        assert_eq!(
            engine.active_zrtp_call_id().await,
            Some(970_002),
            "stale Call #1 teardown MUST NOT kill Call #2's active ZRTP!"
        );

        engine.end_call().await.unwrap();
        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_stale_answer_402_abort_does_not_kill_newer_call_zrtp() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let mock = MockSignalingTransport::new(json!({}));
        mock.delay_402_by_call
            .lock()
            .unwrap()
            .insert(980_001, Duration::from_millis(550));
        let transport = Arc::new(mock);

        let engine = Arc::new(
            CallEngine::with_transport(
                transport,
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        let make_invite = |cid: i64| VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": cid,
                "uidN": "2000",
                "uidFrom": "2000",
                "ts": now_ms(),
                "params": json!({
                    "sessId": format!("sess_{cid}"),
                    "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                }).to_string()
            }),
        };

        // Incoming Call #1 -> accept -> enters slow 402
        engine.handle_voip_event(&make_invite(980_001)).await.unwrap();
        let eng1 = engine.clone();
        let ans1_task = tokio::spawn(async move {
            eng1.answer_call_by_id(Some(980_001), true).await
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if engine.active_zrtp_call_id().await == Some(980_001) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        assert_eq!(engine.active_zrtp_call_id().await, Some(980_001));

        // Call #1 is remotely cancelled while 402 is in flight -> then Call #2 arrives and connects!
        let cancel1 = voip_event_from_parts("cancel", json!({ "callId": 980_001 }));
        engine.handle_voip_event(&cancel1).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Idle);

        engine.handle_voip_event(&make_invite(980_002)).await.unwrap();
        engine.answer_call_by_id(Some(980_002), true).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Connected);
        assert_eq!(engine.active_zrtp_call_id().await, Some(980_002));

        // Stale Call #1's 402 completes and observes `!transitioned`
        ans1_task.await.unwrap().unwrap();

        // Call #2 MUST STILL BE CONNECTED with its own ZRTP session intact!
        assert_eq!(engine.phase().await, CallPhase::Connected);
        assert_eq!(engine.snapshot().await.unwrap().call_id, 980_002);
        assert_eq!(
            engine.active_zrtp_call_id().await,
            Some(980_002),
            "stale Call #1 answer abort MUST NOT kill Call #2's active ZRTP!"
        );

        engine.end_call().await.unwrap();
        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_multi_relay_selects_server_b_token_and_surfaces_media_warnings() {
        const TOKEN_A: u32 = 0xAAAA_1111;
        const TOKEN_B: u32 = 0xBBBB_2222;

        let sock_a = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_b = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr_a = sock_a.local_addr().unwrap().to_string();
        let addr_b = sock_b.local_addr().unwrap().to_string();

        let b_pings_tok_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_rtp_tok_b = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_wrong_tok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);

        let sa = sock_a.clone();
        let sb = sock_b.clone();
        let cb_ping = b_pings_tok_b.clone();
        let cb_rtp = b_rtp_tok_b.clone();
        let cb_bad = b_wrong_tok.clone();
        let relay_task = tokio::spawn(async move {
            let mut ba = [0u8; 1500];
            let mut bb = [0u8; 1500];
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = sa.recv_from(&mut ba) => {
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([ba[18], ba[19]]);
                        if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            let cid = u32::from_le_bytes(ba[21..25].try_into().unwrap());
                            let rep = crate::zrtp::encode_zrtp_init_response(cmd, 0, cid, TOKEN_A, None);
                            let _ = sa.send_to(&rep, peer).await;
                        }
                    }
                    Ok((n, peer)) = sb.recv_from(&mut bb) => {
                        if n >= 5 && bb[0] == crate::zrtp::RELAY_WRAP_AUDIO_RTP {
                            let tok = u32::from_le_bytes(bb[1..5].try_into().unwrap());
                            if tok == TOKEN_B {
                                cb_rtp.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            } else {
                                cb_bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            continue;
                        }
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([bb[18], bb[19]]);
                        if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            let cid = u32::from_le_bytes(bb[21..25].try_into().unwrap());
                            let rep = crate::zrtp::encode_zrtp_init_response(cmd, 0, cid, TOKEN_B, None);
                            let _ = sb.send_to(&rep, peer).await;
                        } else if cmd == crate::zrtp::ZRTP_CMD_PING {
                            let tok = u32::from_le_bytes(bb[14..18].try_into().unwrap());
                            if tok == TOKEN_B {
                                cb_ping.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            } else {
                                cb_bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        });

        let has_output = crate::audio::list_output_devices().is_ok_and(|v| !v.is_empty());
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 990_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_multi_relay",
            "servers": [
                { "rtpaddr": addr_a, "rtcpaddr": addr_a },
                { "rtpaddr": addr_b, "rtcpaddr": addr_b }
            ]
        })));
        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: has_output,
                devices: AudioDeviceSelection::default(),
                trace: None,
            },
        )
        .unwrap();

        engine
            .make_call("2000", "Alice", CallType::DirectVoice)
            .await
            .unwrap();

        // Callee answers selecting ServerB (`addr_b`), whose token is TOKEN_B != TOKEN_A
        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 990_001,
                "status": 0,
                "params": json!({ "rtpSerIp": addr_b }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Connected);
        let snap = engine.snapshot().await.unwrap();
        assert_eq!(snap.rtp_ip, addr_b);
        assert_eq!(
            snap.relay_token, TOKEN_B,
            "selected ServerB must use TOKEN_B, never TOKEN_A"
        );

        tokio::time::sleep(Duration::from_millis(220)).await;
        assert!(
            b_pings_tok_b.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "ServerB should receive Ping with TOKEN_B"
        );
        if has_output {
            assert!(
                b_rtp_tok_b.load(std::sync::atomic::Ordering::Relaxed) >= 3,
                "ServerB should receive outbound RTP wrapped with TOKEN_B"
            );
        }
        assert_eq!(
            b_wrong_tok.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "ServerB must NEVER receive packets stamped with ServerA's token!"
        );

        engine.end_call().await.unwrap();

        // Also verify `CallEvent::MediaWarning` is emitted when media cannot start (invalid RTP addr)
        let warn_transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 990_002,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_warn",
            "servers": [{ "rtpaddr": addr_a, "rtcpaddr": addr_a }]
        })));
        let warn_engine = CallEngine::with_transport(
            warn_transport,
            CallEngineConfig {
                enable_audio: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rx = warn_engine.subscribe();
        warn_engine
            .make_call("2000", "Warn", CallType::DirectVoice)
            .await
            .unwrap();
        let bad_ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 990_002,
                "status": 0,
                "params": r#"{"rtpSerIp":"not-a-valid-socket-addr"}"#,
            }),
        );
        warn_engine.handle_voip_event(&bad_ans).await.unwrap();
        let mut saw_warning = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, CallEvent::MediaWarning { call_id: 990_002, .. }) {
                saw_warning = true;
            }
        }
        assert!(saw_warning, "expected CallEvent::MediaWarning on invalid RTP addr");
        warn_engine.end_call().await.unwrap();

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_multi_relay_late_server_b_ack_activates_without_server_a_contamination() {
        const TOKEN_A: u32 = 0xAAAA_1111;
        const TOKEN_A_SPURIOUS: u32 = 0xAAAA_9999;
        const TOKEN_B_LATE: u32 = 0xBBBB_7777;

        let sock_a = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_b = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr_a = sock_a.local_addr().unwrap().to_string();
        let addr_b = sock_b.local_addr().unwrap().to_string();

        let b_inits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_pings_ok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_rtp_ok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b_wrong_tok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let arm_spurious_a = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);

        let sa = sock_a.clone();
        let sb = sock_b.clone();
        let cb_init = b_inits.clone();
        let cb_ping = b_pings_ok.clone();
        let cb_rtp = b_rtp_ok.clone();
        let cb_bad = b_wrong_tok.clone();
        let arm_a = arm_spurious_a.clone();
        let relay_task = tokio::spawn(async move {
            let mut ba = [0u8; 1500];
            let mut bb = [0u8; 1500];
            let mut sent_spurious_a = false;
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = sa.recv_from(&mut ba) => {
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([ba[18], ba[19]]);
                        if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            let cid = u32::from_le_bytes(ba[21..25].try_into().unwrap());
                            let rep = crate::zrtp::encode_zrtp_init_response(cmd, 0, cid, TOKEN_A, None);
                            let _ = sa.send_to(&rep, peer).await;
                        } else if cmd == crate::zrtp::ZRTP_CMD_PING
                            && arm_a.load(std::sync::atomic::Ordering::Relaxed)
                            && !sent_spurious_a
                        {
                            sent_spurious_a = true;
                            let sp = crate::zrtp::encode_zrtp_init_response(
                                crate::zrtp::ZRTP_CMD_INIT_CALLER,
                                0,
                                990_003,
                                TOKEN_A_SPURIOUS,
                                None,
                            );
                            let _ = sa.send_to(&sp, peer).await;
                        }
                    }
                    Ok((n, peer)) = sb.recv_from(&mut bb) => {
                        if n >= 5 && bb[0] == crate::zrtp::RELAY_WRAP_AUDIO_RTP {
                            let tok = u32::from_le_bytes(bb[1..5].try_into().unwrap());
                            if tok == TOKEN_B_LATE {
                                cb_rtp.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            } else {
                                cb_bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            continue;
                        }
                        if n < crate::zrtp::ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([bb[18], bb[19]]);
                        if matches!(cmd, crate::zrtp::ZRTP_CMD_INIT_CALLER | crate::zrtp::ZRTP_CMD_INIT_CALLEE) {
                            // Drop the first InitZRTP from `ensure_target_registered`; reply on
                            // keepalive_task's per-server InitZRTP retry!
                            let k = cb_init.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            if k >= 2 {
                                let cid = u32::from_le_bytes(bb[21..25].try_into().unwrap());
                                let rep = crate::zrtp::encode_zrtp_init_response(
                                    cmd,
                                    0,
                                    cid,
                                    TOKEN_B_LATE,
                                    None,
                                );
                                let _ = sb.send_to(&rep, peer).await;
                            }
                        } else if cmd == crate::zrtp::ZRTP_CMD_PING {
                            let tok = u32::from_le_bytes(bb[14..18].try_into().unwrap());
                            if tok == TOKEN_B_LATE {
                                cb_ping.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            } else {
                                cb_bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        });

        let has_output = crate::audio::list_output_devices().is_ok_and(|v| !v.is_empty());
        // 401 lists only ServerA; callee answers with a brand-new ServerB (`addr_b`).
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 990_003,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_late_server_b",
            "servers": [{ "rtpaddr": addr_a, "rtcpaddr": addr_a }]
        })));
        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: has_output,
                devices: AudioDeviceSelection::default(),
                trace: None,
            },
        )
        .unwrap();

        engine
            .make_call("2000", "LateB", CallType::DirectVoice)
            .await
            .unwrap();
        assert_eq!(engine.snapshot().await.unwrap().relay_token, TOKEN_A);

        arm_spurious_a.store(true, std::sync::atomic::Ordering::Relaxed);
        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 990_003,
                "status": 0,
                "params": json!({ "rtpSerIp": addr_b }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        assert_eq!(engine.phase().await, CallPhase::Connected);
        // Immediately after switching to unacked ServerB, snapshot MUST NOT show TOKEN_A
        // (or TOKEN_A_SPURIOUS); it is either still 0 or already TOKEN_B_LATE if OS audio startup
        // took longer than keepalive_task's 400ms InitZRTP retry.
        let snap_initial = engine.snapshot().await.unwrap();
        assert_eq!(snap_initial.rtp_ip, addr_b);
        assert_ne!(snap_initial.relay_token, TOKEN_A);
        assert_ne!(snap_initial.relay_token, TOKEN_A_SPURIOUS);
        assert!(
            snap_initial.relay_token == 0 || snap_initial.relay_token == TOKEN_B_LATE,
            "unexpected initial ServerB relay token: 0x{:08x}",
            snap_initial.relay_token
        );

        // Wait for keepalive_task's per-server InitZRTP retry (400ms) -> ServerB acks TOKEN_B_LATE
        // -> AudioMediaSession activates 5-byte wrapping with TOKEN_B_LATE & keepalive switches to Ping!
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1600);
        while tokio::time::Instant::now() < deadline {
            let tok = engine.snapshot().await.unwrap().relay_token;
            let pings = b_pings_ok.load(std::sync::atomic::Ordering::Relaxed);
            let rtps = b_rtp_ok.load(std::sync::atomic::Ordering::Relaxed);
            if tok == TOKEN_B_LATE && pings >= 1 && (!has_output || rtps >= 3) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        assert_eq!(
            engine.snapshot().await.unwrap().relay_token,
            TOKEN_B_LATE,
            "ServerB late InitZRTP ack must activate TOKEN_B_LATE"
        );
        assert!(
            b_pings_ok.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "keepalive_task must switch ServerB to Ping(TOKEN_B_LATE)"
        );
        if has_output {
            assert!(
                b_rtp_ok.load(std::sync::atomic::Ordering::Relaxed) >= 3,
                "AudioMediaSession must transmit RTP to ServerB wrapped with TOKEN_B_LATE"
            );
        }
        assert_eq!(
            b_wrong_tok.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "ServerB must NEVER receive packets wrapped with ServerA's token!"
        );

        engine.end_call().await.unwrap();
        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[test]
    fn mark_outgoing_ringing_rejects_mismatched_call_id_and_402_rejection_dismisses_replays() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_000_000_i64;

        // 1. Call #2 is currently in Dialing; a stale mark_outgoing_ringing(Call #1) MUST NOT
        // overwrite Call #2's record or transition it to RingingOutgoing!
        sm.begin_outgoing(2002, "3000".into(), "Call2".into(), CallType::DirectVoice, t0)
            .unwrap();
        let stale_out = sm.mark_outgoing_ringing(
            1001,
            1000,
            2000,
            "stale_sess".into(),
            "1.2.3.4:1000".into(),
            "1.2.3.4:1001".into(),
            0xDEAD,
        );
        assert!(stale_out.events.is_empty());
        assert_eq!(sm.phase(), CallPhase::Dialing);
        let active = sm.active().unwrap();
        assert_eq!(active.call_id, 2002);
        assert_eq!(active.partner_id, "3000");
        assert_eq!(active.relay_token, 0);
        let _ = sm.end_call(t0 + 100);

        // 2. When incoming accept fails with RemoteRejected (non-zero 402 status), call_id is
        // marked dismissed so a duplicate <90s request replay does not re-ring.
        let inv = make_test_invite_event(555_777, "2000", "Caller", t0);
        let _ = sm.handle_voip_event(&inv, t0);
        assert!(sm.begin_accept_incoming(Some(555_777)).unwrap().is_some());
        let fail_out = sm.fail_connecting(
            555_777,
            CallEndReason::RemoteRejected {
                code: -101,
                message: "already ended".into(),
            },
        );
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(sm.is_dismissed_by_peer(555_777));
        assert!(fail_out.effects.contains(&CallEffect::StopMediaAndZrtp));

        let replay = sm.handle_voip_event(&inv, t0 + 1_000);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(replay.events.is_empty());
    }

    #[tokio::test]
    async fn call_engine_receive_only_and_audio_start_error_surface_media_warnings() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;

        // Case A: AudioMediaSession::start returns Err (e.g., output device start failure) ->
        // CallEngine stays Connected, has_active_media == false, emits CallEvent::MediaWarning.
        let transport_err = Arc::new(MockSignalingTransport::new(json!({
            "id": 995_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_audio_err",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine_err = CallEngine::with_transport_and_audio_fault(
            transport_err,
            CallEngineConfig {
                enable_audio: true,
                devices: AudioDeviceSelection::default(),
                trace: None,
            },
            TestAudioFault::OutputStartFailure,
        )
        .unwrap();
        let mut rx_err = engine_err.subscribe();
        engine_err
            .make_call("2000", "AudioErr", CallType::DirectVoice)
            .await
            .unwrap();
        let ans_err = voip_event_from_parts(
            "answer",
            json!({
                "callId": 995_001,
                "status": 0,
                "params": json!({ "rtpSerIp": rtp_addr }).to_string(),
            }),
        );
        engine_err.handle_voip_event(&ans_err).await.unwrap();
        assert_eq!(engine_err.phase().await, CallPhase::Connected);
        assert!(!engine_err.has_active_media().await);
        assert!(engine_err.has_active_zrtp().await);

        let mut saw_start_err_warning = false;
        while let Ok(ev) = rx_err.try_recv() {
            if let CallEvent::MediaWarning { call_id: 995_001, message } = ev {
                if message.contains("Không khởi động được audio media") {
                    saw_start_err_warning = true;
                }
            }
        }
        assert!(
            saw_start_err_warning,
            "AudioMediaSession::start error must emit CallEvent::MediaWarning"
        );
        engine_err.end_call().await.unwrap();

        // Case B: Missing microphone (`TestAudioFault::MissingInput`) on a host with speakers ->
        // CallEngine starts in receive-only mode, emits MediaWarning, reports `has_input_device == false`,
        // and streams 20 ms silence RTP packets over UDP.
        let has_output = crate::audio::list_output_devices().is_ok_and(|v| !v.is_empty());
        if has_output {
            let transport_rx = Arc::new(MockSignalingTransport::new(json!({
                "id": 995_002,
                "status": 0,
                "fromId": 1000,
                "toId": 2000,
                "sessId": "sess_rx_only",
                "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
            })));
            let engine_rx = CallEngine::with_transport_and_audio_fault(
                transport_rx,
                CallEngineConfig {
                    enable_audio: true,
                    devices: AudioDeviceSelection::default(),
                    trace: None,
                },
                TestAudioFault::MissingInput,
            )
            .unwrap();
            let mut rx_warn = engine_rx.subscribe();
            engine_rx
                .make_call("2000", "RxOnly", CallType::DirectVoice)
                .await
                .unwrap();
            let ans_rx = voip_event_from_parts(
                "answer",
                json!({
                    "callId": 995_002,
                    "status": 0,
                    "params": json!({ "rtpSerIp": rtp_addr }).to_string(),
                }),
            );
            engine_rx.handle_voip_event(&ans_rx).await.unwrap();
            assert_eq!(engine_rx.phase().await, CallPhase::Connected);
            assert!(engine_rx.has_active_media().await);

            tokio::time::sleep(Duration::from_millis(220)).await;
            let telem = engine_rx.audio_telemetry().await.expect("receive-only telemetry");
            assert!(!telem.has_input_device);
            assert_eq!(telem.mic_level_permille, 0);
            assert!(
                telem.tx_packets >= 5,
                "receive-only call must transmit 20 ms silence RTP packets, got {}",
                telem.tx_packets
            );
            assert!(
                telem.rx_packets >= 3,
                "receive-only call must receive remote audio RTP packets, got {}",
                telem.rx_packets
            );

            let mut saw_rx_only_warning = false;
            while let Ok(ev) = rx_warn.try_recv() {
                if let CallEvent::MediaWarning { call_id: 995_002, message } = ev {
                    if message.contains("receive-only") {
                        saw_rx_only_warning = true;
                    }
                }
            }
            assert!(
                saw_rx_only_warning,
                "receive-only fallback must emit CallEvent::MediaWarning"
            );
            engine_rx.end_call().await.unwrap();
        }

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn call_engine_early_ring_ring_during_zrtp_probe_does_not_abort_make_call() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 996_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_early_ring",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine = Arc::new(
            CallEngine::with_transport(
                transport.clone(),
                CallEngineConfig {
                    enable_audio: false,
                    ..Default::default()
                },
            )
            .unwrap(),
        );

        let eng_ring = engine.clone();
        let ring_task = tokio::spawn(async move {
            // Wait until 401 binds real_call_id=996_001 (during the 280ms ZRTP probe)
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while tokio::time::Instant::now() < deadline {
                if eng_ring
                    .snapshot()
                    .await
                    .is_some_and(|s| s.call_id == 996_001)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let ev = voip_event_from_parts("ring_ring", json!({ "callId": 996_001 }));
            eng_ring.handle_voip_event(&ev).await.unwrap();
        });

        let cid = engine
            .make_call("2000", "EarlyRing", CallType::DirectVoice)
            .await
            .expect("early ring_ring during ZRTP probe must not abort make_call");
        ring_task.await.unwrap();

        assert_eq!(cid, 996_001);
        assert_eq!(engine.phase().await, CallPhase::RingingOutgoing);
        assert_eq!(engine.active_zrtp_call_id().await, Some(996_001));
        assert!(transport
            .recorded()
            .iter()
            .any(|r| matches!(r, RecordedSignal::Request416(996_001, ..))));

        engine.end_call().await.unwrap();
        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[test]
    fn duplicate_request_never_downgrades_rich_invite_and_upgrades_stub_with_full_metadata() {
        let mut sm = CallStateMachine::new("1000000000000000001", 1000);
        let now = 1_700_000_000_000_i64;

        // Case 1: Rich WS invite arrives first, followed by stripped lp_control stub -> MUST NOT downgrade!
        let rich_ws = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 700_001,
                "uidN": "2000000000000000002",
                "uidFrom": 2000,
                "uidTo": 1000,
                "ts": now,
                "params": json!({
                    "Dname": "Alice Rich",
                    "sessId": "rich_sess_1",
                    "extendData": r#"{"callType":1,"serverAddr":[{"rtp":"42.113.206.4:10490","rtcp":"42.113.206.4:12490"}]}"#,
                    "servers": [{ "rtpaddr": "42.113.206.4:10490", "rtcpaddr": "42.113.206.4:12490" }]
                }).to_string()
            }),
        };
        let stub_poll = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 700_001,
                "uidFrom": 2000,
                "ts": now + 200,
                "params": "{}"
            }),
        };
        let o1 = sm.handle_voip_event(&rich_ws, now);
        assert_eq!(o1.events.len(), 2);
        let o2 = sm.handle_voip_event(&stub_poll, now + 200);
        assert!(o2.events.is_empty(), "duplicate request must not re-emit IncomingCall");

        let active = sm.active().unwrap();
        assert_eq!(active.partner_id, "2000000000000000002");
        assert_eq!(active.partner_name, "Alice Rich");
        assert_eq!(active.call_type, CallType::DirectVideo);
        assert_eq!(active.sess_id, "rich_sess_1");
        assert_eq!(active.rtp_ip, "42.113.206.4:10490");
        assert_eq!(active.rtcp_ip, "42.113.206.4:12490");
        assert!(
            active.invite_payload.get("extendData").is_some(),
            "stub poll request MUST NOT clobber rich WS invite_payload"
        );
        let _ = sm.decline_incoming(Some(700_001));

        // Case 2: Stub arrives first, then rich WS invite arrives -> MUST upgrade all metadata!
        let stub_first = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 700_002,
                "uidFrom": 2000,
                "ts": now,
                "params": "{}"
            }),
        };
        let rich_second = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 700_002,
                "uidN": "2000000000000000002",
                "uidFrom": 2000,
                "uidTo": 1001,
                "ts": now + 150,
                "params": json!({
                    "Dname": "Bob Upgraded",
                    "sessId": "upgraded_sess_2",
                    "extendData": r#"{"callType":1,"serverAddr":[{"rtp":"171.244.128.62:4200","rtcp":"171.244.128.62:4201"}]}"#
                }).to_string()
            }),
        };
        let _ = sm.handle_voip_event(&stub_first, now);
        assert_eq!(sm.active().unwrap().rtp_ip, "");
        let o_up = sm.handle_voip_event(&rich_second, now + 150);
        assert!(
            o_up.events.iter().any(|e| matches!(
                e,
                CallEvent::StateChanged {
                    phase: CallPhase::IncomingRinging,
                    call_id: Some(700_002)
                }
            )),
            "upgrading stub invite call_type/partner_name must emit StateChanged so UI refreshes"
        );
        assert!(
            !o_up
                .events
                .iter()
                .any(|e| matches!(e, CallEvent::IncomingCall { .. })),
            "upgrading stub invite must NOT re-emit IncomingCall"
        );
        let st2 = sm.active().unwrap();
        assert_eq!(st2.partner_id, "2000000000000000002");
        assert_eq!(st2.partner_name, "Bob Upgraded");
        assert_eq!(st2.call_type, CallType::DirectVideo);
        assert_eq!(st2.sess_id, "upgraded_sess_2");
        assert_eq!(st2.rtp_ip, "171.244.128.62:4200");
        assert_eq!(st2.rtcp_ip, "171.244.128.62:4201");
        assert_eq!(st2.from_uid_u32, 1001);
        assert_eq!(st2.partner_uid_u32, 2000);
    }

    #[test]
    fn caller_rejection_and_self_request_dismiss_ghost_rings_and_deduplicate_ring_ring() {
        let mut sm = CallStateMachine::new("1000000000000000001", 435_550_501);
        let t0 = 1_700_000_000_000_i64;

        // 1. Self-originated `request` echo (by 19-digit uidN or 32-bit uidFrom) is ignored!
        let self_req1 = VoipControlEvent {
            act: "request".into(),
            data: json!({ "callId": 810_001, "uidN": "1000000000000000001", "ts": t0 }),
        };
        let self_req2 = VoipControlEvent {
            act: "request".into(),
            data: json!({ "callId": 810_002, "uidFrom": 435_550_501, "ts": t0 }),
        };
        assert!(sm.handle_voip_event(&self_req1, t0).events.is_empty());
        assert!(sm.handle_voip_event(&self_req2, t0).events.is_empty());
        assert_eq!(sm.phase(), CallPhase::Idle);

        // 2. Duplicate `ring_ring` (WS + poll) emits RemoteRinging only ONCE.
        sm.begin_outgoing(810_003, "2000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .unwrap();
        let rr = voip_event_from_parts("ring_ring", json!({ "callId": 810_003 }));
        let o_rr1 = sm.handle_voip_event(&rr, t0 + 100);
        assert!(o_rr1
            .events
            .iter()
            .any(|e| matches!(e, CallEvent::RemoteRinging { call_id: 810_003 })));
        let o_rr2 = sm.handle_voip_event(&rr, t0 + 150);
        assert!(
            o_rr2.events.is_empty(),
            "duplicate ring_ring must not re-emit RemoteRinging or StateChanged"
        );

        // 3. Caller rejection marks call_id dismissed so duplicate WS+poll `answer` (status=3 with
        // media) NEVER synthesizes a ghost incoming call after returning to Idle!
        let rej = voip_event_from_parts(
            "answer",
            json!({
                "callId": 810_003,
                "status": 3,
                "uidFrom": "2000",
                "params": r#"{"sessId":"s","extendData":"{}"}"#
            }),
        );
        let o_rej = sm.handle_voip_event(&rej, t0 + 500);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(sm.is_dismissed_by_peer(810_003));
        assert!(o_rej
            .events
            .iter()
            .any(|e| matches!(e, CallEvent::CallEnded { call_id: 810_003, .. })));

        let dup_rej = sm.handle_voip_event(&rej, t0 + 700);
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(
            dup_rej.events.is_empty(),
            "duplicate poll answer after caller rejection must never ghost-ring"
        );
    }

    #[tokio::test]
    async fn callee_accept_resolves_extend_data_rtcp_decline_sends_media_and_errors_emit_warning() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let mock = MockSignalingTransport::new(json!({}));
        *mock.resp_12006.lock().unwrap() = Err(CallError::Signaling {
            code: 114,
            message: "bad conf".into(),
        });
        let transport = Arc::new(mock);

        let engine = CallEngine::with_transport(
            transport.clone(),
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rx = engine.subscribe();

        // 1. Incoming invite whose distinct RTCP port ONLY lives inside `extendData.serverAddr`
        let invite_ext = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 997_001,
                "uidN": "2000",
                "uidFrom": "2000",
                "Dname": "Alice",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_ext_rtcp",
                    "extendData": json!({
                        "serverAddr": [{ "rtp": rtp_addr, "rtcp": rtcp_addr }]
                    }).to_string()
                }).to_string()
            }),
        };
        engine.handle_voip_event(&invite_ext).await.unwrap();
        engine.answer_call_by_id(Some(997_001), true).await.unwrap();
        let snap = engine.snapshot().await.unwrap();
        assert_eq!(snap.rtp_ip, rtp_addr);
        assert_eq!(
            snap.rtcp_ip, rtcp_addr,
            "callee accept must preserve distinct RTCP port nested in extendData.serverAddr"
        );

        // 2. Failing 12006 surfaces CallEvent::SignalingWarning instead of silent swallowing
        engine.set_mic_muted(true).await.unwrap();
        let mut saw_sig_warn = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(
                ev,
                CallEvent::SignalingWarning {
                    call_id: 997_001,
                    command: 12006,
                    code: Some(114),
                    ..
                }
            ) {
                saw_sig_warn = true;
            }
        }
        assert!(
            saw_sig_warn,
            "failing 12006 effect must emit CallEvent::SignalingWarning"
        );
        engine.end_call().await.unwrap();

        // 3. Local decline sends 402 carrying codec + rtpAddress + rtcpAddress + session_id
        let invite_dec = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 997_002,
                "uidN": "2000",
                "uidFrom": "2000",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_dec_media",
                    "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
                }).to_string()
            }),
        };
        engine.handle_voip_event(&invite_dec).await.unwrap();
        engine.answer_call_by_id(Some(997_002), false).await.unwrap();
        let rec = transport.recorded();
        assert!(
            rec.iter().any(|r| matches!(
                r,
                RecordedSignal::Answer402 {
                    call_id: 997_002,
                    status: 3,
                    has_extend: false,
                    rtp_address: Some(rtp),
                    rtcp_address: Some(rtcp),
                    session_id: Some(sid),
                    has_codec: true,
                } if rtp == &rtp_addr && rtcp == &rtcp_addr && sid == "sess_dec_media"
            )),
            "decline 402 must carry non-empty codec/rtp/rtcp/session_id: {rec:?}"
        );

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[tokio::test]
    async fn mute_audio_set_devices_and_force_stop_work_cleanly() {
        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 998_001,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_998001",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine = CallEngine::with_transport(
            transport.clone(),
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rx = engine.subscribe();

        let call_id = engine
            .make_call("2000", "Alice", CallType::DirectVoice)
            .await
            .unwrap();
        assert_eq!(call_id, 998_001);

        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 998_001,
                "status": 0,
                "params": json!({ "rtpSerIp": rtp_addr }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        assert!(!engine.snapshot().await.unwrap().peer_mic_muted);

        // Peer mutes microphone (`act: "mute_audio"`, `params.type: 1`)
        let peer_mute = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 998_001,
                "params": r#"{"type":1}"#,
            }),
        );
        engine.handle_voip_event(&peer_mute).await.unwrap();
        assert!(engine.snapshot().await.unwrap().peer_mic_muted);

        // Repeated identical state does not spam duplicate event, unmute (`type: 0`) clears it
        engine.handle_voip_event(&peer_mute).await.unwrap();
        let peer_unmute = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 998_001,
                "params": r#"{"type":0}"#,
            }),
        );
        engine.handle_voip_event(&peer_unmute).await.unwrap();
        assert!(!engine.snapshot().await.unwrap().peer_mic_muted);

        let next_dev = AudioDeviceSelection {
            input_device_name: Some("Headset Mic".into()),
            output_device_name: Some("Headset Ear".into()),
        };
        engine.set_devices(next_dev.clone()).await;
        assert_eq!(engine.devices().await, next_dev);

        // force_stop tears down state and ZRTP without sending HTTP 409/406
        let sig_before = transport.recorded().len();
        engine
            .force_stop(Some(CallEndReason::Error("relay turned on".into())))
            .await;
        assert_eq!(engine.phase().await, CallPhase::Idle);
        assert!(!engine.has_active_zrtp().await);
        assert_eq!(
            transport.recorded().len(),
            sig_before,
            "force_stop must not send HTTP signaling requests"
        );

        let mut peer_changes = Vec::new();
        let mut saw_forced_end = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                CallEvent::PeerMediaChanged { call_id: 998_001, mic_muted } => {
                    peer_changes.push(mic_muted);
                }
                CallEvent::CallEnded {
                    call_id: 998_001,
                    reason: CallEndReason::Error(msg),
                    ..
                } if msg == "relay turned on" => {
                    saw_forced_end = true;
                }
                _ => {}
            }
        }
        assert_eq!(peer_changes, vec![true, false]);
        assert!(saw_forced_end);

        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[test]
    fn format_virtual_audio_warning_covers_both_mic_and_speaker_cases() {
        let both_virt = AudioTelemetry {
            has_input_device: true,
            active_input_device: Some("CABLE Output (VB-Audio Virtual Cable)".into()),
            active_output_device: Some("CABLE Input (VB-Audio Virtual Cable)".into()),
            input_is_virtual: true,
            output_is_virtual: true,
            ..Default::default()
        };
        let msg_both = format_virtual_audio_warning(&both_virt).unwrap();
        assert!(msg_both.contains("CABLE Output"));
        assert!(msg_both.contains("CABLE Input"));

        let mic_virt = AudioTelemetry {
            has_input_device: true,
            active_input_device: Some("Stereo Mix (Realtek(R) Audio)".into()),
            active_output_device: Some("Speakers (Realtek)".into()),
            input_is_virtual: true,
            output_is_virtual: false,
            ..Default::default()
        };
        let msg_mic = format_virtual_audio_warning(&mic_virt).unwrap();
        assert!(msg_mic.contains("Stereo Mix"));

        let spk_virt = AudioTelemetry {
            has_input_device: false,
            active_input_device: None,
            active_output_device: Some("CABLE Input (VB-Audio Virtual Cable)".into()),
            input_is_virtual: false,
            output_is_virtual: true,
            ..Default::default()
        };
        let msg_spk = format_virtual_audio_warning(&spk_virt).unwrap();
        assert!(msg_spk.contains("CABLE Input"));

        let clean = AudioTelemetry {
            has_input_device: true,
            active_input_device: Some("USB Mic".into()),
            active_output_device: Some("Headset".into()),
            input_is_virtual: false,
            output_is_virtual: false,
            ..Default::default()
        };
        assert!(format_virtual_audio_warning(&clean).is_none());
    }

    #[test]
    fn direct_video_sm_emits_12006_cam_states_and_handles_peer_onoff_camera() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_100_000_i64;
        sm.begin_outgoing(
            555_001,
            "2000".into(),
            "Bob".into(),
            CallType::DirectVideo,
            t0,
        )
        .unwrap();
        let snap0 = sm.snapshot(t0).unwrap();
        assert!(snap0.cam_enabled);
        assert!(snap0.peer_cam_enabled);

        let ans_ev = voip_event_from_parts(
            "answer",
            json!({
                "callId": 555_001,
                "status": 0,
                "params": r#"{"rtpSerIp":"171.244.128.62:4200"}"#
            }),
        );
        let o_ans = sm.handle_voip_event(&ans_ev, t0 + 1000);
        assert!(o_ans.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendMediaConf12006 {
                callee_id,
                call_id: 555_001,
                is_on_mic: true,
                is_on_cam: true,
            } if callee_id == "2000"
        )));

        let o_mute = sm.set_mic_muted(true);
        assert!(o_mute.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendMediaConf12006 {
                is_on_mic: false,
                is_on_cam: true,
                ..
            }
        )));

        let o_cam_off = sm.set_cam_enabled(false);
        assert!(o_cam_off
            .effects
            .contains(&CallEffect::ApplyLocalCam(false)));
        assert!(o_cam_off.effects.iter().any(|e| matches!(
            e,
            CallEffect::SendMediaConf12006 {
                is_on_mic: false,
                is_on_cam: false,
                ..
            }
        )));
        assert!(o_cam_off.events.iter().any(|ev| matches!(
            ev,
            CallEvent::CameraStateChanged {
                call_id: 555_001,
                cam_enabled: false
            }
        )));

        let peer_cam_off = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 555_001,
                "params": r#"{"type":0}"#,
            }),
        );
        let o_pc_off = sm.handle_voip_event(&peer_cam_off, t0 + 2000);
        assert!(!sm.snapshot(t0 + 2000).unwrap().peer_cam_enabled);
        assert!(o_pc_off.events.iter().any(|ev| matches!(
            ev,
            CallEvent::PeerCameraChanged {
                call_id: 555_001,
                cam_enabled: false
            }
        )));

        let peer_cam_on = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 555_001,
                "params": r#"{"type":1}"#,
            }),
        );
        let o_pc_on = sm.handle_voip_event(&peer_cam_on, t0 + 3000);
        assert!(sm.snapshot(t0 + 3000).unwrap().peer_cam_enabled);
        assert!(o_pc_on.events.iter().any(|ev| matches!(
            ev,
            CallEvent::PeerCameraChanged {
                call_id: 555_001,
                cam_enabled: true
            }
        )));

        // Mobile sends `type: 2, reason: 2` when camera pauses/backgrounds; must disable peer cam.
        let peer_cam_paused = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 555_001,
                "params": r#"{"type":2,"reason":2}"#,
            }),
        );
        let o_pc_paused = sm.handle_voip_event(&peer_cam_paused, t0 + 4000);
        assert!(!sm.snapshot(t0 + 4000).unwrap().peer_cam_enabled);
        assert!(o_pc_paused.events.iter().any(|ev| matches!(
            ev,
            CallEvent::PeerCameraChanged {
                call_id: 555_001,
                cam_enabled: false
            }
        )));

        // Resuming with `type: 1` restores `peer_cam_enabled = true`.
        let o_pc_resumed = sm.handle_voip_event(&peer_cam_on, t0 + 5000);
        assert!(sm.snapshot(t0 + 5000).unwrap().peer_cam_enabled);
        assert!(o_pc_resumed.events.iter().any(|ev| matches!(
            ev,
            CallEvent::PeerCameraChanged {
                call_id: 555_001,
                cam_enabled: true
            }
        )));
    }

    #[tokio::test]
    async fn aux_media_hook_attaches_receives_video_rtp_rtcp_and_stops_on_hangup() {
        use crate::media_hook::{AuxiliaryMediaHook, AuxiliaryMediaSession, MediaTransportContext};
        use crate::rtp::{build_rtp_packet, OPUS_AUDIO_PT};
        use crate::zrtp::{wrap_relay_audio_rtp, RELAY_INCOMING_VIDEO_RTP};
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        struct StubAuxSession {
            rtp_rx: Arc<AtomicUsize>,
            rtcp_rx: Arc<AtomicUsize>,
            cam_flag: Arc<AtomicBool>,
            stopped: Arc<AtomicBool>,
        }

        impl AuxiliaryMediaSession for StubAuxSession {
            fn on_incoming_rtp(&self, outer_byte: u8, inner_rtp: &[u8]) {
                assert_eq!(outer_byte, RELAY_INCOMING_VIDEO_RTP);
                assert!(inner_rtp.len() >= 12);
                self.rtp_rx.fetch_add(1, Ordering::Relaxed);
            }

            fn on_incoming_rtcp(&self, _outer_byte: u8, inner_rtcp: &[u8]) {
                assert!(inner_rtcp.len() >= 8);
                self.rtcp_rx.fetch_add(1, Ordering::Relaxed);
            }

            fn set_cam_enabled(&self, enabled: bool) {
                self.cam_flag.store(enabled, Ordering::Relaxed);
            }

            fn stop(&self) {
                self.stopped.store(true, Ordering::Relaxed);
            }
        }

        struct StubAuxHook {
            rtp_rx: Arc<AtomicUsize>,
            rtcp_rx: Arc<AtomicUsize>,
            cam_flag: Arc<AtomicBool>,
            stopped: Arc<AtomicBool>,
            seen_call_type: Arc<std::sync::Mutex<Option<CallType>>>,
        }

        impl AuxiliaryMediaHook for StubAuxHook {
            fn on_media_start(
                &self,
                ctx: MediaTransportContext,
            ) -> Option<Arc<dyn AuxiliaryMediaSession>> {
                *self.seen_call_type.lock().unwrap() = Some(ctx.call_type);
                self.cam_flag
                    .store(ctx.initial_cam_enabled, Ordering::Relaxed);
                let vid_rtp = build_rtp_packet(98, true, 1, 9000, 0x2222_3333, &[0, 0, 0, 1, 0x65]);
                let mut down_vid = vec![RELAY_INCOMING_VIDEO_RTP];
                down_vid.extend_from_slice(&vid_rtp);
                let _ = ctx.udp_rtp_sock.send_to(&down_vid, ctx.rtp_target);

                let opus_rtp = build_rtp_packet(OPUS_AUDIO_PT, false, 1, 320, 0x2222_3333, &[0xFC]);
                let down_aud = wrap_relay_audio_rtp(ctx.active_relay_token(), &opus_rtp);
                let _ = ctx.udp_rtp_sock.send_to(&down_aud, ctx.rtp_target);

                Some(Arc::new(StubAuxSession {
                    rtp_rx: self.rtp_rx.clone(),
                    rtcp_rx: self.rtcp_rx.clone(),
                    cam_flag: self.cam_flag.clone(),
                    stopped: self.stopped.clone(),
                }))
            }
        }

        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        // Wrap relay echo in this test by spinning a small reflector so down_vid + RR echo back
        let reflector = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let ref_addr = reflector.local_addr().unwrap();
        let ref_stop = stop_tx.clone();
        let mut ref_stop_rx = ref_stop.subscribe();
        let ref_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                tokio::select! {
                    _ = ref_stop_rx.changed() => break,
                    Ok((n, peer)) = reflector.recv_from(&mut buf) => {
                        if n >= 1 && buf[0] == RELAY_INCOMING_VIDEO_RTP {
                            let _ = reflector.send_to(&buf[..n], peer).await;
                            // Also echo an RTCP RR packet back
                            let rr = [0x05u8, 1, 2, 3, 4, 0x80, 201, 0x00, 0x01, 0x22, 0x22, 0x33, 0x33];
                            let _ = reflector.send_to(&rr, peer).await;
                        }
                    }
                }
            }
        });

        let rtp_rx = Arc::new(AtomicUsize::new(0));
        let rtcp_rx = Arc::new(AtomicUsize::new(0));
        let cam_flag = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let seen_call_type = Arc::new(std::sync::Mutex::new(None));
        let hook = Arc::new(StubAuxHook {
            rtp_rx: rtp_rx.clone(),
            rtcp_rx: rtcp_rx.clone(),
            cam_flag: cam_flag.clone(),
            stopped: stopped.clone(),
            seen_call_type: seen_call_type.clone(),
        });

        let transport = Arc::new(MockSignalingTransport::new(json!({
            "id": 999_123,
            "status": 0,
            "fromId": 1000,
            "toId": 2000,
            "sessId": "sess_999123",
            "servers": [{ "rtpaddr": rtp_addr, "rtcpaddr": rtcp_addr }]
        })));
        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap()
        .with_aux_media_hook(hook);

        let cid = engine
            .make_call("2000", "VideoBob", CallType::DirectVideo)
            .await
            .unwrap();
        assert_eq!(cid, 999_123);

        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 999_123,
                "status": 0,
                "params": json!({ "rtpSerIp": ref_addr.to_string() }).to_string(),
            }),
        );
        engine.handle_voip_event(&ans).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while tokio::time::Instant::now() < deadline
            && (rtp_rx.load(Ordering::Relaxed) == 0 || rtcp_rx.load(Ordering::Relaxed) == 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(engine.has_active_aux_media().await);
        assert_eq!(*seen_call_type.lock().unwrap(), Some(CallType::DirectVideo));
        assert!(cam_flag.load(Ordering::Relaxed));
        assert!(rtp_rx.load(Ordering::Relaxed) >= 1);
        assert!(rtcp_rx.load(Ordering::Relaxed) >= 1);

        engine.set_cam_enabled(false).await.unwrap();
        assert!(!cam_flag.load(Ordering::Relaxed));

        engine.end_call().await.unwrap();
        assert!(stopped.load(Ordering::Relaxed));
        assert!(!engine.has_active_aux_media().await);

        let _ = stop_tx.send(true);
        let _ = ref_task.await;
        let _ = relay_task.await;
    }

    #[test]
    fn peer_cam_and_mute_ignore_self_echo_prefer_params_and_request_keyframe_on_resume() {
        let mut sm = CallStateMachine::new("1000000000000000001", 435_550_501);
        let t0 = 1_700_000_200_000_i64;
        sm.begin_outgoing(
            880_001,
            "2000".into(),
            "Bob".into(),
            CallType::DirectVideo,
            t0,
        )
        .unwrap();
        let ans = voip_event_from_parts(
            "answer",
            json!({
                "callId": 880_001,
                "status": 0,
                "params": r#"{"rtpSerIp":"171.244.128.62:4200"}"#
            }),
        );
        let _ = sm.handle_voip_event(&ans, t0 + 500);
        assert!(sm.snapshot(t0 + 500).unwrap().peer_cam_enabled);
        assert!(!sm.snapshot(t0 + 500).unwrap().peer_mic_muted);

        // 1. Self-originated echoes (by 19-digit uidN or 32-bit uidFrom) must be ignored
        let self_cam_off_n = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 880_001,
                "uidN": "1000000000000000001",
                "params": r#"{"type":0}"#
            }),
        );
        let self_cam_off_u32 = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 880_001,
                "uidFrom": 435_550_501,
                "params": r#"{"type":0}"#
            }),
        );
        let self_mute_n = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 880_001,
                "uidN": "1000000000000000001",
                "params": r#"{"type":1}"#
            }),
        );
        let self_mute_u32 = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 880_001,
                "uidFrom": "435550501",
                "params": r#"{"type":1}"#
            }),
        );
        assert!(sm.handle_voip_event(&self_cam_off_n, t0 + 600).events.is_empty());
        assert!(sm.handle_voip_event(&self_cam_off_u32, t0 + 700).events.is_empty());
        assert!(sm.handle_voip_event(&self_mute_n, t0 + 800).events.is_empty());
        assert!(sm.handle_voip_event(&self_mute_u32, t0 + 900).events.is_empty());
        assert!(sm.snapshot(t0 + 900).unwrap().peer_cam_enabled);
        assert!(!sm.snapshot(t0 + 900).unwrap().peer_mic_muted);

        // 2. Peer turns camera OFF -> PeerCameraChanged(false), NO RequestRemoteKeyframe
        let peer_cam_off = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 880_001,
                "uidFrom": "2000",
                "params": r#"{"type":0}"#
            }),
        );
        let o_off = sm.handle_voip_event(&peer_cam_off, t0 + 1000);
        assert!(!sm.snapshot(t0 + 1000).unwrap().peer_cam_enabled);
        assert!(o_off.effects.is_empty());
        assert!(o_off.events.iter().any(|e| matches!(
            e,
            CallEvent::PeerCameraChanged {
                call_id: 880_001,
                cam_enabled: false
            }
        )));

        // 3. Outer `ev.data.type: 3` must NOT shadow `params.isOnCam: 1`, AND false->true
        //    transition must emit `CallEffect::RequestRemoteKeyframe`!
        let peer_cam_on_shadow = voip_event_from_parts(
            "onoff_camera",
            json!({
                "callId": 880_001,
                "uidFrom": "2000",
                "type": 3,
                "params": r#"{"isOnCam":1}"#
            }),
        );
        let o_on = sm.handle_voip_event(&peer_cam_on_shadow, t0 + 1100);
        assert!(sm.snapshot(t0 + 1100).unwrap().peer_cam_enabled);
        assert!(
            o_on.effects
                .contains(&CallEffect::RequestRemoteKeyframe { call_id: 880_001 }),
            "peer camera false->true resume must emit RequestRemoteKeyframe"
        );
        assert!(o_on.events.iter().any(|e| matches!(
            e,
            CallEvent::PeerCameraChanged {
                call_id: 880_001,
                cam_enabled: true
            }
        )));

        // 4. Outer `ev.data.type: 3` must NOT shadow `params.isOnMic: 1` (unmuted) or `params.mute: false`
        let peer_mute_on = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 880_001,
                "uidFrom": "2000",
                "params": r#"{"type":1}"#
            }),
        );
        let _ = sm.handle_voip_event(&peer_mute_on, t0 + 1200);
        assert!(sm.snapshot(t0 + 1200).unwrap().peer_mic_muted);

        let peer_unmute_shadow = voip_event_from_parts(
            "mute_audio",
            json!({
                "callId": 880_001,
                "uidFrom": "2000",
                "type": 3,
                "params": r#"{"isOnMic":1}"#
            }),
        );
        let o_unmute = sm.handle_voip_event(&peer_unmute_shadow, t0 + 1300);
        assert!(
            !sm.snapshot(t0 + 1300).unwrap().peer_mic_muted,
            "outer data.type=3 must not shadow params.isOnMic=1"
        );
        assert!(o_unmute.events.iter().any(|e| matches!(
            e,
            CallEvent::PeerMediaChanged {
                call_id: 880_001,
                mic_muted: false
            }
        )));
    }

    #[tokio::test]
    async fn callee_answer_starts_media_before_connected_resyncs_privacy_race_and_requests_keyframe() {
        use crate::media_hook::{AuxiliaryMediaHook, AuxiliaryMediaSession, MediaTransportContext};
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        struct RaceProbeAuxSession {
            cam_flag: Arc<AtomicBool>,
            keyframe_reqs: Arc<AtomicUsize>,
            stopped: Arc<AtomicBool>,
        }

        impl AuxiliaryMediaSession for RaceProbeAuxSession {
            fn on_incoming_rtp(&self, _outer_byte: u8, _inner_rtp: &[u8]) {}
            fn on_incoming_rtcp(&self, _outer_byte: u8, _inner_rtcp: &[u8]) {}
            fn set_cam_enabled(&self, enabled: bool) {
                self.cam_flag.store(enabled, Ordering::Relaxed);
            }
            fn request_remote_keyframe(&self) {
                self.keyframe_reqs.fetch_add(1, Ordering::Relaxed);
            }
            fn stop(&self) {
                self.stopped.store(true, Ordering::Relaxed);
            }
        }

        struct RaceProbeAuxHook {
            cam_flag: Arc<AtomicBool>,
            keyframe_reqs: Arc<AtomicUsize>,
            stopped: Arc<AtomicBool>,
            connected_seen_before_media_start: Arc<AtomicBool>,
            rx_for_check: Arc<std::sync::Mutex<broadcast::Receiver<CallEvent>>>,
            engine_slot: Arc<std::sync::Mutex<Option<Arc<CallEngine>>>>,
        }

        impl AuxiliaryMediaHook for RaceProbeAuxHook {
            fn on_media_start(
                &self,
                ctx: MediaTransportContext,
            ) -> Option<Arc<dyn AuxiliaryMediaSession>> {
                // Verify CallEvent::Connected has NOT been emitted yet when callee media starts!
                if let Ok(mut rx) = self.rx_for_check.lock() {
                    while let Ok(ev) = rx.try_recv() {
                        if matches!(ev, CallEvent::Connected { .. }) {
                            self.connected_seen_before_media_start
                                .store(true, Ordering::Relaxed);
                        }
                    }
                }
                self.cam_flag
                    .store(ctx.initial_cam_enabled, Ordering::Relaxed);

                // Toggle mic mute + cam off concurrently on a dedicated thread BEFORE self.media /
                // self.aux_media are inserted, reproducing the Connecting->Connected privacy race.
                if let Some(eng) = self.engine_slot.lock().unwrap().clone() {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(async move {
                            eng.set_mic_muted(true).await.unwrap();
                            eng.set_cam_enabled(false).await.unwrap();
                        });
                    })
                    .join()
                    .unwrap();
                }

                Some(Arc::new(RaceProbeAuxSession {
                    cam_flag: self.cam_flag.clone(),
                    keyframe_reqs: self.keyframe_reqs.clone(),
                    stopped: self.stopped.clone(),
                }))
            }
        }

        let (rtp_addr, rtcp_addr, stop_tx, relay_task) = spawn_mock_zrtp_relay().await;
        let transport = Arc::new(MockSignalingTransport::new(json!({})));
        let engine = CallEngine::with_transport(
            transport,
            CallEngineConfig {
                enable_audio: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rx_after = engine.subscribe();
        let rx_hook = engine.subscribe();

        let cam_flag = Arc::new(AtomicBool::new(true));
        let keyframe_reqs = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let connected_before_media = Arc::new(AtomicBool::new(false));
        let engine_slot: Arc<std::sync::Mutex<Option<Arc<CallEngine>>>> =
            Arc::new(std::sync::Mutex::new(None));

        let hook = Arc::new(RaceProbeAuxHook {
            cam_flag: cam_flag.clone(),
            keyframe_reqs: keyframe_reqs.clone(),
            stopped: stopped.clone(),
            connected_seen_before_media_start: connected_before_media.clone(),
            rx_for_check: Arc::new(std::sync::Mutex::new(rx_hook)),
            engine_slot: engine_slot.clone(),
        });

        let engine = Arc::new(engine.with_aux_media_hook(hook));
        *engine_slot.lock().unwrap() = Some(engine.clone());

        let invite = VoipControlEvent {
            act: "request".into(),
            data: json!({
                "callId": 995_100,
                "uidN": "2000",
                "uidFrom": "2000",
                "Dname": "VideoAlice",
                "ts": now_ms(),
                "params": json!({
                    "sessId": "sess_race_995100",
                    "extendData": json!({
                        "callType": 1,
                        "serverAddr": [{ "rtp": rtp_addr, "rtcp": rtcp_addr }]
                    }).to_string()
                }).to_string()
            }),
        };
        engine.handle_voip_event(&invite).await.unwrap();
        engine.answer_call_by_id(Some(995_100), true).await.unwrap();
        let _ = engine_slot.lock().unwrap().take();

        assert!(
            !connected_before_media.load(Ordering::Relaxed),
            "callee answer_call_by_id must start media BEFORE emitting CallEvent::Connected"
        );
        let mut saw_connected = false;
        while let Ok(ev) = rx_after.try_recv() {
            if matches!(ev, CallEvent::Connected { call_id: 995_100, .. }) {
                saw_connected = true;
            }
        }
        assert!(saw_connected, "CallEvent::Connected must be emitted after media start");

        assert!(
            !cam_flag.load(Ordering::Relaxed),
            "camera-off toggle made during media init must be re-applied under sm_g"
        );
        assert!(
            engine.audio_telemetry().await.unwrap().mic_muted,
            "mic-mute toggle made during media init must be re-applied under sm_g"
        );

        // Peer turns camera off then back on -> must invoke aux.request_remote_keyframe()
        let peer_off = voip_event_from_parts(
            "onoff_camera",
            json!({ "callId": 995_100, "uidFrom": "2000", "params": r#"{"type":0}"# }),
        );
        let peer_on = voip_event_from_parts(
            "onoff_camera",
            json!({ "callId": 995_100, "uidFrom": "2000", "params": r#"{"type":1}"# }),
        );
        engine.handle_voip_event(&peer_off).await.unwrap();
        assert_eq!(keyframe_reqs.load(Ordering::Relaxed), 0);
        engine.handle_voip_event(&peer_on).await.unwrap();
        assert_eq!(
            keyframe_reqs.load(Ordering::Relaxed),
            1,
            "peer camera false->true must invoke aux.request_remote_keyframe()"
        );

        engine.end_call().await.unwrap();
        assert!(stopped.load(Ordering::Relaxed));
        let _ = stop_tx.send(true);
        let _ = relay_task.await;
    }

    #[test]
    fn fail_outgoing_only_tears_down_dialing_and_ignores_ringing_or_connected() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_500_000_i64;

        // 1. In Dialing, fail_outgoing cleanly aborts Dialing -> Idle
        sm.begin_outgoing(700_001, "2000".into(), "Alice".into(), CallType::DirectVoice, t0)
            .unwrap();
        let out_fail = sm.fail_outgoing(700_001, CallEndReason::Error("416 timeout".into()));
        assert_eq!(sm.phase(), CallPhase::Idle);
        assert!(out_fail.effects.contains(&CallEffect::StopMediaAndZrtp));
        assert!(out_fail.events.iter().any(|e| matches!(
            e,
            CallEvent::CallEnded {
                call_id: 700_001,
                ..
            }
        )));

        // 2. Once early WS `ring_ring` moves the call to RingingOutgoing, fail_outgoing is a no-op
        sm.begin_outgoing(700_002, "2000".into(), "Alice".into(), CallType::DirectVoice, t0)
            .unwrap();
        let ring_ev = voip_event_from_parts("ring_ring", json!({ "callId": 700_002 }));
        let _ = sm.handle_voip_event(&ring_ev, t0 + 100);
        assert_eq!(sm.phase(), CallPhase::RingingOutgoing);

        let out_ring_noop =
            sm.fail_outgoing(700_002, CallEndReason::Error("late 416 timeout".into()));
        assert!(out_ring_noop.events.is_empty());
        assert!(out_ring_noop.effects.is_empty());
        assert_eq!(sm.phase(), CallPhase::RingingOutgoing);

        // 3. Once early WS `answer` moves the call to Connected, late fail_outgoing MUST NOT kill it
        let ans_ev = voip_event_from_parts(
            "answer",
            json!({
                "callId": 700_002,
                "status": 0,
                "params": r#"{"rtpSerIp":"127.0.0.1:4200"}"#
            }),
        );
        let _ = sm.handle_voip_event(&ans_ev, t0 + 200);
        assert_eq!(sm.phase(), CallPhase::Connected);

        let out_conn_noop =
            sm.fail_outgoing(700_002, CallEndReason::Error("late 416 timeout".into()));
        assert!(out_conn_noop.events.is_empty());
        assert!(out_conn_noop.effects.is_empty());
        assert_eq!(sm.phase(), CallPhase::Connected);
        assert_eq!(sm.active().map(|s| s.call_id), Some(700_002));
    }

    #[test]
    fn set_cam_enabled_is_clean_noop_on_voice_calls() {
        let mut sm = CallStateMachine::new("1000", 1000);
        let t0 = 1_700_000_600_000_i64;

        sm.begin_outgoing(710_001, "2000".into(), "Bob".into(), CallType::DirectVoice, t0)
            .unwrap();
        let ans_ev = voip_event_from_parts(
            "answer",
            json!({ "callId": 710_001, "status": 0, "params": "{}" }),
        );
        let _ = sm.handle_voip_event(&ans_ev, t0 + 500);
        assert_eq!(sm.phase(), CallPhase::Connected);

        let out_enable = sm.set_cam_enabled(true);
        assert!(out_enable.events.is_empty());
        assert!(out_enable.effects.is_empty());
        assert!(!sm.snapshot(t0 + 600).unwrap().cam_enabled);

        let out_disable = sm.set_cam_enabled(false);
        assert!(out_disable.events.is_empty());
        assert!(out_disable.effects.is_empty());
        assert!(!sm.snapshot(t0 + 700).unwrap().cam_enabled);
    }

    #[test]
    fn incoming_request_parses_numeric_string_call_type_and_video_enable() {
        let t0 = 1_700_000_700_000_i64;

        // 1. data.callType as numeric string "3" -> DirectVideo
        let mut sm1 = CallStateMachine::new("1000", 1000);
        let ev1 = voip_event_from_parts(
            "request",
            json!({
                "callId": 720_001,
                "uidN": "2000",
                "callType": "3",
                "ts": t0,
                "params": "{}"
            }),
        );
        let out1 = sm1.handle_voip_event(&ev1, t0);
        assert_eq!(sm1.snapshot(t0).unwrap().call_type, CallType::DirectVideo);
        assert!(out1.events.iter().any(|e| matches!(
            e,
            CallEvent::IncomingCall {
                call_id: 720_001,
                call_type: CallType::DirectVideo,
                ..
            }
        )));

        // 2. extendData.callType as numeric string "1" -> DirectVideo
        let mut sm2 = CallStateMachine::new("1000", 1000);
        let ev2 = voip_event_from_parts(
            "request",
            json!({
                "callId": 720_002,
                "uidN": "2000",
                "ts": t0,
                "params": json!({
                    "extendData": r#"{"callType":"1"}"#
                }).to_string()
            }),
        );
        let _ = sm2.handle_voip_event(&ev2, t0);
        assert_eq!(sm2.snapshot(t0).unwrap().call_type, CallType::DirectVideo);

        // 3. zrtc_config.video.enable as numeric string "1" -> DirectVideo
        let mut sm3 = CallStateMachine::new("1000", 1000);
        let ev3 = voip_event_from_parts(
            "request",
            json!({
                "callId": 720_003,
                "uidN": "2000",
                "ts": t0,
                "params": json!({
                    "zrtc_config": {
                        "callType": "1",
                        "video": { "enable": "1" }
                    }
                }).to_string()
            }),
        );
        let _ = sm3.handle_voip_event(&ev3, t0);
        assert_eq!(sm3.snapshot(t0).unwrap().call_type, CallType::DirectVideo);

        // 4. zrtc_config.callType as numeric string "3" -> DirectVideo
        let mut sm4 = CallStateMachine::new("1000", 1000);
        let ev4 = voip_event_from_parts(
            "request",
            json!({
                "callId": 720_004,
                "uidN": "2000",
                "ts": t0,
                "params": json!({
                    "zrtc_config": { "callType": "3" }
                }).to_string()
            }),
        );
        let _ = sm4.handle_voip_event(&ev4, t0);
        assert_eq!(sm4.snapshot(t0).unwrap().call_type, CallType::DirectVideo);

        // 5. Numeric strings indicating voice ("1" / "0" / "0") stay DirectVoice
        let mut sm5 = CallStateMachine::new("1000", 1000);
        let ev5 = voip_event_from_parts(
            "request",
            json!({
                "callId": 720_005,
                "uidN": "2000",
                "callType": "2",
                "ts": t0,
                "params": json!({
                    "extendData": r#"{"callType":"0"}"#,
                    "zrtc_config": {
                        "callType": "1",
                        "video": { "enable": "0" }
                    }
                }).to_string()
            }),
        );
        let _ = sm5.handle_voip_event(&ev5, t0);
        assert_eq!(sm5.snapshot(t0).unwrap().call_type, CallType::DirectVoice);
    }
}
