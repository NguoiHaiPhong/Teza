//! Zalo VoIP HTTP signaling client (`voicecall-wpa.chat.zalo.me`) and `lp_control` polling.
//!
//! Covers 1-1 signaling endpoints (`401`, `416`, `407`, `402`, `408`, `405`, `409`, `406`, `12006`),
//! reusing `zalo_core::session::Session` AES encryption and refusing all operations whenever
//! `session.relay.enabled == true`.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD}, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use zalo_core::chat::parser::{extract_voip_controls, VoipControlEvent};
use zalo_core::endpoints;
use zalo_core::session::{is_allowed_zalo_http_url, urlencode, Session, SessionError};

use crate::error::{ensure_callable_session, CallError, CallResult};
use crate::trace::{emit_trace, format_trace_json, TraceSink};
use crate::zrtp::{detect_local_p2p_endpoint, ZrtpProbeResult};

/// Default Opus 16 kHz mono 20 ms codec descriptor negotiated with Zalo 1-1 VoIP peers.
pub const OPUS_16K_CODEC_JSON: &str =
    "[{\"dynamicFptime\":0,\"frmPtime\":20,\"name\":\"opus/16000/1\",\"payload\":112}]\n";

/// Zalo 1-1 call media type wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CallType {
    DirectVoice = 1,
    DirectVideo = 3,
}

impl CallType {
    pub fn from_video_flag(is_video: bool) -> Self {
        if is_video {
            Self::DirectVideo
        } else {
            Self::DirectVoice
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn is_video(self) -> bool {
        matches!(self, Self::DirectVideo)
    }
}

/// Verified Zalo 1-1 voicecall signaling commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingCommand {
    /// `401` -> `/api/voicecall/requestcall`
    RequestCall = 401,
    /// `416` -> `/api/voicecall/request`
    Request = 416,
    /// `407` -> `/api/voicecall/ringring`
    RingRing = 407,
    /// `402` -> `/api/voicecall/answer`
    Answer = 402,
    /// `408` -> `/api/voicecall/answerack`
    AnswerAck = 408,
    /// `405` -> `/api/voicecall/cancel`
    Cancel = 405,
    /// `409` -> `/api/voicecall/endcall`
    EndCall = 409,
    /// `406` -> `/api/voicecall/logendcall`
    LogEndCall = 406,
    /// `12006` -> `/api/voicecall/conf`
    Conf = 12006,
}

impl SignalingCommand {
    pub fn endpoint_path(self) -> &'static str {
        match self {
            Self::RequestCall => "/api/voicecall/requestcall",
            Self::Request => "/api/voicecall/request",
            Self::RingRing => "/api/voicecall/ringring",
            Self::Answer => "/api/voicecall/answer",
            Self::AnswerAck => "/api/voicecall/answerack",
            Self::Cancel => "/api/voicecall/cancel",
            Self::EndCall => "/api/voicecall/endcall",
            Self::LogEndCall => "/api/voicecall/logendcall",
            Self::Conf => "/api/voicecall/conf",
        }
    }

    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            401 => Some(Self::RequestCall),
            416 => Some(Self::Request),
            407 => Some(Self::RingRing),
            402 => Some(Self::Answer),
            408 => Some(Self::AnswerAck),
            405 => Some(Self::Cancel),
            409 => Some(Self::EndCall),
            406 => Some(Self::LogEndCall),
            12006 => Some(Self::Conf),
            _ => None,
        }
    }
}

/// Parameters for `401` `/api/voicecall/requestcall`.
#[derive(Debug, Clone)]
pub struct RequestCallParams {
    pub callee_id: String,
    pub call_id: i64,
    pub call_type: CallType,
    pub codec: String,
    /// Optional pre-populated media fields (only sent on media-carrying `401` variant).
    pub extend_data: Option<String>,
    pub rtp_address: Option<String>,
    pub rtcp_address: Option<String>,
    pub session: Option<String>,
}

impl RequestCallParams {
    pub fn initial(callee_id: impl Into<String>, call_id: i64, call_type: CallType) -> Self {
        Self {
            callee_id: callee_id.into(),
            call_id,
            call_type,
            codec: "[]".to_string(),
            extend_data: None,
            rtp_address: None,
            rtcp_address: None,
            session: None,
        }
    }
}

/// Parameters for `416` `/api/voicecall/request`.
#[derive(Debug, Clone)]
pub struct RequestMediaParams {
    pub callee_id: String,
    pub call_id: i64,
    pub rtp_address: String,
    pub rtcp_address: String,
    pub codec: String,
    pub session: String,
    pub extend_data: Option<String>,
}

/// Parameters for `402` `/api/voicecall/answer`.
#[derive(Debug, Clone)]
pub struct AnswerParams {
    pub caller_id: String,
    pub call_id: i64,
    /// `0` = accept, `3` = reject/decline.
    pub status: i64,
    pub codec: Option<String>,
    pub rtp_address: Option<String>,
    pub rtcp_address: Option<String>,
    pub session_id: Option<String>,
    pub extend_data: Option<String>,
    pub imei: Option<String>,
}

impl AnswerParams {
    pub fn decline(caller_id: impl Into<String>, call_id: i64) -> Self {
        Self {
            caller_id: caller_id.into(),
            call_id,
            status: 3,
            codec: None,
            rtp_address: None,
            rtcp_address: None,
            session_id: None,
            extend_data: None,
            imei: None,
        }
    }

    /// Build a `402` decline payload carrying any known non-empty media fields (`codec`,
    /// `rtpAddress`, `rtcpAddress`, `session_id`) while letting `build_402_body` omit empty ones
    /// so Zalo never returns error `114`.
    pub fn decline_with_media(
        caller_id: impl Into<String>,
        call_id: i64,
        rtp_address: Option<String>,
        rtcp_address: Option<String>,
        session_id: Option<String>,
    ) -> Self {
        Self {
            caller_id: caller_id.into(),
            call_id,
            status: 3,
            codec: Some(OPUS_16K_CODEC_JSON.to_string()),
            rtp_address: rtp_address.filter(|s| !s.trim().is_empty()),
            rtcp_address: rtcp_address.filter(|s| !s.trim().is_empty()),
            session_id: session_id.filter(|s| !s.trim().is_empty()),
            extend_data: None,
            imei: None,
        }
    }
}

/// Parameters for `406` `/api/voicecall/logendcall`.
#[derive(Debug, Clone)]
pub struct LogEndCallParams {
    pub call_id: i64,
    pub partner_id: String,
    pub status: i64,
    pub log_data: String,
    pub duration_secs: u64,
    /// `1` = caller, `0` = callee.
    pub role: u8,
}

/// Parameters for `12006` `/api/voicecall/conf`.
#[derive(Debug, Clone)]
pub struct MediaConfParams {
    pub callee_id: String,
    pub call_id: i64,
    pub is_on_mic: bool,
    pub is_on_cam: bool,
    pub data: String,
}

/// Normalized media handshake fields computed from a `401` response and UDP ZRTP probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteFields {
    pub rtp_ip: String,
    pub rtcp_ip: String,
    pub sess_id: String,
    pub codec: String,
    pub extend_data: String,
}

/// Generate a 154-character URL-safe Base64 call session token (115 random bytes),
/// matching the official desktop client shape.
pub fn generate_call_session() -> CallResult<String> {
    let mut raw = [0u8; 115];
    getrandom::fill(&mut raw)
        .map_err(|e| CallError::Protocol(format!("OS CSPRNG failure: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

const ZALO_PC_CALL_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Zalo/26.10.11 Chrome/122.0.6261.95 Electron/29.1.5 Safari/537.36";

/// Generate an official 69-character PC `imei` (`uuid_v4 + "-" + md5(userAgent)`).
pub fn official_call_imei() -> CallResult<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| CallError::Protocol(format!("OS CSPRNG failure: {e}")))?;
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC 4122 variant
    let hex = bytes.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    Ok(format!("{uuid}-{}", zalo_core::crypto::md5_hex(ZALO_PC_CALL_UA)))
}

/// Normalize a session `imei` into the 69-character PC voicecall `uuid-md5(ua)` shape expected by
/// Zalo's voicecall backend (`ctdozlite` reports degraded invites when shorter values are sent).
///
/// - 69-char values are kept as-is.
/// - 36-char UUIDs (such as `zalo-core::Session.imei`) deterministically append `-{md5(ua)}`.
/// - Other formats fall back to a freshly generated 69-char `official_call_imei()`.
pub fn normalize_call_imei(raw_imei: &str) -> String {
    let trimmed = raw_imei.trim();
    if trimmed.len() == 69 {
        return trimmed.to_string();
    }
    if trimmed.len() == 36 && trimmed.bytes().filter(|&b| b == b'-').count() == 4 {
        return format!(
            "{trimmed}-{}",
            zalo_core::crypto::md5_hex(ZALO_PC_CALL_UA)
        );
    }
    official_call_imei().unwrap_or_else(|_| trimmed.to_string())
}

/// Normalize a `/api/voicecall/*` response `data` field into a JSON object.
pub fn normalize_voicecall_data(raw: Value) -> Value {
    match raw {
        Value::Object(_) => raw,
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                json!({})
            } else if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                match parsed {
                    Value::Object(_) => parsed,
                    other => json!({ "value": other }),
                }
            } else {
                json!({ "value": s })
            }
        }
        Value::Null => json!({}),
        other => json!({ "value": other }),
    }
}

