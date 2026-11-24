//! `teza-call` — Isolated pure-Rust VoIP signaling, UDP ZRTP relay, Opus/RTP audio pipeline,
//! and 1-1 call state machine engine for Teza.
//!
//! ## Core Guarantees
//! - **Zero external binary dependency**: No `ZaloCall.exe` or native C/CMake Opus toolchain required
//!   (uses pure-Rust RFC 6716 Opus via `opus-pure` and `cpal` for OS audio).
//! - **Strict Relay mode exclusion**: All calling operations reject `session.relay.enabled == true`
//!   with [`CallError::RelayUnsupported`].
//! - **Multi-device passive ringing**: Incoming invites never send `407` or probe UDP until locally
//!   accepted, and calls handled on another device are recorded in `dismissed_by_peer` so desktop
//!   never sends a conflicting `402` answer/decline.

pub mod audio;
pub mod engine;
pub mod error;
pub mod rtp;
pub mod signaling;
pub mod trace;
pub mod zrtp;

pub use audio::{
    is_virtual_or_loopback_audio_device, list_input_devices, list_output_devices, AudioControls,
    AudioDeviceInfo, AudioDeviceSelection, AudioLoopbackSession, AudioMediaConfig,
    AudioMediaSession, AudioTelemetry, LinearResampler, OpusVoiceCodec,
};
pub use engine::{
    CallEffect, CallEndReason, CallEngine, CallEngineConfig, CallEvent, CallPhase, CallSnapshot,
    CallStateMachine, TransitionOutput,
};
pub use error::{ensure_callable_session, ensure_direct_network, CallError, CallResult};
pub use rtp::{
    build_rtcp_rr, build_rtp_packet, is_rtcp_packet, parse_rtp_packet, JitterBuffer, JitterPop,
    JitterStats, RtpPacket, RtpRxStats, AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE, OPUS_AUDIO_PT,
};
pub use signaling::{
    build_callee_extend_data, build_caller_invite_fields, generate_call_session,
    normalize_call_imei, normalize_voicecall_data, official_call_imei, poll_voip_controls,
    poll_voip_controls_with_trace, resolve_voice_call_host, session_uid_u32, AnswerParams,
    BoxFuture, CallType, CtrlPollState, InviteFields, LogEndCallParams, MediaConfParams,
    QueueCursor, RequestCallParams, RequestMediaParams, SessionSignalingTransport, SignalingClient,
    SignalingCommand, SignalingTransport, DEFAULT_LP_CONTROL_QUEUES, OPUS_16K_CODEC_JSON,
};
pub use trace::{format_trace_json, sanitize_trace_json, TraceSink};
pub use zrtp::{
    build_zrtp_echo_packet, build_zrtp_end_packet, build_zrtp_init_packet, build_zrtp_ping_packet,
    collect_relay_servers, parse_zrtp_echo_response, parse_zrtp_init_response, probe_udp_echo,
    uid_str_to_u32, unwrap_relay_media, wrap_relay_audio_rtcp, wrap_relay_audio_rtp,
    wrap_relay_media, EchoProbeSummary, RelayServer, ZrtpInitResponse, ZrtpProbeConfig,
    ZrtpProbeResult, ZrtpRelaySession,
};