/// Resolve the validated `voice_call` HTTPS base URL for `session`.
pub fn resolve_voice_call_host(session: &Session) -> String {
    let candidate = session.domains.voice_call.trim().trim_end_matches('/');
    if !candidate.is_empty() && is_allowed_zalo_http_url(candidate) {
        return candidate.to_string();
    }
    endpoints::service("voice_call")
        .unwrap_or(endpoints::VOICE_CALL_FALLBACK)
        .trim_end_matches('/')
        .to_string()
}

/// Resolve the caller's lower 32-bit Zalo UID from `zpw_sek` or `profile.uid`.
pub fn session_uid_u32(session: &Session) -> u32 {
    session
        .effective_zpw_sek()
        .as_deref()
        .and_then(|sek| sek.split('.').nth(1))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_else(|| crate::zrtp::uid_str_to_u32(&session.profile.uid))
}

/// Construct caller-side `InviteFields` (`rtp_ip`, `rtcp_ip`, `sess_id`, `codec`, `extend_data`)
/// from a `401` `/api/voicecall/requestcall` response and optional UDP ZRTP probe result.
pub fn build_caller_invite_fields(
    resp_401: &Value,
    is_video: bool,
    local_udp_port: Option<u16>,
    probe: Option<&ZrtpProbeResult>,
) -> InviteFields {
    let servers_arr = resp_401
        .get("servers")
        .or_else(|| resp_401.pointer("/callSetting/servers"))
        .and_then(Value::as_array);

    let first_srv = servers_arr.and_then(|arr| arr.first());
    let selected_rtp = probe
        .and_then(|p| p.selected_rtp.as_deref())
        .filter(|s| !s.is_empty());
    let selected_srv = selected_rtp.and_then(|sel| {
        servers_arr.and_then(|arr| {
            arr.iter().find(|s| {
                s.get("rtpaddr")
                    .or_else(|| s.get("rtp"))
                    .and_then(Value::as_str)
                    == Some(sel)
            })
        })
    });
    let chosen_srv = selected_srv.or(first_srv);

    let rtp_ip = chosen_srv
        .and_then(|s| s.get("rtpaddr").or_else(|| s.get("rtp")))
        .and_then(Value::as_str)
        .or(selected_rtp)
        .or_else(|| {
            resp_401
                .get("rtpIP")
                .or_else(|| resp_401.get("rtpSerIp"))
                .or_else(|| resp_401.get("rtpAddress"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_string();

    let rtcp_ip = chosen_srv
        .and_then(|s| s.get("rtcpaddr").or_else(|| s.get("rtcp")))
        .and_then(Value::as_str)
        .or_else(|| {
            resp_401
                .get("rtcpIP")
                .or_else(|| resp_401.get("rtcpAddress"))
                .or_else(|| resp_401.get("rtpSerIp"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(&rtp_ip)
        .to_string();

    let rtp_v6 = first_srv
        .and_then(|s| s.get("rtpIPv6"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let rtcp_v6 = first_srv
        .and_then(|s| s.get("rtcpIPv6"))
        .and_then(Value::as_str)
        .unwrap_or(rtp_v6);

    let sess_id = resp_401
        .get("sessId")
        .or_else(|| resp_401.get("session"))
        .or_else(|| resp_401.pointer("/callSetting/session"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();

    let port_hint = local_udp_port.or_else(|| probe.map(|p| p.local_port));
    let (local_ip, local_port) = detect_local_p2p_endpoint(port_hint);

    let mut p2p_entries = vec![json!({
        "ip": local_ip,
        "port": local_port,
        "type": 0
    })];
    if let Some((pub_ip, pub_port)) = probe.and_then(|p| p.public_p2p.as_ref()) {
        if !pub_ip.is_empty() && pub_ip != &local_ip {
            p2p_entries.push(json!({
                "ip": pub_ip,
                "port": *pub_port,
                "type": 1
            }));
        }
    }

    let server_result: Vec<Value> = servers_arr
        .map(|arr| {
            arr.iter()
                .map(|srv| {
                    let rtp_str = srv
                        .get("rtpaddr")
                        .or_else(|| srv.get("rtp"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let (recv, rtt) = match probe {
                        Some(p) => p.echo_stats.get(rtp_str).copied().unwrap_or((14, 25)),
                        None => (0, 0),
                    };
                    json!({
                        "recv": recv,
                        "rtcp": srv.get("rtcpaddr").or_else(|| srv.get("rtcp")).cloned().unwrap_or(json!("")),
                        "rtcpIPv6": srv.get("rtcpIPv6").cloned().unwrap_or(json!("")),
                        "rtp": srv.get("rtpaddr").or_else(|| srv.get("rtp")).cloned().unwrap_or(json!("")),
                        "rtpIPv6": srv.get("rtpIPv6").cloned().unwrap_or(json!("")),
                        "rtt": rtt,
                        "spTcp": 1,
                        "tpType": srv.get("tpType").cloned().unwrap_or(json!(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let server_addr: Vec<Value> = servers_arr
        .filter(|arr| !arr.is_empty())
        .map(|arr| {
            arr.iter()
                .map(|srv| {
                    let rtp = srv
                        .get("rtpaddr")
                        .or_else(|| srv.get("rtp"))
                        .cloned()
                        .unwrap_or(json!(""));
                    let rtcp = srv
                        .get("rtcpaddr")
                        .or_else(|| srv.get("rtcp"))
                        .cloned()
                        .unwrap_or_else(|| rtp.clone());
                    json!({
                        "rtcp": rtcp,
                        "rtcpIPv6": srv.get("rtcpIPv6").cloned().unwrap_or(json!("")),
                        "rtp": rtp,
                        "rtpIPv6": srv.get("rtpIPv6").cloned().unwrap_or(json!("")),
                        "tpType": srv.get("tpType").cloned().unwrap_or(json!(0)),
                    })
                })
                .collect()
        })
        .unwrap_or_else(|| {
            vec![json!({
                "rtcp": rtcp_ip,
                "rtcpIPv6": rtcp_v6,
                "rtp": rtp_ip,
                "rtpIPv6": rtp_v6,
                "tpType": 0
            })]
        });

    let num_servers = if probe.is_some() {
        server_result.len()
    } else {
        0
    };

    let extend_obj = json!({
        "callType": if is_video { 1 } else { 0 },
        "fecTP": 0,
        "gccAudio": 1,
        "gccEarlyCall": 0,
        "gccMode": 1,
        "gccSVLR": 1,
        "maxFT": 60,
        "newZrtc": 1,
        "numServers": num_servers,
        "p2p": p2p_entries,
        "packetMode": 2,
        "platform": 2,
        "sP2P": 0,
        "serverAddr": server_addr,
        "serverResult": server_result,
        "spTcp": 1,
        "srtcp": 0,
        "srtpMode": 0,
        "supportCallBusy": 1,
        "supportHevcDecode": 0,
        "tpType": 0,
        "video": {
            "codec": [
                {
                    "name": "h264",
                    "payload": 97
                }
            ]
        }
    });

    InviteFields {
        rtp_ip,
        rtcp_ip,
        sess_id,
        codec: OPUS_16K_CODEC_JSON.to_string(),
        extend_data: format!("{extend_obj}\n"),
    }
}

/// Build the callee-side `extendData` JSON string for `402` `/api/voicecall/answer`.
pub fn build_callee_extend_data(
    rtp_ip: &str,
    rtcp_ip: &str,
    is_video: bool,
    local_udp_port: Option<u16>,
    probe: Option<&ZrtpProbeResult>,
) -> String {
    let port_hint = local_udp_port.or_else(|| probe.map(|p| p.local_port));
    let (local_ip, local_port) = detect_local_p2p_endpoint(port_hint);

    let mut p2p_entries = vec![json!({
        "ip": local_ip,
        "port": local_port,
        "type": 0
    })];
    if let Some((pub_ip, pub_port)) = probe.and_then(|p| p.public_p2p.as_ref()) {
        if !pub_ip.is_empty() && pub_ip != &local_ip {
            p2p_entries.push(json!({
                "ip": pub_ip,
                "port": *pub_port,
                "type": 1
            }));
        }
    }
    let rtcp_val = if rtcp_ip.is_empty() { rtp_ip } else { rtcp_ip };
    let extend_obj = json!({
        "callType": if is_video { 1 } else { 0 },
        "fecTP": 0,
        "gccAudio": 1,
        "gccEarlyCall": 0,
        "gccMode": 1,
        "gccSVLR": 1,
        "maxFT": 60,
        "newZrtc": 1,
        "numServers": 0,
        "p2p": p2p_entries,
        "packetMode": 2,
        "platform": 2,
        "sP2P": 0,
        "select2side": 1,
        "serverAddr": [
            {
                "rtcp": rtcp_val,
                "rtcpIPv6": "",
                "rtp": rtp_ip,
                "rtpIPv6": "",
                "tpType": 0
            }
        ],
        "spTcp": 1,
        "srtcp": 0,
        "srtpMode": 0,
        "supportCallBusy": 1,
        "supportHevcDecode": 0,
        "tpType": 0,
        "video": {
            "codec": [
                {
                    "name": "h264",
                    "payload": 97
                }
            ]
        }
    });
    format!("{extend_obj}\n")
}

// --- Pure payload builders (also exercised directly by unit tests) ---

pub fn build_401_body(params: &RequestCallParams, imei: &str) -> Value {
    if let Some(ref ext) = params.extend_data.as_deref().filter(|s| !s.is_empty()) {
        json!({
            "callId": params.call_id,
            "calleeId": params.callee_id,
            "codec": params.codec,
            "extendData": ext,
            "rtcpAddress": params.rtcp_address.as_deref().unwrap_or(""),
            "rtpAddress": params.rtp_address.as_deref().unwrap_or(""),
            "session": params.session.as_deref().unwrap_or(""),
        })
    } else {
        json!({
            "calleeId": params.callee_id,
            "callId": params.call_id,
            "codec": params.codec,
            "typeRequest": params.call_type.as_u8(),
            "imei": imei,
        })
    }
}

pub fn build_416_body(params: &RequestMediaParams, imei: &str) -> Value {
    let mut body = json!({
        "calleeId": params.callee_id,
        "rtcpAddress": params.rtcp_address,
        "rtpAddress": params.rtp_address,
        "codec": params.codec,
        "session": params.session,
        "callId": params.call_id,
        "imei": imei,
    });
    if let Some(ext) = params.extend_data.as_deref().filter(|s| !s.is_empty()) {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("extendData".into(), json!(ext));
            obj.insert("subCommand".into(), json!(3));
        }
    }
    body
}

pub fn build_407_body(caller_id: &str, call_id: i64, imei: &str) -> Value {
    json!({
        "callerId": caller_id,
        "callId": call_id,
        "status": 0,
        "imei": imei,
    })
}

pub fn build_402_body(params: &AnswerParams) -> Value {
    let mut obj = Map::new();
    obj.insert("callerId".into(), json!(params.caller_id));
    obj.insert("callId".into(), json!(params.call_id));
    obj.insert("status".into(), json!(params.status));
    if let Some(ref c) = params.codec {
        obj.insert("codec".into(), json!(c));
    }
    if let Some(ref rtcp) = params.rtcp_address.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("rtcpAddress".into(), json!(rtcp));
    }
    if let Some(ref rtp) = params.rtp_address.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("rtpAddress".into(), json!(rtp));
    }
    if let Some(ref sid) = params.session_id.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("session_id".into(), json!(sid));
    }
    if let Some(ref imei) = params.imei.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("imei".into(), json!(imei));
    }
    if let Some(ref ext) = params.extend_data.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("extendData".into(), json!(ext));
        obj.insert("subCommand".into(), json!(3));
    }
    Value::Object(obj)
}

pub fn build_408_body(callee_id: &str, call_id: i64, imei: &str) -> Value {
    json!({
        "calleeId": callee_id,
        "callId": call_id,
        "status": 0,
        "imei": imei,
    })
}

pub fn build_405_body(to_id: &str, call_id: i64, call_type: CallType, imei: &str) -> Value {
    json!({
        "callerId": to_id,
        "callId": call_id,
        "callType": call_type.as_u8(),
        "status": 0,
        "imei": imei,
    })
}

pub fn build_409_body(uid_to: &str, call_id: i64, imei: &str) -> Value {
    json!({
        "uidTo": uid_to,
        "callId": call_id,
        "status": 3,
        "imei": imei,
    })
}

pub fn build_406_body(params: &LogEndCallParams, imei: &str) -> Value {
    json!({
        "call_id": params.call_id,
        "partner_id": params.partner_id,
        "status": params.status,
        "logdata": params.log_data,
        "duration": params.duration_secs,
        "role": params.role,
        "imei": imei,
    })
}

pub fn build_12006_body(params: &MediaConfParams) -> Value {
    json!({
        "calleeId": params.callee_id,
        "callId": params.call_id,
        "isOnCam": if params.is_on_cam { 1 } else { 0 },
        "isOnMic": if params.is_on_mic { 1 } else { 0 },
        "data": params.data,
    })
}

/// Stateless VoIP signaling client executing encrypted HTTPS requests against `voicecall-wpa.chat.zalo.me`.
///
/// Every method enforces `session.relay.enabled == false` before encrypting or transmitting.
#[derive(Debug, Default, Clone)]
pub struct SignalingClient {
    zpw_override: Option<(String, String)>,
    imei_override: Option<String>,
    trace: Option<TraceSink>,
}

impl SignalingClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Optional override for `zpw_ver` / `zpw_type` on voicecall requests only (e.g. `("693", "24")`).
    pub fn with_zpw_override(mut self, ver: impl Into<String>, ty: impl Into<String>) -> Self {
        self.zpw_override = Some((ver.into(), ty.into()));
        self
    }

    /// Optional override for the `imei` field transmitted on `/api/voicecall/*` requests.
    pub fn with_imei_override(mut self, imei: impl Into<String>) -> Self {
        self.imei_override = Some(imei.into());
        self
    }

    /// Optional sanitized wire trace sink logging voicecall requests/responses without secrets.
    pub fn with_trace(mut self, sink: TraceSink) -> Self {
        self.trace = Some(sink);
        self
    }

    /// Effective 69-character voicecall `imei` sent for `session`.
    pub fn effective_imei(&self, session: &Session) -> String {
        if let Some(ref ov) = self.imei_override {
            return ov.clone();
        }
        normalize_call_imei(&session.imei)
    }

    fn zpw_query(&self, session: &Session) -> String {
        if let Some((ref ver, ref ty)) = self.zpw_override {
            format!("zpw_ver={ver}&zpw_type={ty}")
        } else {
            session.common_params()
        }
    }

    /// Build the encrypted voicecall GET URL without sending it (useful for verification & tests).
    pub fn build_voicecall_url(
        &self,
        session: &Session,
        endpoint_path: &str,
        payload: &Value,
    ) -> CallResult<String> {
        ensure_callable_session(session)?;
        let encoded = session
            .encoded_params(payload)
            .map_err(|e| CallError::Protocol(format!("Không mã hóa được tham số voicecall: {e}")))?;
        let host = resolve_voice_call_host(session);
        Ok(format!(
            "{host}{endpoint_path}?{}&params={encoded}",
            self.zpw_query(session)
        ))
    }

    async fn voicecall_get(
        &self,
        session: &Session,
        endpoint_path: &str,
        payload: &Value,
    ) -> CallResult<Value> {
        let host = resolve_voice_call_host(session);
        let zpw_q = self.zpw_query(session);
        emit_trace(self.trace.as_ref(), || {
            format!(
                "[SIG TX] {host}{endpoint_path} ({zpw_q}) body={}",
                format_trace_json(payload, 900)
            )
        });
        let t0 = Instant::now();
        let url = self.build_voicecall_url(session, endpoint_path, payload)?;
        let resp = match session.get_json(&url).await.map_err(map_session_anyhow_err) {
            Ok(v) => v,
            Err(e) => {
                emit_trace(self.trace.as_ref(), || {
                    format!(
                        "[SIG RX] {endpoint_path} err ({}ms): {e}",
                        t0.elapsed().as_millis()
                    )
                });
                return Err(e);
            }
        };
        let decrypted = session.maybe_decrypt(resp, session.session_key_bytes());
        if let Err(e) = check_zalo_error_code(&decrypted) {
            emit_trace(self.trace.as_ref(), || {
                format!(
                    "[SIG RX] {endpoint_path} error ({}ms): {e} raw={}",
                    t0.elapsed().as_millis(),
                    format_trace_json(&decrypted, 600)
                )
            });
            return Err(e);
        }
        let raw_data = decrypted.get("data").cloned().unwrap_or(Value::Null);
        let norm = normalize_voicecall_data(raw_data);
        emit_trace(self.trace.as_ref(), || {
            format!(
                "[SIG RX] {endpoint_path} ok ({}ms) data={}",
                t0.elapsed().as_millis(),
                format_trace_json(&norm, 900)
            )
        });
        Ok(norm)
    }

    /// `401` -> `/api/voicecall/requestcall`
    pub async fn request_call_401(
        &self,
        session: &Session,
        params: &RequestCallParams,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_401_body(params, &imei);
        self.voicecall_get(session, SignalingCommand::RequestCall.endpoint_path(), &body)
            .await
    }

    /// `416` -> `/api/voicecall/request`
    pub async fn request_416(
        &self,
        session: &Session,
        params: &RequestMediaParams,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_416_body(params, &imei);
        self.voicecall_get(session, SignalingCommand::Request.endpoint_path(), &body)
            .await
    }

    /// `407` -> `/api/voicecall/ringring`
    pub async fn ring_ring_407(
        &self,
        session: &Session,
        caller_id: &str,
        call_id: i64,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_407_body(caller_id, call_id, &imei);
        self.voicecall_get(session, SignalingCommand::RingRing.endpoint_path(), &body)
            .await
    }

    /// `402` -> `/api/voicecall/answer`
    pub async fn answer_402(&self, session: &Session, params: &AnswerParams) -> CallResult<Value> {
        let body = build_402_body(params);
        self.voicecall_get(session, SignalingCommand::Answer.endpoint_path(), &body)
            .await
    }

    /// `408` -> `/api/voicecall/answerack`
    pub async fn answer_ack_408(
        &self,
        session: &Session,
        callee_id: &str,
        call_id: i64,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_408_body(callee_id, call_id, &imei);
        self.voicecall_get(session, SignalingCommand::AnswerAck.endpoint_path(), &body)
            .await
    }

    /// `405` -> `/api/voicecall/cancel`
    pub async fn cancel_405(
        &self,
        session: &Session,
        to_id: &str,
        call_id: i64,
        call_type: CallType,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_405_body(to_id, call_id, call_type, &imei);
        self.voicecall_get(session, SignalingCommand::Cancel.endpoint_path(), &body)
            .await
    }

    /// `409` -> `/api/voicecall/endcall`
    pub async fn end_call_409(
        &self,
        session: &Session,
        uid_to: &str,
        call_id: i64,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_409_body(uid_to, call_id, &imei);
        self.voicecall_get(session, SignalingCommand::EndCall.endpoint_path(), &body)
            .await
    }

    /// `406` -> `/api/voicecall/logendcall`
    pub async fn log_end_call_406(
        &self,
        session: &Session,
        params: &LogEndCallParams,
    ) -> CallResult<Value> {
        let imei = self.effective_imei(session);
        let body = build_406_body(params, &imei);
        self.voicecall_get(session, SignalingCommand::LogEndCall.endpoint_path(), &body)
            .await
    }

    /// `12006` -> `/api/voicecall/conf`
    pub async fn set_media_conf_12006(
        &self,
        session: &Session,
        params: &MediaConfParams,
    ) -> CallResult<Value> {
        let body = build_12006_body(params);
        self.voicecall_get(session, SignalingCommand::Conf.endpoint_path(), &body)
            .await
    }

    /// Raw command dispatcher covering all supported 1-1 commands (`401`, `416`, `407`, `402`,
    /// `408`, `405`, `409`, `406`, `12006`).
    pub async fn dispatch_command(
        &self,
        session: &Session,
        command: i64,
        n: &Value,
    ) -> CallResult<Value> {
        ensure_callable_session(session)?;
        let Some(cmd) = SignalingCommand::from_code(command) else {
            return Err(CallError::InvalidArgument(format!(
                "Lệnh tín hiệu không được hỗ trợ: {command}"
            )));
        };
        let imei = self.effective_imei(session);
        let body = match cmd {
            SignalingCommand::RequestCall => {
                let has_media = n
                    .get("extendData")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                let params = RequestCallParams {
                    callee_id: value_as_string(n.get("calleeId")),
                    call_id: value_as_i64(n.get("callId")),
                    call_type: if value_as_i64(n.get("typeRequest").or_else(|| n.get("type"))) == 3
                    {
                        CallType::DirectVideo
                    } else {
                        CallType::DirectVoice
                    },
                    codec: n
                        .get("codec")
                        .and_then(Value::as_str)
                        .unwrap_or("[]")
                        .to_string(),
                    extend_data: if has_media {
                        n.get("extendData").and_then(Value::as_str).map(str::to_string)
                    } else {
                        None
                    },
                    rtp_address: n.get("rtpAddress").and_then(Value::as_str).map(str::to_string),
                    rtcp_address: n.get("rtcpAddress").and_then(Value::as_str).map(str::to_string),
                    session: n.get("session").and_then(Value::as_str).map(str::to_string),
                };
                build_401_body(&params, &imei)
            }
            SignalingCommand::Request => {
                let params = RequestMediaParams {
                    callee_id: value_as_string(n.get("calleeId")),
                    call_id: value_as_i64(n.get("callId")),
                    rtp_address: value_as_string(n.get("rtpAddress")),
                    rtcp_address: value_as_string(n.get("rtcpAddress")),
                    codec: n
                        .get("codec")
                        .and_then(Value::as_str)
                        .unwrap_or("[]")
                        .to_string(),
                    session: value_as_string(n.get("session")),
                    extend_data: n
                        .get("extendData")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                };
                build_416_body(&params, &imei)
            }
            SignalingCommand::RingRing => build_407_body(
                &value_as_string(n.get("callerId")),
                value_as_i64(n.get("callId")),
                &imei,
            ),
            SignalingCommand::Answer => {
                let params = AnswerParams {
                    caller_id: value_as_string(n.get("callerId")),
                    call_id: value_as_i64(n.get("callId")),
                    status: value_as_i64(n.get("status")),
                    codec: n.get("codec").and_then(Value::as_str).map(str::to_string),
                    rtp_address: n
                        .get("rtpAddress")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    rtcp_address: n
                        .get("rtcpAddress")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    session_id: n
                        .get("session_id")
                        .or_else(|| n.get("session"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    extend_data: n
                        .get("extendData")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    imei: n
                        .get("imei")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                };
                build_402_body(&params)
            }
            SignalingCommand::AnswerAck => build_408_body(
                &value_as_string(n.get("calleeId")),
                value_as_i64(n.get("callId")),
                &imei,
            ),
            SignalingCommand::Cancel => {
                let ct = if value_as_i64(n.get("callType")) == 3 {
                    CallType::DirectVideo
                } else {
                    CallType::DirectVoice
                };
                build_405_body(
                    &value_as_string(n.get("toId").or_else(|| n.get("callerId"))),
                    value_as_i64(n.get("callId")),
                    ct,
                    &imei,
                )
            }
            SignalingCommand::EndCall => build_409_body(
                &value_as_string(n.get("toId").or_else(|| n.get("uidTo"))),
                value_as_i64(n.get("callId")),
                &imei,
            ),
            SignalingCommand::LogEndCall => {
                let params = LogEndCallParams {
                    call_id: value_as_i64(n.get("callId")),
                    partner_id: value_as_string(n.get("partnerId")),
                    status: value_as_i64(n.get("status")),
                    log_data: value_as_string(n.get("data")),
                    duration_secs: value_as_i64(n.get("duration")).max(0) as u64,
                    role: value_as_i64(n.get("role")).clamp(0, 1) as u8,
                };
                build_406_body(&params, &imei)
            }
            SignalingCommand::Conf => {
                let params = MediaConfParams {
                    callee_id: value_as_string(n.get("calleeId")),
                    call_id: value_as_i64(n.get("callId")),
                    is_on_mic: value_as_i64(n.get("isOnMic")) != 0,
                    is_on_cam: value_as_i64(n.get("isOnCam")) != 0,
                    data: value_as_string(n.get("data")),
                };
                build_12006_body(&params)
            }
        };
        self.voicecall_get(session, cmd.endpoint_path(), &body).await
    }
}

/// Boxed `Send` future alias used by [`SignalingTransport`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Abstract VoIP signaling transport seam used by [`crate::engine::CallEngine`].
///
/// Production code uses [`SessionSignalingTransport`] backed by `zalo-core::Session` +
/// [`SignalingClient`]; tests can inject deterministic mock transports to verify full
/// async call orchestration without touching external `*.zalo.me` servers.
pub trait SignalingTransport: Send + Sync {
    fn check_callable(&self) -> CallResult<()>;
    fn self_uid(&self) -> String;
    fn self_uid_u32(&self) -> u32;

    fn request_call_401<'a>(
        &'a self,
        params: &'a RequestCallParams,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn request_416<'a>(
        &'a self,
        params: &'a RequestMediaParams,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn ring_ring_407<'a>(
        &'a self,
        caller_id: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn answer_402<'a>(&'a self, params: &'a AnswerParams) -> BoxFuture<'a, CallResult<Value>>;
    fn answer_ack_408<'a>(
        &'a self,
        callee_id: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn cancel_405<'a>(
        &'a self,
        to_id: &'a str,
        call_id: i64,
        call_type: CallType,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn end_call_409<'a>(
        &'a self,
        uid_to: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn log_end_call_406<'a>(
        &'a self,
        params: &'a LogEndCallParams,
    ) -> BoxFuture<'a, CallResult<Value>>;
    fn set_media_conf_12006<'a>(
        &'a self,
        params: &'a MediaConfParams,
    ) -> BoxFuture<'a, CallResult<Value>>;
}

/// Live [`SignalingTransport`] backed by a real `zalo-core::Session` and [`SignalingClient`].
pub struct SessionSignalingTransport {
    session: Session,
    client: SignalingClient,
}

impl SessionSignalingTransport {
    pub fn new(session: Session, client: SignalingClient) -> CallResult<Self> {
        ensure_callable_session(&session)?;
        Ok(Self { session, client })
    }
}

impl SignalingTransport for SessionSignalingTransport {
    fn check_callable(&self) -> CallResult<()> {
        ensure_callable_session(&self.session)
    }

    fn self_uid(&self) -> String {
        self.session.profile.uid.clone()
    }

    fn self_uid_u32(&self) -> u32 {
        session_uid_u32(&self.session)
    }

    fn request_call_401<'a>(
        &'a self,
        params: &'a RequestCallParams,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.request_call_401(&self.session, params))
    }

    fn request_416<'a>(
        &'a self,
        params: &'a RequestMediaParams,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.request_416(&self.session, params))
    }

    fn ring_ring_407<'a>(
        &'a self,
        caller_id: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.ring_ring_407(&self.session, caller_id, call_id))
    }

    fn answer_402<'a>(&'a self, params: &'a AnswerParams) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.answer_402(&self.session, params))
    }

    fn answer_ack_408<'a>(
        &'a self,
        callee_id: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.answer_ack_408(&self.session, callee_id, call_id))
    }

    fn cancel_405<'a>(
        &'a self,
        to_id: &'a str,
        call_id: i64,
        call_type: CallType,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(
            self.client
                .cancel_405(&self.session, to_id, call_id, call_type),
        )
    }

    fn end_call_409<'a>(
        &'a self,
        uid_to: &'a str,
        call_id: i64,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.end_call_409(&self.session, uid_to, call_id))
    }

    fn log_end_call_406<'a>(
        &'a self,
        params: &'a LogEndCallParams,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.log_end_call_406(&self.session, params))
    }

    fn set_media_conf_12006<'a>(
        &'a self,
        params: &'a MediaConfParams,
    ) -> BoxFuture<'a, CallResult<Value>> {
        Box::pin(self.client.set_media_conf_12006(&self.session, params))
    }
}

fn check_zalo_error_code(decrypted: &Value) -> CallResult<()> {
    if let Some(code) = decrypted.get("error_code").and_then(Value::as_i64) {
        if zalo_core::session::is_session_expired_code(code) {
            return Err(CallError::SessionExpired { code });
        }
        if code != 0 {
            let msg = decrypted
                .get("error_message")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            return Err(CallError::Signaling { code, message: msg });
        }
    }
    Ok(())
}

fn map_session_anyhow_err(err: anyhow::Error) -> CallError {
    if let Some(se) = err.downcast_ref::<SessionError>() {
        match se {
            SessionError::Expired { code } => CallError::SessionExpired { code: *code },
            SessionError::Network(msg) => CallError::Network(msg.clone()),
            SessionError::Http { status, body } => {
                CallError::Network(format!("HTTP {status}: {body}"))
            }
            SessionError::Protocol { code, message } => CallError::Signaling {
                code: *code,
                message: message.clone(),
            },
        }
    } else {
        CallError::Network(err.to_string())
    }
}

fn value_as_string(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn value_as_i64(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Optional HTTP `lp_control` polling fallback for durable VoIP control events
// ---------------------------------------------------------------------------

/// Default Zalo `lp_control` queue IDs matching official PC client / `ctdozlite`.
pub const DEFAULT_LP_CONTROL_QUEUES: &[&str] = &["4", "5", "14", "17", "18", "19", "20"];

fn default_cursor_id() -> String {
    "1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueueCursor {
    #[serde(rename = "lastId", default = "default_cursor_id")]
    pub last_id: String,
    #[serde(default, alias = "preIds")]
    pub ids: Vec<String>,
}

impl Default for QueueCursor {
    fn default() -> Self {
        Self {
            last_id: default_cursor_id(),
            ids: Vec::new(),
        }
    }
}

/// Cursor state for polling `lp_control` (`p*-ctl-chat.zaloapp.com`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrlPollState {
    pub last_action_id: String,
    pub last_action_id_other: String,
    pub queues: BTreeMap<String, QueueCursor>,
    #[serde(default)]
    pub initialized: bool,
    /// True only once multi-page initial backlog draining (`more == 1`) has reached server head.
    #[serde(default)]
    pub initial_drain_done: bool,
    /// Optional minimum timestamp floor (Unix ms) applied to `request` invites seen during the
    /// initial backlog drain so back-to-back CLI runs never replay previously declined invites.
    #[serde(skip)]
    pub min_initial_invite_ts_ms: Option<i64>,
    /// True when the most recent `lp_control` response signaled `more == 1` (more queued pages ready).
    #[serde(skip)]
    pub last_more: bool,
}

impl Default for CtrlPollState {
    fn default() -> Self {
        let env_queues: Vec<String> = std::env::var("ZCALL_QUEUES")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !env_queues.is_empty() {
            let refs: Vec<&str> = env_queues.iter().map(String::as_str).collect();
            Self::with_queues(&refs)
        } else {
            Self::with_queues(DEFAULT_LP_CONTROL_QUEUES)
        }
    }
}

fn is_newer_cursor(candidate: &str, current: &str) -> bool {
    let cand = candidate.trim();
    if cand.is_empty() || cand == "0" {
        return false;
    }
    let curr = current.trim();
    if curr.is_empty() || curr == "0" {
        return true;
    }
    match (cand.parse::<u128>(), curr.parse::<u128>()) {
        (Ok(c), Ok(cur)) => c > cur,
        _ => cand != curr,
    }
}

impl CtrlPollState {
    /// Construct a fresh `CtrlPollState` initialized with the specified queue IDs.
    pub fn with_queues(queue_ids: &[&str]) -> Self {
        let mut queues = BTreeMap::new();
        for qid in queue_ids {
            let clean = qid.trim();
            if !clean.is_empty() {
                queues.insert(clean.to_string(), QueueCursor::default());
            }
        }
        Self {
            last_action_id: default_cursor_id(),
            last_action_id_other: default_cursor_id(),
            queues,
            initialized: false,
            initial_drain_done: false,
            min_initial_invite_ts_ms: None,
            last_more: false,
        }
    }

    /// Reject initial-backlog `request` invites older than `cutoff_ms` (Unix epoch ms) while
    /// draining `more=1` startup pages so fresh CLI sessions never ghost-ring on prior calls.
    pub fn with_live_cutoff_ms(mut self, cutoff_ms: i64) -> Self {
        self.min_initial_invite_ts_ms = Some(cutoff_ms);
        self
    }

    /// Construct a fresh `CtrlPollState` configured to ignore any initial-backlog `request` invites
    /// from more than 2 seconds before poller startup (prevents ghost-ringing on recent sessions
    /// after back-to-back CLI runs or quick restarts).
    pub fn new_live_only() -> Self {
        let cutoff = chrono::Utc::now().timestamp_millis().saturating_sub(2_000);
        Self::default().with_live_cutoff_ms(cutoff)
    }

    /// True once initial multi-page backlog sync (`more=1` -> `more=0`) has completed.
    pub fn is_backlog_synced(&self) -> bool {
        self.initial_drain_done
    }

    /// Encode the `params` query value in Zalo's `lp_control` CSV+Base64 wire format:
    /// `{lastActionId},{lastActionIdOther},{urlencode("{base64(queues_json)},1")}`.
    pub fn encode_params(&self) -> String {
        let queues_json = serde_json::to_string(&self.queues).unwrap_or_else(|_| "{}".to_string());
        let b64 = STANDARD.encode(queues_json.as_bytes());
        format!(
            "{},{},{}",
            self.last_action_id,
            self.last_action_id_other,
            urlencode(&format!("{b64},1"))
        )
    }

    pub fn update_from_response(&mut self, data_node: &Value) {
        self.last_more = false;
        let Some(obj) = data_node.as_object() else {
            return;
        };
        if obj.is_empty() {
            return;
        }
        self.last_more = obj
            .get("more")
            .and_then(|v| v.as_i64().or_else(|| v.as_bool().map(i64::from)))
            .is_some_and(|m| m != 0);
        let mut cursor_touched = false;

        if let Some(reset_id) = obj
            .get("resetLastActionId")
            .and_then(val_id_str)
            .filter(|s| !s.is_empty() && s != "0")
        {
            self.last_action_id = reset_id.clone();
            self.last_action_id_other = default_cursor_id();
            for cur in self.queues.values_mut() {
                cur.last_id = reset_id.clone();
                cur.ids.clear();
            }
            cursor_touched = true;
        }

        if let Some(id) = obj.get("lastActionId").and_then(val_id_str) {
            if is_newer_cursor(&id, &self.last_action_id) {
                self.last_action_id = id;
                cursor_touched = true;
            }
        }
        if let Some(id) = obj.get("lastActionIdOther").and_then(val_id_str) {
            if is_newer_cursor(&id, &self.last_action_id_other) {
                self.last_action_id_other = id;
                cursor_touched = true;
            }
        }
        if let Some(qs) = obj.get("queueStatus").and_then(Value::as_object) {
            for (qid, entry) in qs {
                let cur = self.queues.entry(qid.clone()).or_default();
                if let Some(lid) = entry.get("lastId").and_then(val_id_str) {
                    if is_newer_cursor(&lid, &cur.last_id) {
                        cur.last_id = lid;
                        cursor_touched = true;
                    }
                }
                if let Some(arr) = entry.get("ids").and_then(Value::as_array) {
                    let max_lid = cur.last_id.parse::<u128>().unwrap_or(u128::MAX);
                    let mut new_ids: Vec<String> = arr
                        .iter()
                        .filter_map(val_id_str)
                        .filter(|s| s.parse::<u128>().map(|n| n <= max_lid).unwrap_or(true))
                        .collect();
                    new_ids.sort_by(|a, b| {
                        b.parse::<u128>()
                            .unwrap_or(0)
                            .cmp(&a.parse::<u128>().unwrap_or(0))
                            .then_with(|| b.cmp(a))
                    });
                    new_ids.dedup();
                    new_ids.truncate(10);
                    new_ids.reverse();
                    if new_ids.is_empty() {
                        cur.ids.clear();
                    } else {
                        cur.ids = new_ids;
                    }
                    cursor_touched = true;
                }
            }
        }

        if cursor_touched || obj.get("controls").is_some() {
            self.initialized = true;
            if !self.last_more {
                self.initial_drain_done = true;
            }
        }
    }
}

fn val_id_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Apply one decrypted `lp_control` JSON payload to `state` and extract live VoIP control events.
pub fn apply_lp_control_response(
    state: &mut CtrlPollState,
    decrypted: &Value,
    now_ms: i64,
) -> CallResult<Vec<VoipControlEvent>> {
    check_zalo_error_code(decrypted)?;
    let is_initial_drain = !state.initial_drain_done;
    let mut data_node = decrypted.get("data").unwrap_or(decrypted);
    if let Some(inner) = data_node.get("data").filter(|v| v.is_object()) {
        data_node = inner;
    }
    state.update_from_response(data_node);
    let events = extract_voip_controls(data_node);
    if is_initial_drain {
        let terminated_call_ids: HashSet<i64> = events
            .iter()
            .filter_map(|ev| {
                let cid = ev.call_id();
                if cid == 0 {
                    return None;
                }
                let terminated = matches!(ev.act.as_str(), "end_call" | "cancel")
                    || (ev.act == "answer" && ev.status() != Some(5));
                terminated.then_some(cid)
            })
            .collect();
        return Ok(events
            .into_iter()
            .filter(|ev| {
                if ev.act != "request" {
                    return false;
                }
                let cid = ev.call_id();
                if cid != 0 && terminated_call_ids.contains(&cid) {
                    return false;
                }
                let ev_ts = value_as_i64(ev.data.get("ts"));
                let ev_ms = if (1_000_000_000..10_000_000_000).contains(&ev_ts) {
                    ev_ts.saturating_mul(1000)
                } else {
                    ev_ts
                };
                if ev_ms <= 0 || now_ms.saturating_sub(ev_ms) > 45_000 {
                    return false;
                }
                if let Some(min_ms) = state.min_initial_invite_ts_ms {
                    if ev_ms < min_ms {
                        return false;
                    }
                }
                true
            })
            .collect());
    }
    Ok(events)
}

/// Poll `lp_control` once for VoIP control events (refused when `session.relay.enabled == true`).
pub async fn poll_voip_controls(
    session: &Session,
    state: &mut CtrlPollState,
) -> CallResult<Vec<VoipControlEvent>> {
    poll_voip_controls_with_trace(session, state, None).await
}

/// Poll `lp_control` once with optional sanitized trace logging (never logs secrets/cookies).
pub async fn poll_voip_controls_with_trace(
    session: &Session,
    state: &mut CtrlPollState,
    trace: Option<&TraceSink>,
) -> CallResult<Vec<VoipControlEvent>> {
    ensure_callable_session(session)?;
    let host = if is_allowed_zalo_http_url(&session.domains.lp_control) {
        session.domains.lp_control.trim_end_matches('/')
    } else {
        endpoints::CONVERS_CTL
    };
    let ts = chrono::Utc::now().timestamp();
    let encoded_params = state.encode_params();
    let url = format!(
        "{host}/?{}&params={encoded_params}&ts={ts}",
        session.common_params(),
    );
    emit_trace(trace, || {
        format!(
            "[POLL TX] host={host} queues={} lastActionId={} other={} init={}",
            state.queues.len(),
            state.last_action_id,
            state.last_action_id_other,
            state.initialized
        )
    });
    let t0 = Instant::now();
    let resp = match session
        .get_json_with_timeout(&url, Duration::from_secs(60))
        .await
        .map_err(map_session_anyhow_err)
    {
        Ok(v) => v,
        Err(e) => {
            emit_trace(trace, || {
                format!("[POLL RX] err ({}ms): {e}", t0.elapsed().as_millis())
            });
            return Err(e);
        }
    };
    let decrypted = session.maybe_decrypt(resp, session.session_key_bytes());
    let is_empty_keepalive = decrypted
        .get("data")
        .is_none_or(|d| d.is_null() || d.as_object().is_some_and(|m| m.is_empty()));
    let res = apply_lp_control_response(state, &decrypted, chrono::Utc::now().timestamp_millis());
    match &res {
        Ok(events) => {
            emit_trace(trace, || {
                let kind = if is_empty_keepalive {
                    "keepalive(empty)"
                } else {
                    "payload"
                };
                format!(
                    "[POLL RX] {kind} ({}ms) voip_events={} more={} lastActionId={} init={}",
                    t0.elapsed().as_millis(),
                    events.len(),
                    state.last_more,
                    state.last_action_id,
                    state.initialized
                )
            });
        }
        Err(e) => {
            emit_trace(trace, || {
                format!(
                    "[POLL RX] error ({}ms): {e} raw={}",
                    t0.elapsed().as_millis(),
                    format_trace_json(&decrypted, 500)
                )
            });
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use zalo_core::relay::{RelayConfig, RelayMode};

    fn make_auth_session(relay_enabled: bool) -> Session {
        let mut s = Session::new().unwrap();
        s.set_zpw_sek("1.435550501.secret_token".into());
        s.set_zpw_enk("0123456789abcdef0123456789abcdef");
        s.profile.uid = "435550501".into();
        if relay_enabled {
            s.set_relay(RelayConfig {
                enabled: true,
                mode: RelayMode::Socks5,
                host: "10.0.0.5".into(),
                port: 1088,
                username: String::new(),
                password: String::new(),
                route_webview_media: true,
            })
            .unwrap();
        }
        s
    }

    #[tokio::test]
    async fn signaling_client_rejects_every_endpoint_when_relay_enabled() {
        let sess = make_auth_session(true);
        let client = SignalingClient::new();

        assert!(client
            .request_call_401(&sess, &RequestCallParams::initial("200", 1, CallType::DirectVoice))
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .request_416(
                &sess,
                &RequestMediaParams {
                    callee_id: "200".into(),
                    call_id: 1,
                    rtp_address: "1.2.3.4:1000".into(),
                    rtcp_address: "1.2.3.4:1001".into(),
                    codec: OPUS_16K_CODEC_JSON.into(),
                    session: "s".into(),
                    extend_data: Some("{}".into()),
                }
            )
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .ring_ring_407(&sess, "200", 1)
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .answer_402(&sess, &AnswerParams::decline("200", 1))
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .answer_ack_408(&sess, "200", 1)
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .cancel_405(&sess, "200", 1, CallType::DirectVoice)
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .end_call_409(&sess, "200", 1)
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .log_end_call_406(
                &sess,
                &LogEndCallParams {
                    call_id: 1,
                    partner_id: "200".into(),
                    status: 0,
                    log_data: "{}".into(),
                    duration_secs: 10,
                    role: 1,
                }
            )
            .await
            .unwrap_err()
            .is_relay_unsupported());
        assert!(client
            .set_media_conf_12006(
                &sess,
                &MediaConfParams {
                    callee_id: "200".into(),
                    call_id: 1,
                    is_on_mic: true,
                    is_on_cam: false,
                    data: String::new(),
                }
            )
            .await
            .unwrap_err()
            .is_relay_unsupported());

        let mut poll_state = CtrlPollState::default();
        assert!(poll_voip_controls(&sess, &mut poll_state)
            .await
            .unwrap_err()
            .is_relay_unsupported());
    }

    #[test]
    fn signaling_payload_builders_match_zalo_wire_contracts() {
        let p401_bare = RequestCallParams::initial("999", 464564007, CallType::DirectVoice);
        let b401 = build_401_body(&p401_bare, "test-imei");
        assert_eq!(b401["calleeId"], "999");
        assert_eq!(b401["callId"], 464564007);
        assert_eq!(b401["typeRequest"], 1);
        assert_eq!(b401["imei"], "test-imei");
        assert!(b401.get("subCommand").is_none());

        let b416 = build_416_body(
            &RequestMediaParams {
                callee_id: "999".into(),
                call_id: 464564007,
                rtp_address: "42.113.206.4:10490".into(),
                rtcp_address: "42.113.206.4:12490".into(),
                codec: OPUS_16K_CODEC_JSON.into(),
                session: "sess_1".into(),
                extend_data: Some("{\"callType\":0}\n".into()),
            },
            "test-imei",
        );
        assert_eq!(b416["subCommand"], 3);
        assert_eq!(b416["extendData"], "{\"callType\":0}\n");

        // Decline 402 omits empty rtpAddress / session_id / extendData so server never returns 114
        let b402_decline = build_402_body(&AnswerParams::decline("888", 464564007));
        assert_eq!(b402_decline["callerId"], "888");
        assert_eq!(b402_decline["status"], 3);
        assert!(b402_decline.get("rtpAddress").is_none());
        assert!(b402_decline.get("session_id").is_none());
        assert!(b402_decline.get("subCommand").is_none());

        // Accept 402 includes subCommand=3 alongside extendData
        let b402_accept = build_402_body(&AnswerParams {
            caller_id: "888".into(),
            call_id: 464564007,
            status: 0,
            codec: Some(OPUS_16K_CODEC_JSON.into()),
            rtp_address: Some("42.113.206.4:10490".into()),
            rtcp_address: Some("42.113.206.4:12490".into()),
            session_id: Some("sess_1".into()),
            extend_data: Some("{\"select2side\":1}\n".into()),
            imei: None,
        });
        assert_eq!(b402_accept["status"], 0);
        assert_eq!(b402_accept["subCommand"], 3);
        assert_eq!(b402_accept["session_id"], "sess_1");

        // 405 cancel uses callerId key for peer ID
        let b405 = build_405_body("999", 464564007, CallType::DirectVoice, "test-imei");
        assert_eq!(b405["callerId"], "999");
        assert_eq!(b405["callType"], 1);

        // 409 endcall uses status 3 and uidTo
        let b409 = build_409_body("999", 464564007, "test-imei");
        assert_eq!(b409["uidTo"], "999");
        assert_eq!(b409["status"], 3);

        // 12006 conf
        let b12006 = build_12006_body(&MediaConfParams {
            callee_id: "999".into(),
            call_id: 464564007,
            is_on_mic: false,
            is_on_cam: false,
            data: String::new(),
        });
        assert_eq!(b12006["isOnMic"], 0);
        assert_eq!(b12006["isOnCam"], 0);
    }

    #[test]
    fn builds_encrypted_voicecall_url_and_invite_fields() {
        let sess = make_auth_session(false);
        let client = SignalingClient::new();
        let url = client
            .build_voicecall_url(&sess, "/api/voicecall/ringring", &json!({"callId": 1}))
            .unwrap();
        assert!(url.starts_with("https://voicecall-wpa.chat.zalo.me/api/voicecall/ringring?"));
        assert!(url.contains("&params="));
        assert_eq!(session_uid_u32(&sess), 435_550_501);

        let resp_401 = json!({
            "id": 464564007,
            "sessId": "server_sess_123",
            "servers": [
                { "rtpaddr": "42.113.206.4:10490", "rtcpaddr": "42.113.206.4:12490" },
                { "rtpaddr": "171.244.128.62:4200", "rtcpaddr": "171.244.128.62:4200" }
            ]
        });
        let mut probe = ZrtpProbeResult {
            local_port: 55123,
            public_p2p: Some(("203.0.113.9".into(), 55123)),
            selected_rtp: Some("171.244.128.62:4200".into()),
            token: 0x1234,
            ..Default::default()
        };
        probe
            .echo_stats
            .insert("171.244.128.62:4200".into(), (14, 15));

        let invite = build_caller_invite_fields(&resp_401, false, Some(55123), Some(&probe));
        assert_eq!(invite.rtp_ip, "171.244.128.62:4200");
        assert_eq!(invite.rtcp_ip, "171.244.128.62:4200");
        assert_eq!(invite.sess_id, "server_sess_123");
        let ext: Value = serde_json::from_str(invite.extend_data.trim()).unwrap();
        assert_eq!(ext["numServers"], 2);
        assert_eq!(ext["callType"], 0);

        let callee_ext_str = build_callee_extend_data(
            "42.113.206.4:10490",
            "42.113.206.4:12490",
            false,
            Some(55123),
            Some(&probe),
        );
        let callee_ext: Value = serde_json::from_str(callee_ext_str.trim()).unwrap();
        assert_eq!(callee_ext["select2side"], 1);
        assert_eq!(callee_ext["numServers"], 0);

        // Session & IMEI shapes
        assert_eq!(generate_call_session().unwrap().len(), 154);
        let official = official_call_imei().unwrap();
        assert_eq!(official.len(), 69);
        assert_eq!(normalize_call_imei(&official), official);

        // 36-char UUID from `zalo-core::Session` deterministically normalizes to 69-char PC IMEI
        let uuid36 = "2820ea6b-8652-403d-b2dd-7fc95213b59f";
        let norm1 = normalize_call_imei(uuid36);
        let norm2 = normalize_call_imei(uuid36);
        assert_eq!(norm1.len(), 69);
        assert_eq!(norm1, norm2);
        assert!(norm1.starts_with(uuid36));
        assert_eq!(normalize_call_imei("short-hex").len(), 69);

        // zpw override and error_code checker
        let client_ov = SignalingClient::new().with_zpw_override("693", "24");
        let url_ov = client_ov
            .build_voicecall_url(&sess, "/api/voicecall/requestcall", &json!({"callId": 1}))
            .unwrap();
        assert!(url_ov.contains("zpw_ver=693&zpw_type=24&params="));
        assert!(check_zalo_error_code(&json!({"error_code": 0})).is_ok());
        assert!(matches!(
            check_zalo_error_code(&json!({"error_code": -1001, "error_message": "expired"})),
            Err(CallError::SessionExpired { .. }) | Err(CallError::Signaling { code: -1001, .. })
        ));
        assert!(matches!(
            check_zalo_error_code(&json!({"error_code": 114, "error_message": "bad param"})),
            Err(CallError::Signaling { code: 114, .. })
        ));
    }

    #[test]
    fn lp_control_cursor_initial_filter_never_gets_stuck_when_last_action_id_is_zero() {
        let mut state = CtrlPollState::default();
        let now_ms = 1_700_000_100_000_i64;

        // First poll returns queueStatus only (lastActionId="0") plus an old end_call and a fresh request.
        let first_resp = json!({
            "error_code": 0,
            "data": {
                "lastActionId": "0",
                "queueStatus": { "4": { "lastId": "9001" } },
                "controls": [
                    { "act_type": "voip", "act": "end_call", "data": { "callId": 1 } },
                    { "act_type": "voip", "act": "request", "data": { "callId": 2, "ts": now_ms - 5_000 } },
                    { "act_type": "voip", "act": "request", "data": { "callId": 3, "ts": now_ms - 60_000 } }
                ]
            }
        });
        let evs1 = apply_lp_control_response(&mut state, &first_resp, now_ms).unwrap();
        assert!(state.initialized);
        assert_eq!(state.queues["4"].last_id, "9001");
        assert_eq!(evs1.len(), 1);
        assert_eq!(evs1[0].act, "request");
        assert_eq!(evs1[0].call_id(), 2);

        // Second poll STILL has last_action_id == "1" because server returned "0",
        // yet `ring_ring` / `answer` MUST NOT be filtered out anymore!
        let second_resp = json!({
            "error_code": 0,
            "data": {
                "controls": [
                    { "act_type": "voip", "act": "ring_ring", "data": { "callId": 42 } },
                    { "act_type": "voip", "act": "answer", "data": { "callId": 42, "status": 0 } }
                ]
            }
        });
        let evs2 = apply_lp_control_response(&mut state, &second_resp, now_ms + 1_000).unwrap();
        assert_eq!(evs2.len(), 2);
        assert_eq!(evs2[0].act, "ring_ring");
        assert_eq!(evs2[1].act, "answer");
    }

    #[test]
    fn lp_control_encode_params_matches_csv_base64_wire_format_and_default_queues() {
        let state = CtrlPollState::with_queues(DEFAULT_LP_CONTROL_QUEUES);
        assert_eq!(state.queues.len(), 7);
        for qid in ["4", "5", "14", "17", "18", "19", "20"] {
            assert_eq!(state.queues[qid].last_id, "1");
            assert!(state.queues[qid].ids.is_empty());
        }

        let encoded = state.encode_params();
        assert!(
            encoded.starts_with("1,1,"),
            "expected CSV prefix `1,1,`, got: {encoded}"
        );
        let tail = encoded.strip_prefix("1,1,").unwrap();
        // Tail is urlencode("{b64},1") -> ends with `%2C1`
        assert!(
            tail.ends_with("%2C1"),
            "expected urlencoded `,1` suffix, got: {tail}"
        );
        let b64_enc = tail.strip_suffix("%2C1").unwrap();
        let b64_raw = b64_enc
            .replace("%2B", "+")
            .replace("%2F", "/")
            .replace("%3D", "=");
        let decoded_bytes = STANDARD.decode(&b64_raw).expect("valid standard base64");
        let parsed: Value = serde_json::from_slice(&decoded_bytes).expect("valid queues JSON");
        assert_eq!(parsed["4"]["lastId"], "1");
        assert_eq!(parsed["4"]["ids"], json!([]));
        assert!(parsed["4"].get("preIds").is_none());
        assert_eq!(parsed["20"]["lastId"], "1");
    }

    #[test]
    fn lp_control_update_handles_empty_keepalive_reset_monotonic_u128_and_ids_cap() {
        let mut state = CtrlPollState::with_queues(&["4", "5"]);

        // 1. Empty 200 keepalive (`data: null`) MUST NOT flip `initialized = true`.
        let empty = json!({ "error_code": 0, "data": null });
        let evs0 = apply_lp_control_response(&mut state, &empty, 1_700_000_000_000).unwrap();
        assert!(evs0.is_empty());
        assert!(!state.initialized);
        assert_eq!(state.last_action_id, "1");

        // 2. Normal monotonic cursor advancement + ids <= max_lid filter, dedup, top-10 ascending.
        state.update_from_response(&json!({
            "lastActionId": "1000",
            "lastActionIdOther": "200",
            "queueStatus": {
                "4": {
                    "lastId": "5012",
                    "ids": [
                        "5099", "5012", "5011", "5011", "5010", "5009", "5008",
                        "5007", "5006", "5005", "5004", "5003", "5002", "5001"
                    ]
                }
            }
        }));
        assert!(state.initialized);
        assert_eq!(state.last_action_id, "1000");
        assert_eq!(state.last_action_id_other, "200");
        assert_eq!(state.queues["4"].last_id, "5012");
        assert_eq!(
            state.queues["4"].ids,
            vec![
                "5003", "5004", "5005", "5006", "5007", "5008", "5009", "5010", "5011", "5012"
            ]
        );

        // 3. Stale/zero cursor values MUST NOT regress monotonic u128 cursors; absent `ids` preserves existing.
        state.update_from_response(&json!({
            "lastActionId": "999",
            "lastActionIdOther": "0",
            "queueStatus": {
                "4": { "lastId": "0" },
                "5": { "lastId": "100", "ids": ["200"] }
            }
        }));
        assert_eq!(state.last_action_id, "1000");
        assert_eq!(state.last_action_id_other, "200");
        assert_eq!(state.queues["4"].last_id, "5012");
        assert_eq!(state.queues["4"].ids.len(), 10);
        // Queue 5 had `ids: ["200"]` > `max_lid=100` -> filtered out -> cleared.
        assert_eq!(state.queues["5"].last_id, "100");
        assert!(state.queues["5"].ids.is_empty());

        // 4. `resetLastActionId` resets lastActionId, resets lastActionIdOther to "1",
        // sets all queue lastIds to reset_id, and clears all queue ids.
        state.update_from_response(&json!({
            "resetLastActionId": "750"
        }));
        assert_eq!(state.last_action_id, "750");
        assert_eq!(state.last_action_id_other, "1");
        assert_eq!(state.queues["4"].last_id, "750");
        assert!(state.queues["4"].ids.is_empty());
        assert_eq!(state.queues["5"].last_id, "750");
        assert!(state.queues["5"].ids.is_empty());
    }

    #[test]
    fn caller_invite_server_addr_uses_call_setting_servers_and_decline_with_media_omits_empty() {
        let resp_401 = json!({
            "id": 464564008,
            "callSetting": {
                "session": "nested_sess",
                "servers": [
                    { "rtp": "42.113.206.4:10490", "rtcp": "42.113.206.4:12490" },
                    { "rtp": "171.244.128.62:4200", "rtcp": "171.244.128.62:4200" }
                ]
            }
        });
        let invite = build_caller_invite_fields(&resp_401, false, Some(50000), None);
        let ext: Value = serde_json::from_str(invite.extend_data.trim()).unwrap();
        let addrs = ext["serverAddr"].as_array().unwrap();
        assert_eq!(addrs.len(), 2, "serverAddr must include all /callSetting/servers entries");
        assert_eq!(addrs[0]["rtp"], "42.113.206.4:10490");
        assert_eq!(addrs[0]["rtcp"], "42.113.206.4:12490");
        assert_eq!(addrs[1]["rtp"], "171.244.128.62:4200");

        let b_dec_full = build_402_body(&AnswerParams::decline_with_media(
            "888",
            42,
            Some("42.113.206.4:10490".into()),
            Some("42.113.206.4:12490".into()),
            Some("sess_xyz".into()),
        ));
        assert_eq!(b_dec_full["status"], 3);
        assert_eq!(b_dec_full["codec"], OPUS_16K_CODEC_JSON);
        assert_eq!(b_dec_full["rtpAddress"], "42.113.206.4:10490");
        assert_eq!(b_dec_full["rtcpAddress"], "42.113.206.4:12490");
        assert_eq!(b_dec_full["session_id"], "sess_xyz");
        assert!(b_dec_full.get("subCommand").is_none());

        let b_dec_empty = build_402_body(&AnswerParams::decline_with_media(
            "888",
            42,
            Some(String::new()),
            None,
            Some("   ".into()),
        ));
        assert_eq!(b_dec_empty["status"], 3);
        assert!(b_dec_empty.get("rtpAddress").is_none());
        assert!(b_dec_empty.get("rtcpAddress").is_none());
        assert!(b_dec_empty.get("session_id").is_none());
    }

    #[test]
    fn lp_control_multipage_backlog_filters_historical_events_and_cutoff_blocks_recent_replays() {
        let now_ms = 1_795_483_000_000_i64;
        // Cutoff set to `now_ms - 2_000` (simulating fresh `call_lab serve` started right after Lần 1 declined).
        let mut state = CtrlPollState::with_queues(&["4", "5", "14"])
            .with_live_cutoff_ms(now_ms - 2_000);

        // Poll #1: `more = 1`, `controls = []`, cursor advances from `"1"` -> `"1808246132792"`.
        let p1 = json!({
            "error_code": 0,
            "data": {
                "lastActionId": "1808246132792",
                "more": 1,
                "queueStatus": { "14": { "lastId": "1808246132792", "ids": [] } },
                "controls": []
            }
        });
        assert!(apply_lp_control_response(&mut state, &p1, now_ms).unwrap().is_empty());
        assert!(state.initialized);
        assert!(state.last_more);
        assert!(!state.is_backlog_synced());

        // Poll #2 (STILL `more = 1`): replays a locally-declined invite from 17s prior (`callId = 191051239`)
        // AND an already-terminated call pair from the same batch. Both MUST be filtered!
        let p2 = json!({
            "error_code": 0,
            "data": {
                "lastActionId": "1808246132792",
                "more": 1,
                "controls": [
                    { "act_type": "voip", "act": "request", "data": { "callId": 191051239, "ts": now_ms - 17_000 } },
                    { "act_type": "voip", "act": "request", "data": { "callId": 777, "ts": now_ms - 500 } },
                    { "act_type": "voip", "act": "end_call", "data": { "callId": 777 } }
                ]
            }
        });
        assert!(apply_lp_control_response(&mut state, &p2, now_ms).unwrap().is_empty());
        assert!(!state.is_backlog_synced());

        // Poll #3: final page (`more = 0`), delivering a genuine new live invite started after startup.
        let p3 = json!({
            "error_code": 0,
            "data": {
                "lastActionId": "1808246132792",
                "more": 0,
                "controls": [
                    { "act_type": "voip", "act": "request", "data": { "callId": 191051999, "ts": now_ms + 500 } }
                ]
            }
        });
        let evs3 = apply_lp_control_response(&mut state, &p3, now_ms + 500).unwrap();
        assert_eq!(evs3.len(), 1);
        assert_eq!(evs3[0].call_id(), 191051999);
        assert!(state.is_backlog_synced());

        // Poll #4 (after backlog sync): ALL live VoIP events (`ring_ring`, `answer`, etc.) pass unfiltered.
        let p4 = json!({
            "error_code": 0,
            "data": {
                "controls": [
                    { "act_type": "voip", "act": "end_call", "data": { "callId": 191051999 } }
                ]
            }
        });
        let evs4 = apply_lp_control_response(&mut state, &p4, now_ms + 3_000).unwrap();
        assert_eq!(evs4.len(), 1);
        assert_eq!(evs4[0].act, "end_call");
    }
}
