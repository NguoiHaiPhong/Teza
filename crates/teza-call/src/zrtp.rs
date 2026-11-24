//! Pure-Rust Zalo UDP ZRTP relay protocol, NAT probe, keepalive, and relay media framing.
//!
//! Packet layouts and constants are verified against `ctdozlite` (`MIT OR Apache-2.0`).

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UdpSocket;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use crate::error::{CallError, CallResult};
use crate::trace::{emit_trace, TraceSink};

/// Fixed 21-byte common ZRTP UDP header length.
pub const ZRTP_HEADER_LEN: usize = 21;

pub const ZRTP_MAGIC_REQ: u8 = 0x01;
pub const ZRTP_MAGIC_RESP: u8 = 0x02;

pub const ZRTP_PT_CONTROL: u8 = 0x01;
pub const ZRTP_PT_INIT: u8 = 0x7E;
pub const ZRTP_PT_INIT_ALT: u8 = 0x7F;

pub const ZRTP_CMD_PING: u16 = 0x0002;
pub const ZRTP_CMD_ECHO: u16 = 0x0005;
pub const ZRTP_CMD_INIT_CALLER: u16 = 0x000B;
pub const ZRTP_CMD_INIT_CALLEE: u16 = 0x000C;
pub const ZRTP_CMD_END: u16 = 0x0020;

pub const ZRTP_SUB_PING: u8 = 0x01;
pub const ZRTP_SUB_ECHO: u8 = 0x00;
pub const ZRTP_SUB_INIT_V1: u8 = 0x01;
pub const ZRTP_SUB_INIT_V3: u8 = 0x02;
pub const ZRTP_SUB_END: u8 = 0x09;

pub const ZRTP_ECHO_MAGIC_FROM_ID: u32 = 0x003E_9765;
pub const ZRTP_ECHO_MAGIC_CALL_ID: u32 = 0x012F_833C;

/// Outbound 5-byte relay wrapper type bytes (`[type_byte][token: u32 LE][inner_rtp_or_rtcp]`).
pub const RELAY_WRAP_AUDIO_RTP: u8 = 0x03;
pub const RELAY_WRAP_AUDIO_RTCP: u8 = 0x05;
pub const RELAY_WRAP_VIDEO_RTP: u8 = 0x0D;
pub const RELAY_WRAP_VIDEO_RTCP: u8 = 0x0F;

const RTCP_INBOX_MAX: usize = 512;

/// Convert a decimal 64-bit Zalo UID string into the lower 32-bit ID used in ZRTP UDP headers.
pub fn uid_str_to_u32(uid: &str) -> u32 {
    uid.trim().parse::<u64>().unwrap_or(0) as u32
}

fn unix_secs_string() -> String {
    chrono::Utc::now().timestamp().max(0).to_string()
}

/// Write the 21-byte common ZRTP UDP request header.
#[allow(clippy::too_many_arguments)]
fn write_common_header(
    buf: &mut Vec<u8>,
    payload_type: u8,
    seq_or_ts: u32,
    ssrc_or_seq: u32,
    from_id: u32,
    call_or_magic: u32,
    cmd: u16,
    sub_cmd: u8,
) {
    buf.push(ZRTP_MAGIC_REQ);
    buf.push(payload_type);
    buf.extend_from_slice(&seq_or_ts.to_le_bytes());
    buf.extend_from_slice(&ssrc_or_seq.to_le_bytes());
    buf.extend_from_slice(&from_id.to_le_bytes());
    buf.extend_from_slice(&call_or_magic.to_le_bytes());
    buf.extend_from_slice(&cmd.to_le_bytes());
    buf.push(sub_cmd);
}

/// Build an `InitZRTP` UDP packet (`0x000B` for caller, `0x000C` for callee).
///
/// Body layout: `[call_id: u32 LE][partner_uid: u32 LE][len: u16 LE][sess_id]`.
pub fn build_zrtp_init_packet(
    is_caller: bool,
    from_uid: u32,
    partner_uid: u32,
    call_id: u32,
    sub_cmd: u8,
    sess_id: &str,
) -> Vec<u8> {
    let cmd = if is_caller {
        ZRTP_CMD_INIT_CALLER
    } else {
        ZRTP_CMD_INIT_CALLEE
    };
    let sess_bytes = sess_id.as_bytes();
    let sess_len = u16::try_from(sess_bytes.len()).unwrap_or(u16::MAX);
    let mut buf = Vec::with_capacity(ZRTP_HEADER_LEN + 10 + sess_len as usize);
    write_common_header(&mut buf, ZRTP_PT_INIT, 0, 0, from_uid, 0, cmd, sub_cmd);
    buf.extend_from_slice(&call_id.to_le_bytes());
    buf.extend_from_slice(&partner_uid.to_le_bytes());
    buf.extend_from_slice(&sess_len.to_le_bytes());
    buf.extend_from_slice(&sess_bytes[..sess_len as usize]);
    buf
}

/// Build a `ZRTPPing` UDP keepalive packet (`cmd = 0x0002, sub_cmd = 1`).
pub fn build_zrtp_ping_packet(from_uid: u32, call_id_or_token: u32, sess_id: &str) -> Vec<u8> {
    let sess_bytes = sess_id.as_bytes();
    let sess_len = u16::try_from(sess_bytes.len()).unwrap_or(u16::MAX);
    let mut buf = Vec::with_capacity(ZRTP_HEADER_LEN + 2 + sess_len as usize);
    write_common_header(
        &mut buf,
        ZRTP_PT_CONTROL,
        0,
        0,
        from_uid,
        call_id_or_token,
        ZRTP_CMD_PING,
        ZRTP_SUB_PING,
    );
    buf.extend_from_slice(&sess_len.to_le_bytes());
    buf.extend_from_slice(&sess_bytes[..sess_len as usize]);
    buf
}

/// Build a `ZRTPEcho` UDP RTT probe packet (`cmd = 0x0005, sub_cmd = 0`).
pub fn build_zrtp_echo_packet(seq: u32, ts_str: &str) -> Vec<u8> {
    let ts_bytes = ts_str.as_bytes();
    let ts_len = u16::try_from(ts_bytes.len()).unwrap_or(u16::MAX);
    let mut buf = Vec::with_capacity(ZRTP_HEADER_LEN + 2 + ts_len as usize);
    write_common_header(
        &mut buf,
        ZRTP_PT_CONTROL,
        0,
        seq,
        ZRTP_ECHO_MAGIC_FROM_ID,
        ZRTP_ECHO_MAGIC_CALL_ID,
        ZRTP_CMD_ECHO,
        ZRTP_SUB_ECHO,
    );
    buf.extend_from_slice(&ts_len.to_le_bytes());
    buf.extend_from_slice(&ts_bytes[..ts_len as usize]);
    buf
}

/// Build a `ZRTPEndCall` UDP teardown packet (`cmd = 0x0020, sub_cmd = 0x09`).
///
/// Unlike `Init`/`Ping`/`Echo`, `End` prefixes `sess_id` with a `u32 LE` length.
pub fn build_zrtp_end_packet(from_uid: u32, call_id: u32, sess_id: &str) -> Vec<u8> {
    let sess_bytes = sess_id.as_bytes();
    let sess_len = u32::try_from(sess_bytes.len()).unwrap_or(u32::MAX);
    let mut buf = Vec::with_capacity(ZRTP_HEADER_LEN + 4 + sess_len as usize);
    write_common_header(
        &mut buf,
        ZRTP_PT_CONTROL,
        0,
        0,
        from_uid,
        call_id,
        ZRTP_CMD_END,
        ZRTP_SUB_END,
    );
    buf.extend_from_slice(&sess_len.to_le_bytes());
    buf.extend_from_slice(&sess_bytes[..sess_len as usize]);
    buf
}

/// Decoded reply to `InitZRTP` (`cmd = 0x000B` or `0x000C`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZrtpInitResponse {
    pub res: i32,
    pub call_id: u32,
    pub token: u32,
    pub public_p2p: Option<(String, u16)>,
}

/// Parse a relay server's UDP response to `InitZRTP`.
pub fn parse_zrtp_init_response(buf: &[u8]) -> Option<ZrtpInitResponse> {
    if buf.len() < 25
        || !matches!(buf[0], ZRTP_MAGIC_REQ | ZRTP_MAGIC_RESP)
        || !matches!(buf[1], ZRTP_PT_INIT | ZRTP_PT_INIT_ALT)
    {
        return None;
    }
    let cmd = u16::from_le_bytes([buf[18], buf[19]]);
    if !matches!(cmd, ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE) {
        return None;
    }
    let res = i32::from_le_bytes([buf[21], buf[22], buf[23], buf[24]]);
    if res != 0 || buf.len() < 33 {
        return Some(ZrtpInitResponse {
            res,
            call_id: 0,
            token: 0,
            public_p2p: None,
        });
    }
    let call_id = u32::from_le_bytes([buf[25], buf[26], buf[27], buf[28]]);
    let token = u32::from_le_bytes([buf[29], buf[30], buf[31], buf[32]]);
    let public_p2p = if buf.len() >= 35 {
        let addr_len = u16::from_le_bytes([buf[33], buf[34]]) as usize;
        if addr_len > 0 && buf.len() >= 35 + addr_len {
            let raw = std::str::from_utf8(&buf[35..35 + addr_len])
                .unwrap_or("")
                .trim();
            raw.rsplit_once('|')
                .or_else(|| raw.rsplit_once(':'))
                .and_then(|(ip, p)| {
                    let clean_ip = ip.trim();
                    if clean_ip.is_empty() {
                        return None;
                    }
                    p.trim()
                        .parse::<u16>()
                        .ok()
                        .filter(|&port| port > 0)
                        .map(|port| (clean_ip.to_string(), port))
                })
        } else {
            None
        }
    } else {
        None
    };
    Some(ZrtpInitResponse {
        res,
        call_id,
        token,
        public_p2p,
    })
}

/// Build a synthetic `InitZRTP` server response packet (useful for mock relays & tests).
pub fn encode_zrtp_init_response(
    cmd: u16,
    res: i32,
    call_id: u32,
    token: u32,
    public_p2p: Option<(&str, u16)>,
) -> Vec<u8> {
    let mut pkt = vec![0u8; ZRTP_HEADER_LEN];
    pkt[0] = ZRTP_MAGIC_RESP;
    pkt[1] = ZRTP_PT_INIT;
    pkt[18..20].copy_from_slice(&cmd.to_le_bytes());
    pkt[20] = ZRTP_SUB_INIT_V3;
    pkt.extend_from_slice(&res.to_le_bytes());
    if res == 0 {
        pkt.extend_from_slice(&call_id.to_le_bytes());
        pkt.extend_from_slice(&token.to_le_bytes());
        if let Some((ip, port)) = public_p2p {
            let addr = format!("{ip}|{port}");
            let bytes = addr.as_bytes();
            let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
            pkt.extend_from_slice(&len.to_le_bytes());
            pkt.extend_from_slice(&bytes[..len as usize]);
        }
    }
    pkt
}

/// Parse an Echo reply packet (`cmd = 0x0005`), returning its sequence number.
pub fn parse_zrtp_echo_response(buf: &[u8]) -> Option<u32> {
    if buf.len() < ZRTP_HEADER_LEN
        || !matches!(buf[0], ZRTP_MAGIC_REQ | ZRTP_MAGIC_RESP)
        || buf[1] != ZRTP_PT_CONTROL
    {
        return None;
    }
    let cmd = u16::from_le_bytes([buf[18], buf[19]]);
    if cmd != ZRTP_CMD_ECHO {
        return None;
    }
    Some(u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]))
}

/// Wrap an inner RTP or RTCP packet in Zalo's 5-byte relay media header
/// (`[type_byte][token: u32 LE][inner]`).
pub fn wrap_relay_media(type_byte: u8, token: u32, inner: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(5 + inner.len());
    wire.push(type_byte);
    wire.extend_from_slice(&token.to_le_bytes());
    wire.extend_from_slice(inner);
    wire
}

/// Wrap an audio RTP packet for a Zalo UDP relay (`0x03 + token LE + rtp`).
pub fn wrap_relay_audio_rtp(token: u32, rtp: &[u8]) -> Vec<u8> {
    wrap_relay_media(RELAY_WRAP_AUDIO_RTP, token, rtp)
}

/// Wrap an audio RTCP packet for a Zalo UDP relay (`0x05 + token LE + rtcp`).
pub fn wrap_relay_audio_rtcp(token: u32, rtcp: &[u8]) -> Vec<u8> {
    wrap_relay_media(RELAY_WRAP_AUDIO_RTCP, token, rtcp)
}

/// Unwrap a Zalo UDP relay media packet into its inner RFC 3550 RTP/RTCP v2 slice.
///
/// Supports:
/// - 1-byte header (`0x04` incoming audio, `0x07` P2P RTP, `0x0E` incoming video)
/// - 5-byte header (`0x03`/`0x05`/`0x0D`/`0x0F` + `token: u32 LE`)
/// - 9-byte `0x7F` header (`cmd in 6..=10`)
/// - 25-byte `0x02` header (`cmd == 0x0032`)
/// - Raw RTP/RTCP v2 (`(byte0 & 0xC0) == 0x80`)
pub fn unwrap_relay_media(pkt: &[u8]) -> Option<&[u8]> {
    if pkt.len() < 8 {
        return None;
    }
    let inner = match pkt[0] {
        0x04 | 0x07 | 0x0E => pkt.get(1..)?,
        0x03 | 0x05 | 0x0D | 0x0F => pkt.get(5..)?,
        0x7F if pkt.len() > 9 => {
            let cmd = u16::from_le_bytes([pkt[7], pkt[8]]);
            if (6..=10).contains(&cmd) {
                pkt.get(9..)?
            } else {
                return None;
            }
        }
        0x02 if pkt.len() > 25 && u16::from_le_bytes([pkt[18], pkt[19]]) == 0x0032 => {
            pkt.get(25..)?
        }
        b if (b & 0xC0) == 0x80 => pkt,
        _ => return None,
    };
    if inner.len() < 8 || (inner[0] & 0xC0) != 0x80 {
        return None;
    }
    let is_rtcp = (200..=207).contains(&inner[1]);
    if is_rtcp || inner.len() >= 12 {
        Some(inner)
    } else {
        None
    }
}

/// A candidate `(rtp, rtcp)` UDP relay server endpoint pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayServer {
    pub rtp: String,
    pub rtcp: String,
}

impl RelayServer {
    pub fn new(rtp: impl Into<String>, rtcp: impl Into<String>) -> Self {
        let rtp = canonical_addr_key(&rtp.into());
        let rtcp_raw = rtcp.into();
        let rtcp_clean = canonical_addr_key(&rtcp_raw);
        let rtcp = if rtcp_clean.is_empty() {
            rtp.clone()
        } else {
            rtcp_clean
        };
        Self { rtp, rtcp }
    }

    /// True when the relay exposes a separate RTCP endpoint distinct from its RTP endpoint.
    pub fn has_distinct_rtcp(&self) -> bool {
        !self.rtcp.is_empty() && self.rtcp != self.rtp
    }
}

fn canonical_addr_key(addr: &str) -> String {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .parse::<SocketAddr>()
        .map(|sa| sa.to_string())
        .unwrap_or_else(|_| trimmed.to_string())
}

/// Collect candidate `(rtp, rtcp)` relay server pairs from a `401` response or incoming invite payload.
pub fn collect_relay_servers(payload: &Value) -> Vec<RelayServer> {
    let mut out: Vec<RelayServer> = Vec::new();
    let mut push_unique = |rtp: &str, rtcp: &str| {
        let rtp = rtp.trim();
        if rtp.is_empty() {
            return;
        }
        if !out.iter().any(|s| s.rtp == rtp) {
            out.push(RelayServer::new(rtp, rtcp));
        }
    };

    if let Some(arr) = payload
        .get("servers")
        .or_else(|| payload.pointer("/callSetting/servers"))
        .and_then(Value::as_array)
    {
        for srv in arr {
            let rtp = srv
                .get("rtpaddr")
                .or_else(|| srv.get("rtp"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let rtcp = srv
                .get("rtcpaddr")
                .or_else(|| srv.get("rtcp"))
                .and_then(Value::as_str)
                .unwrap_or(rtp);
            push_unique(rtp, rtcp);
        }
    }

    if let Some(ext_str) = payload.get("extendData").and_then(Value::as_str) {
        if let Ok(ext_json) = serde_json::from_str::<Value>(ext_str.trim()) {
            if let Some(arr) = ext_json.get("serverAddr").and_then(Value::as_array) {
                for srv in arr {
                    let rtp = srv.get("rtp").and_then(Value::as_str).unwrap_or("");
                    let rtcp = srv.get("rtcp").and_then(Value::as_str).unwrap_or(rtp);
                    push_unique(rtp, rtcp);
                }
            }
        }
    }

    let top_rtp = payload
        .get("rtpIP")
        .or_else(|| payload.get("rtpAddress"))
        .or_else(|| payload.get("rtpSerIp"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let top_rtcp = payload
        .get("rtcpIP")
        .or_else(|| payload.get("rtcpAddress"))
        .and_then(Value::as_str)
        .unwrap_or(top_rtp);
    push_unique(top_rtp, top_rtcp);

    out
}

/// Results gathered during the UDP Echo (`0x0005`) + `InitZRTP` (`0x000B`/`0x000C`) handshake.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZrtpProbeResult {
    pub local_port: u16,
    pub local_rtcp_port: u16,
    /// Public `(ip, port)` observed by the relay server in its `InitZRTP` reply.
    pub public_p2p: Option<(String, u16)>,
    /// Per-relay `(recv_count, avg_rtt_ms)` keyed by RTP `"ip:port"`.
    pub echo_stats: BTreeMap<String, (i64, i64)>,
    /// Relay RTP `"ip:port"` that first answered `InitZRTP` with `res == 0`.
    pub selected_rtp: Option<String>,
    /// Token returned by the relay server in `InitZRTP`.
    pub token: u32,
    /// Per-relay token map (`"ip:port" -> token`) so the caller can switch to `rtpSerIp` on `answer`.
    pub server_tokens: BTreeMap<String, u32>,
    /// Whether the RTCP socket also received an `InitZRTP` ack (distinct RTCP relay port).
    pub rtcp_acked: bool,
}

/// Tuning parameters for UDP ZRTP probing and keepalive.
#[derive(Debug, Clone)]
pub struct ZrtpProbeConfig {
    pub echo_bursts: usize,
    pub echoes_per_burst: usize,
    pub burst_gap: Duration,
    pub probe_timeout: Duration,
    pub keepalive_interval: Duration,
    pub sub_cmd: u8,
    pub trace: Option<TraceSink>,
}

impl Default for ZrtpProbeConfig {
    fn default() -> Self {
        Self {
            echo_bursts: 2,
            echoes_per_burst: 7,
            burst_gap: Duration::from_millis(20),
            probe_timeout: Duration::from_millis(280),
            keepalive_interval: Duration::from_millis(400),
            sub_cmd: ZRTP_SUB_INIT_V3,
            trace: None,
        }
    }
}

/// Active UDP ZRTP relay session holding bound RTP/RTCP sockets, keepalive loop, and RTCP inbox.
pub struct ZrtpRelaySession {
    rtp_sock: Arc<UdpSocket>,
    rtcp_sock: Arc<UdpSocket>,
    std_rtp_sock: Arc<std::net::UdpSocket>,
    std_rtcp_sock: Arc<std::net::UdpSocket>,
    probe: ZrtpProbeResult,
    server_token_handles: Arc<Mutex<BTreeMap<String, Arc<AtomicU32>>>>,
    media_attached: Arc<AtomicBool>,
    servers: Arc<Mutex<Vec<RelayServer>>>,
    servers_changed: Arc<Notify>,
    from_uid: u32,
    partner_uid: u32,
    call_id: u32,
    is_caller: bool,
    sub_cmd: u8,
    sess_id: String,
    trace: Option<TraceSink>,
    rtcp_inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    abort_tx: Option<watch::Sender<bool>>,
    keepalive_task: Option<JoinHandle<()>>,
}

impl ZrtpRelaySession {
    /// Start a UDP ZRTP relay session from candidate servers extracted from a `401` / invite payload.
    pub async fn start_from_payload(
        payload: &Value,
        from_uid: u32,
        partner_uid: u32,
        call_id: u32,
        is_caller: bool,
        sess_id: &str,
    ) -> CallResult<Self> {
        Self::start_from_payload_with_trace(
            payload,
            from_uid,
            partner_uid,
            call_id,
            is_caller,
            sess_id,
            None,
        )
        .await
    }

    /// Start a UDP ZRTP relay session from a `401`/invite payload with optional sanitized trace logging.
    pub async fn start_from_payload_with_trace(
        payload: &Value,
        from_uid: u32,
        partner_uid: u32,
        call_id: u32,
        is_caller: bool,
        sess_id: &str,
        trace: Option<TraceSink>,
    ) -> CallResult<Self> {
        let servers = collect_relay_servers(payload);
        Self::start_with_config(
            servers,
            from_uid,
            partner_uid,
            call_id,
            is_caller,
            sess_id,
            ZrtpProbeConfig {
                trace,
                ..Default::default()
            },
        )
        .await
    }

    /// Bind RTP & RTCP UDP sockets, probe candidate relays, and spawn the background keepalive task.
    pub async fn start_with_config(
        servers: Vec<RelayServer>,
        from_uid: u32,
        partner_uid: u32,
        call_id: u32,
        is_caller: bool,
        sess_id: &str,
        cfg: ZrtpProbeConfig,
    ) -> CallResult<Self> {
        let raw_rtp = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| CallError::Network(format!("Không bind được UDP RTP socket: {e}")))?;
        raw_rtp
            .set_nonblocking(true)
            .map_err(|e| CallError::Network(format!("Không đặt nonblocking RTP socket: {e}")))?;
        let std_rtp_sock = Arc::new(
            raw_rtp
                .try_clone()
                .map_err(|e| CallError::Network(format!("Không nhân bản được RTP socket: {e}")))?,
        );
        let rtp_sock = Arc::new(
            UdpSocket::from_std(raw_rtp)
                .map_err(|e| CallError::Network(format!("Không đăng ký Tokio RTP socket: {e}")))?,
        );

        let (rtcp_sock, std_rtcp_sock) = match std::net::UdpSocket::bind("0.0.0.0:0") {
            Ok(raw_rtcp) => {
                if raw_rtcp.set_nonblocking(true).is_ok() {
                    match raw_rtcp.try_clone().and_then(|cl| {
                        UdpSocket::from_std(raw_rtcp).map(|ts| (Arc::new(ts), Arc::new(cl)))
                    }) {
                        Ok(pair) => pair,
                        Err(_) => (rtp_sock.clone(), std_rtp_sock.clone()),
                    }
                } else {
                    (rtp_sock.clone(), std_rtp_sock.clone())
                }
            }
            Err(_) => (rtp_sock.clone(), std_rtp_sock.clone()),
        };
        let local_port = rtp_sock.local_addr().map(|a| a.port()).unwrap_or(60020);
        let local_rtcp_port = rtcp_sock.local_addr().map(|a| a.port()).unwrap_or(local_port);
        let trace = cfg.trace.clone();

        emit_trace(trace.as_ref(), || {
            let srvs_desc: Vec<String> = servers
                .iter()
                .map(|s| {
                    if s.has_distinct_rtcp() {
                        format!("{}(rtcp={})", s.rtp, s.rtcp)
                    } else {
                        s.rtp.clone()
                    }
                })
                .collect();
            format!(
                "[ZRTP START] role={} call_id={} from_u32={} partner_u32={} local_rtp={} local_rtcp={} sub_cmd={} servers=[{}]",
                if is_caller { "caller" } else { "callee" },
                call_id,
                from_uid,
                partner_uid,
                local_port,
                local_rtcp_port,
                cfg.sub_cmd,
                srvs_desc.join(", ")
            )
        });

        let mut probe = ZrtpProbeResult {
            local_port,
            local_rtcp_port,
            ..Default::default()
        };

        let pkt_init = build_zrtp_init_packet(
            is_caller,
            from_uid,
            partner_uid,
            call_id,
            cfg.sub_cmd,
            sess_id,
        );
        let ts_now = unix_secs_string();

        let t_start = Instant::now();
        let mut echo_sent_at: BTreeMap<u32, Instant> = BTreeMap::new();
        let mut echo_seq: u32 = 1;
        for burst_idx in 0..cfg.echo_bursts.max(1) {
            for srv in &servers {
                let _ = rtp_sock.send_to(&pkt_init, srv.rtp.as_str()).await;
                // Only send InitZRTP from `rtcp_sock` when the relay has a distinct RTCP port so
                // a single-port relay never overwrites the `rtp_sock` NAT mapping.
                if srv.has_distinct_rtcp() {
                    let _ = rtcp_sock.send_to(&pkt_init, srv.rtcp.as_str()).await;
                }
                for _ in 0..cfg.echoes_per_burst {
                    let seq_this = echo_seq;
                    let pkt_echo = build_zrtp_echo_packet(seq_this, &ts_now);
                    echo_seq = echo_seq.wrapping_add(1);
                    echo_sent_at.insert(seq_this, Instant::now());
                    let _ = rtp_sock.send_to(&pkt_echo, srv.rtp.as_str()).await;
                }
            }
            if burst_idx + 1 < cfg.echo_bursts && !cfg.burst_gap.is_zero() {
                tokio::time::sleep(cfg.burst_gap).await;
            }
        }

        let rtcp_inbox: Arc<Mutex<VecDeque<Vec<u8>>>> = Arc::new(Mutex::new(VecDeque::new()));
        let mut recv_buf = [0u8; 1500];
        let mut rtcp_buf = [0u8; 1500];
        let deadline = tokio::time::Instant::now() + cfg.probe_timeout;
        let mut server_tokens: BTreeMap<String, u32> = BTreeMap::new();
        let distinct_rtcp_exists = !Arc::ptr_eq(&rtp_sock, &rtcp_sock);

        if !servers.is_empty() {
            let has_unresolved_rtp = servers
                .iter()
                .any(|s| !s.rtp.is_empty() && s.rtp.parse::<SocketAddr>().is_err());
            let has_unresolved_rtcp = servers
                .iter()
                .any(|s| s.has_distinct_rtcp() && s.rtcp.parse::<SocketAddr>().is_err());
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    res = rtp_sock.recv_from(&mut recv_buf) => {
                        let Ok((n, peer)) = res else { continue; };
                        let peer_str = peer.to_string();
                        let is_known_rtp =
                            has_unresolved_rtp || servers.iter().any(|s| s.rtp == peer_str);
                        if !is_known_rtp {
                            continue;
                        }
                        if let Some(seq_reply) = parse_zrtp_echo_response(&recv_buf[..n]) {
                            let rtt_ms = echo_sent_at
                                .remove(&seq_reply)
                                .map(|t0| t0.elapsed().as_millis() as i64)
                                .unwrap_or_else(|| t_start.elapsed().as_millis() as i64)
                                .max(1);
                            let entry = probe.echo_stats.entry(peer_str).or_insert((0, rtt_ms));
                            entry.0 += 1;
                            entry.1 = (entry.1 + rtt_ms) / 2;
                        } else if let Some(init_resp) = parse_zrtp_init_response(&recv_buf[..n]) {
                            emit_trace(trace.as_ref(), || {
                                format!(
                                    "[ZRTP INIT RX] peer={peer_str} res={} call_id={} token=0x{:08x} public_p2p={:?}",
                                    init_resp.res, init_resp.call_id, init_resp.token, init_resp.public_p2p
                                )
                            });
                            if init_resp.res == 0 {
                                server_tokens.insert(peer_str.clone(), init_resp.token);
                                if probe.selected_rtp.is_none() {
                                    probe.selected_rtp = Some(peer_str.clone());
                                    probe.token = init_resp.token;
                                }
                                if probe.public_p2p.is_none() && init_resp.public_p2p.is_some() {
                                    probe.public_p2p = init_resp.public_p2p;
                                }
                                let pkt_ping =
                                    build_zrtp_ping_packet(from_uid, init_resp.token, sess_id);
                                let _ = rtp_sock.send_to(&pkt_ping, peer_str.as_str()).await;
                            }
                        }
                    }
                    res = rtcp_sock.recv_from(&mut rtcp_buf), if distinct_rtcp_exists => {
                        let Ok((n, peer)) = res else { continue; };
                        let peer_str = peer.to_string();
                        if let Some(init_resp) = parse_zrtp_init_response(&rtcp_buf[..n]) {
                            let is_known_rtcp = has_unresolved_rtcp
                                || servers.iter().any(|s| s.has_distinct_rtcp() && s.rtcp == peer_str);
                            emit_trace(trace.as_ref(), || {
                                format!(
                                    "[ZRTP RTCP INIT RX] peer={peer_str} known={is_known_rtcp} res={} token=0x{:08x}",
                                    init_resp.res, init_resp.token
                                )
                            });
                            if init_resp.res == 0 && is_known_rtcp {
                                probe.rtcp_acked = true;
                                server_tokens.entry(peer_str.clone()).or_insert(init_resp.token);
                                let pkt_ping =
                                    build_zrtp_ping_packet(from_uid, init_resp.token, sess_id);
                                let _ = rtcp_sock.send_to(&pkt_ping, peer_str.as_str()).await;
                            }
                        } else {
                            push_rtcp_inbox(&rtcp_inbox, &rtcp_buf[..n]);
                        }
                    }
                }
            }
        }
        probe.server_tokens = server_tokens.clone();
        emit_trace(trace.as_ref(), || {
            let echo_desc: Vec<String> = probe
                .echo_stats
                .iter()
                .map(|(srv, (recv, rtt))| format!("{srv}:recv={recv}/rtt={rtt}ms"))
                .collect();
            let tok_desc: Vec<String> = probe
                .server_tokens
                .iter()
                .map(|(srv, tok)| format!("{srv}=0x{tok:08x}"))
                .collect();
            format!(
                "[ZRTP PROBE DONE] selected={:?} token=0x{:08x} rtcp_acked={} public_p2p={:?} echo=[{}] tokens=[{}]",
                probe.selected_rtp,
                probe.token,
                probe.rtcp_acked,
                probe.public_p2p,
                echo_desc.join(", "),
                tok_desc.join(", ")
            )
        });
        let mut handle_map: BTreeMap<String, Arc<AtomicU32>> = BTreeMap::new();
        for (addr, tok) in &server_tokens {
            handle_map.insert(addr.clone(), Arc::new(AtomicU32::new(*tok)));
        }
        for srv in &servers {
            handle_map
                .entry(srv.rtp.clone())
                .or_insert_with(|| Arc::new(AtomicU32::new(0)));
            if srv.has_distinct_rtcp() {
                handle_map
                    .entry(srv.rtcp.clone())
                    .or_insert_with(|| Arc::new(AtomicU32::new(0)));
            }
        }
        let server_token_handles = Arc::new(Mutex::new(handle_map));
        let media_attached = Arc::new(AtomicBool::new(false));
        let shared_servers = Arc::new(Mutex::new(servers));
        let servers_changed = Arc::new(Notify::new());

        let (abort_tx, mut abort_rx) = watch::channel(false);
        let rtp_loop = rtp_sock.clone();
        let rtcp_loop = rtcp_sock.clone();
        let inbox_loop = rtcp_inbox.clone();
        let token_handles_loop = server_token_handles.clone();
        let media_attached_loop = media_attached.clone();
        let servers_loop = shared_servers.clone();
        let servers_changed_loop = servers_changed.clone();
        let sess_owned = sess_id.to_string();
        let sub_cmd = cfg.sub_cmd;
        let trace_loop = trace.clone();
        let keepalive_interval = cfg.keepalive_interval.max(Duration::from_millis(50));

        let keepalive_task = tokio::spawn(async move {
            let mut seq: u32 = echo_seq;
            let mut interval = tokio::time::interval(keepalive_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            let mut buf_rtp = [0u8; 1500];
            let mut buf_rtcp = [0u8; 1500];
            loop {
                let any_missing_rtp_token = {
                    let srvs = servers_loop
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_default();
                    srvs.iter()
                        .any(|srv| load_server_token(&token_handles_loop, &srv.rtp) == 0)
                };
                let allow_rtp_recv =
                    any_missing_rtp_token && !media_attached_loop.load(Ordering::Acquire);
                tokio::select! {
                    changed = abort_rx.changed() => {
                        if changed.is_err() || *abort_rx.borrow() {
                            let pkt_end = build_zrtp_end_packet(from_uid, call_id, &sess_owned);
                            let srvs = servers_loop
                                .lock()
                                .map(|g| g.clone())
                                .unwrap_or_default();
                            emit_trace(trace_loop.as_ref(), || {
                                format!("[ZRTP END] call_id={call_id} sending End to {} relay(s)", srvs.len())
                            });
                            for srv in &srvs {
                                let _ = rtp_loop.send_to(&pkt_end, srv.rtp.as_str()).await;
                                if distinct_rtcp_exists && srv.has_distinct_rtcp() {
                                    let _ = rtcp_loop.send_to(&pkt_end, srv.rtcp.as_str()).await;
                                }
                            }
                            break;
                        }
                    }
                    _ = servers_changed_loop.notified() => {
                        // Wake up immediately when a new relay target is registered or when
                        // `media_attached` flips so `allow_rtp_recv` updates without waiting for a tick.
                    }
                    _ = interval.tick() => {
                        let srvs = servers_loop
                            .lock()
                            .map(|g| g.clone())
                            .unwrap_or_default();
                        let pkt_e = build_zrtp_echo_packet(seq, &unix_secs_string());
                        seq = seq.wrapping_add(1);
                        let mut pkt_retry_cache: Option<Vec<u8>> = None;
                        for srv in &srvs {
                            let rtp_tok = load_server_token(&token_handles_loop, &srv.rtp);
                            if rtp_tok == 0 {
                                let pkt_retry = pkt_retry_cache.get_or_insert_with(|| {
                                    build_zrtp_init_packet(
                                        is_caller,
                                        from_uid,
                                        partner_uid,
                                        call_id,
                                        sub_cmd,
                                        &sess_owned,
                                    )
                                });
                                let _ = rtp_loop.send_to(pkt_retry, srv.rtp.as_str()).await;
                                let _ = rtp_loop.send_to(&pkt_e, srv.rtp.as_str()).await;
                            } else {
                                let pkt_p = build_zrtp_ping_packet(from_uid, rtp_tok, &sess_owned);
                                let _ = rtp_loop.send_to(&pkt_p, srv.rtp.as_str()).await;
                            }

                            if distinct_rtcp_exists && srv.has_distinct_rtcp() {
                                let rtcp_tok = load_server_token(&token_handles_loop, &srv.rtcp);
                                if rtcp_tok == 0 {
                                    let pkt_retry = pkt_retry_cache.get_or_insert_with(|| {
                                        build_zrtp_init_packet(
                                            is_caller,
                                            from_uid,
                                            partner_uid,
                                            call_id,
                                            sub_cmd,
                                            &sess_owned,
                                        )
                                    });
                                    let _ = rtcp_loop.send_to(pkt_retry, srv.rtcp.as_str()).await;
                                } else {
                                    let pkt_rp =
                                        build_zrtp_ping_packet(from_uid, rtcp_tok, &sess_owned);
                                    let _ = rtcp_loop.send_to(&pkt_rp, srv.rtcp.as_str()).await;
                                }
                            }
                        }
                    }
                    res = rtp_loop.recv_from(&mut buf_rtp), if allow_rtp_recv => {
                        if let Ok((n, peer)) = res {
                            if let Some(init_resp) = parse_zrtp_init_response(&buf_rtp[..n]) {
                                if init_resp.res == 0 && init_resp.token != 0 {
                                    emit_trace(trace_loop.as_ref(), || {
                                        format!(
                                            "[ZRTP LATE INIT] peer={peer} token=0x{:08x}",
                                            init_resp.token
                                        )
                                    });
                                    store_registered_server_token(
                                        &token_handles_loop,
                                        &peer.to_string(),
                                        init_resp.token,
                                    );
                                }
                            }
                        }
                    }
                    res = rtcp_loop.recv_from(&mut buf_rtcp), if distinct_rtcp_exists => {
                        if let Ok((n, peer)) = res {
                            if let Some(init_resp) = parse_zrtp_init_response(&buf_rtcp[..n]) {
                                if init_resp.res == 0 && init_resp.token != 0 {
                                    emit_trace(trace_loop.as_ref(), || {
                                        format!(
                                            "[ZRTP LATE RTCP INIT] peer={peer} token=0x{:08x}",
                                            init_resp.token
                                        )
                                    });
                                    store_registered_server_token(
                                        &token_handles_loop,
                                        &peer.to_string(),
                                        init_resp.token,
                                    );
                                }
                            } else {
                                push_rtcp_inbox(&inbox_loop, &buf_rtcp[..n]);
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            rtp_sock,
            rtcp_sock,
            std_rtp_sock,
            std_rtcp_sock,
            probe,
            server_token_handles,
            media_attached,
            servers: shared_servers,
            servers_changed,
            from_uid,
            partner_uid,
            call_id,
            is_caller,
            sub_cmd,
            sess_id: sess_id.to_string(),
            trace,
            rtcp_inbox,
            abort_tx: Some(abort_tx),
            keepalive_task: Some(keepalive_task),
        })
    }

    pub fn rtp_socket(&self) -> Arc<UdpSocket> {
        self.rtp_sock.clone()
    }

    pub fn rtcp_socket(&self) -> Arc<UdpSocket> {
        self.rtcp_sock.clone()
    }

    /// Non-blocking OS UDP RTP socket clone sharing the exact bound port for real-time audio threads.
    pub fn std_rtp_socket(&self) -> Arc<std::net::UdpSocket> {
        self.std_rtp_sock.clone()
    }

    /// Non-blocking OS UDP RTCP socket clone sharing the exact bound port for real-time audio threads.
    pub fn std_rtcp_socket(&self) -> Arc<std::net::UdpSocket> {
        self.std_rtcp_sock.clone()
    }

    pub fn probe(&self) -> &ZrtpProbeResult {
        &self.probe
    }

    pub fn rtcp_inbox(&self) -> Arc<Mutex<VecDeque<Vec<u8>>>> {
        self.rtcp_inbox.clone()
    }

    /// Per-server live relay token handle for `rtp_addr` (`"ip:port"`).
    ///
    /// Seeded ONLY from `rtp_addr`'s own `InitZRTP` response (initially `0` if unacknowledged) and
    /// never populated from another relay server's token. Updates written by `AudioMediaSession`
    /// upon a late `InitZRTP` ack from `rtp_addr` are immediately observed by `keepalive_task`.
    pub fn token_handle_for(&self, rtp_addr: &str) -> Arc<AtomicU32> {
        get_or_create_token_handle(&self.server_token_handles, rtp_addr)
    }

    /// Shared flag set by `AudioMediaSession` while it owns reading from the RTP socket so the
    /// keepalive task never competes for incoming RTP datagrams.
    pub fn media_attached_flag(&self) -> Arc<AtomicBool> {
        self.media_attached.clone()
    }

    /// Notifier woken by `AudioMediaSession` whenever `media_attached` changes so `keepalive_task`
    /// immediately disarms/rearms its RTP socket receiver without waiting for the next timer tick.
    pub fn media_attached_notify(&self) -> Arc<Notify> {
        self.servers_changed.clone()
    }

    /// Register an additional relay server (e.g. callee-selected `rtpSerIp` not in the initial
    /// `401` list) so keepalive pings and `InitZRTP` retries cover it too.
    pub async fn ensure_target_registered(&self, server: RelayServer) {
        if server.rtp.is_empty() {
            return;
        }
        let _ = self.token_handle_for(&server.rtp);
        if server.has_distinct_rtcp() {
            let _ = self.token_handle_for(&server.rtcp);
        }
        let added = if let Ok(mut srvs) = self.servers.lock() {
            if let Some(existing) = srvs.iter_mut().find(|s| s.rtp == server.rtp) {
                if !existing.has_distinct_rtcp() && server.has_distinct_rtcp() {
                    existing.rtcp = server.rtcp.clone();
                    true
                } else {
                    false
                }
            } else {
                srvs.push(server.clone());
                true
            }
        } else {
            false
        };
        let need_rtp_init = self.token_for(&server.rtp) == 0;
        let need_rtcp_init = !Arc::ptr_eq(&self.rtp_sock, &self.rtcp_sock)
            && server.has_distinct_rtcp()
            && self.token_for(&server.rtcp) == 0;
        if added || need_rtp_init || need_rtcp_init {
            emit_trace(self.trace.as_ref(), || {
                format!(
                    "[ZRTP TARGET] rtp={} rtcp={} added={added} need_rtp_init={need_rtp_init} need_rtcp_init={need_rtcp_init}",
                    server.rtp, server.rtcp
                )
            });
            self.servers_changed.notify_one();
            let pkt_init = build_zrtp_init_packet(
                self.is_caller,
                self.from_uid,
                self.partner_uid,
                self.call_id,
                self.sub_cmd,
                &self.sess_id,
            );
            if added || need_rtp_init {
                let _ = self.rtp_sock.send_to(&pkt_init, server.rtp.as_str()).await;
            }
            if need_rtcp_init {
                let _ = self
                    .rtcp_sock
                    .send_to(&pkt_init, server.rtcp.as_str())
                    .await;
            }
        }
    }

    /// Look up the latest relay token for a specific RTP `"ip:port"` (including late acks).
    ///
    /// Returns `0` when `rtp_addr` has not yet acknowledged `InitZRTP` — never falls back to a
    /// different relay server's token.
    pub fn token_for(&self, rtp_addr: &str) -> u32 {
        load_server_token(&self.server_token_handles, rtp_addr)
    }

    /// Drain all non-ZRTP RTCP datagrams received on the RTCP socket.
    pub fn drain_rtcp_inbox(&self) -> Vec<Vec<u8>> {
        self.rtcp_inbox
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// Gracefully stop the keepalive loop and wait until `ZRTPEndCall` packets have been sent.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.abort_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(handle) = self.keepalive_task.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for ZrtpRelaySession {
    fn drop(&mut self) {
        if let Some(tx) = self.abort_tx.take() {
            let _ = tx.send(true);
        }
    }
}

fn get_or_create_token_handle(
    handles: &Arc<Mutex<BTreeMap<String, Arc<AtomicU32>>>>,
    addr: &str,
) -> Arc<AtomicU32> {
    let key = canonical_addr_key(addr);
    if key.is_empty() {
        return Arc::new(AtomicU32::new(0));
    }
    if let Ok(mut map) = handles.lock() {
        map.entry(key)
            .or_insert_with(|| Arc::new(AtomicU32::new(0)))
            .clone()
    } else {
        Arc::new(AtomicU32::new(0))
    }
}

fn store_registered_server_token(
    handles: &Arc<Mutex<BTreeMap<String, Arc<AtomicU32>>>>,
    addr: &str,
    token: u32,
) {
    if token == 0 {
        return;
    }
    let key = canonical_addr_key(addr);
    if key.is_empty() {
        return;
    }
    if let Ok(map) = handles.lock() {
        if let Some(slot) = map.get(&key) {
            slot.store(token, Ordering::Relaxed);
        }
    }
}

fn load_server_token(
    handles: &Arc<Mutex<BTreeMap<String, Arc<AtomicU32>>>>,
    addr: &str,
) -> u32 {
    let key = canonical_addr_key(addr);
    if key.is_empty() {
        return 0;
    }
    if let Ok(map) = handles.lock() {
        map.get(&key)
            .map(|a| a.load(Ordering::Relaxed))
            .unwrap_or(0)
    } else {
        0
    }
}

fn push_rtcp_inbox(inbox: &Arc<Mutex<VecDeque<Vec<u8>>>>, pkt: &[u8]) {
    if let Ok(mut q) = inbox.lock() {
        if q.len() >= RTCP_INBOX_MAX {
            q.pop_front();
        }
        q.push_back(pkt.to_vec());
    }
}

/// Summary of a standalone UDP Echo (`0x0005`) probe against a relay endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoProbeSummary {
    pub target: String,
    pub sent: usize,
    pub received: usize,
    pub min_rtt_ms: Option<u64>,
    pub avg_rtt_ms: Option<u64>,
    pub max_rtt_ms: Option<u64>,
}

/// Send `count` UDP ZRTP Echo (`0x0005`) probes to `target` and measure RTT / packet loss.
pub async fn probe_udp_echo(
    target: SocketAddr,
    count: usize,
    timeout: Duration,
) -> CallResult<EchoProbeSummary> {
    if count == 0 {
        return Err(CallError::InvalidArgument(
            "count phải lớn hơn 0".to_string(),
        ));
    }
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| CallError::Network(format!("Không bind được UDP socket: {e}")))?;

    let mut sent_at: BTreeMap<u32, Instant> = BTreeMap::new();
    let ts_now = unix_secs_string();
    for i in 1..=(count as u32) {
        let pkt = build_zrtp_echo_packet(i, &ts_now);
        sent_at.insert(i, Instant::now());
        sock.send_to(&pkt, target)
            .await
            .map_err(|e| CallError::Network(format!("Gửi UDP Echo thất bại: {e}")))?;
        if i < count as u32 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let mut rtts: Vec<u64> = Vec::with_capacity(count);
    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + timeout;
    while rtts.len() < count {
        let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remain.is_zero() {
            break;
        }
        match tokio::time::timeout(remain, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, _peer))) => {
                if let Some(seq) = parse_zrtp_echo_response(&buf[..n]) {
                    if let Some(t0) = sent_at.remove(&seq) {
                        rtts.push(t0.elapsed().as_millis().max(1) as u64);
                    }
                }
            }
            Ok(Err(_)) => continue,
            Err(_) => break,
        }
    }

    let min_rtt_ms = rtts.iter().copied().min();
    let max_rtt_ms = rtts.iter().copied().max();
    let avg_rtt_ms = if rtts.is_empty() {
        None
    } else {
        Some(rtts.iter().sum::<u64>() / rtts.len() as u64)
    };

    Ok(EchoProbeSummary {
        target: target.to_string(),
        sent: count,
        received: rtts.len(),
        min_rtt_ms,
        avg_rtt_ms,
        max_rtt_ms,
    })
}

/// Detect local IPv4 address and allocate an ephemeral port (or reuse `override_port`)
/// for `extendData.p2p`.
pub fn detect_local_p2p_endpoint(override_port: Option<u16>) -> (String, u16) {
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let port =
            override_port.unwrap_or_else(|| sock.local_addr().map(|a| a.port()).unwrap_or(60020));
        if sock.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = sock.local_addr() {
                return (local.ip().to_string(), port);
            }
        }
        return ("127.0.0.1".to_string(), port);
    }
    ("127.0.0.1".to_string(), override_port.unwrap_or(60020))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn zrtp_packet_builders_match_verified_wire_layout() {
        // InitZRTP caller (0x000B)
        let init_c = build_zrtp_init_packet(true, 111, 222, 333, ZRTP_SUB_INIT_V3, "sess_abc");
        assert_eq!(init_c[0], 0x01);
        assert_eq!(init_c[1], 0x7E);
        assert_eq!(u32::from_le_bytes(init_c[10..14].try_into().unwrap()), 111);
        assert_eq!(u32::from_le_bytes(init_c[14..18].try_into().unwrap()), 0);
        assert_eq!(
            u16::from_le_bytes(init_c[18..20].try_into().unwrap()),
            0x000B
        );
        assert_eq!(init_c[20], 2);
        assert_eq!(u32::from_le_bytes(init_c[21..25].try_into().unwrap()), 333);
        assert_eq!(u32::from_le_bytes(init_c[25..29].try_into().unwrap()), 222);
        assert_eq!(u16::from_le_bytes(init_c[29..31].try_into().unwrap()), 8);
        assert_eq!(&init_c[31..39], b"sess_abc");

        // InitZRTP callee (0x000C)
        let init_r = build_zrtp_init_packet(false, 222, 111, 333, ZRTP_SUB_INIT_V1, "s");
        assert_eq!(
            u16::from_le_bytes(init_r[18..20].try_into().unwrap()),
            0x000C
        );
        assert_eq!(init_r[20], 1);

        // Echo (0x0005)
        let echo = build_zrtp_echo_packet(42, "1700000000");
        assert_eq!(&echo[0..2], &[0x01, 0x01]);
        assert_eq!(u32::from_le_bytes(echo[6..10].try_into().unwrap()), 42);
        assert_eq!(
            u32::from_le_bytes(echo[10..14].try_into().unwrap()),
            0x003E_9765
        );
        assert_eq!(
            u32::from_le_bytes(echo[14..18].try_into().unwrap()),
            0x012F_833C
        );
        assert_eq!(u16::from_le_bytes(echo[18..20].try_into().unwrap()), 0x0005);
        assert_eq!(echo[20], 0);
        assert_eq!(u16::from_le_bytes(echo[21..23].try_into().unwrap()), 10);
        assert_eq!(&echo[23..33], b"1700000000");
        assert_eq!(parse_zrtp_echo_response(&echo), Some(42));

        // Ping (0x0002)
        let ping = build_zrtp_ping_packet(111, 0xDEAD_BEEF, "sess_abc");
        assert_eq!(&ping[0..2], &[0x01, 0x01]);
        assert_eq!(u32::from_le_bytes(ping[10..14].try_into().unwrap()), 111);
        assert_eq!(
            u32::from_le_bytes(ping[14..18].try_into().unwrap()),
            0xDEAD_BEEF
        );
        assert_eq!(u16::from_le_bytes(ping[18..20].try_into().unwrap()), 0x0002);
        assert_eq!(ping[20], 1);
        assert_eq!(u16::from_le_bytes(ping[21..23].try_into().unwrap()), 8);
        assert_eq!(&ping[23..31], b"sess_abc");

        // End (0x0020 sub 0x09, u32 LE string length)
        let end = build_zrtp_end_packet(111, 333, "sess_abc");
        assert_eq!(&end[0..2], &[0x01, 0x01]);
        assert_eq!(u32::from_le_bytes(end[10..14].try_into().unwrap()), 111);
        assert_eq!(u32::from_le_bytes(end[14..18].try_into().unwrap()), 333);
        assert_eq!(u16::from_le_bytes(end[18..20].try_into().unwrap()), 0x0020);
        assert_eq!(end[20], 0x09);
        assert_eq!(u32::from_le_bytes(end[21..25].try_into().unwrap()), 8);
        assert_eq!(&end[25..33], b"sess_abc");
    }

    #[test]
    fn parse_zrtp_init_response_handles_success_error_and_malformed() {
        let ok_pkt = encode_zrtp_init_response(
            ZRTP_CMD_INIT_CALLER,
            0,
            100_000_010,
            0xCAFE_BABE,
            Some(("203.0.113.10", 9849)),
        );
        let parsed = parse_zrtp_init_response(&ok_pkt).unwrap();
        assert_eq!(parsed.res, 0);
        assert_eq!(parsed.call_id, 100_000_010);
        assert_eq!(parsed.token, 0xCAFE_BABE);
        assert_eq!(parsed.public_p2p, Some(("203.0.113.10".to_string(), 9849)));

        // Non-zero error code (-106)
        let err_pkt = encode_zrtp_init_response(ZRTP_CMD_INIT_CALLEE, -106, 0, 0, None);
        let err_parsed = parse_zrtp_init_response(&err_pkt).unwrap();
        assert_eq!(err_parsed.res, -106);
        assert_eq!(err_parsed.token, 0);
        assert!(err_parsed.public_p2p.is_none());

        // Truncated or unrelated packets return None
        assert!(parse_zrtp_init_response(&[0x02, 0x7E, 0x00]).is_none());
        assert!(parse_zrtp_init_response(&build_zrtp_echo_packet(1, "1")).is_none());

        // Alternate payload type 0x7F and colon separator `ip:port`, plus invalid port=0 / empty ip
        let mut colon_pkt = encode_zrtp_init_response(ZRTP_CMD_INIT_CALLEE, 0, 7, 99, None);
        colon_pkt[1] = ZRTP_PT_INIT_ALT;
        let raw_addr = b"198.51.100.42:12345";
        colon_pkt.extend_from_slice(&(raw_addr.len() as u16).to_le_bytes());
        colon_pkt.extend_from_slice(raw_addr);
        let parsed_colon = parse_zrtp_init_response(&colon_pkt).unwrap();
        assert_eq!(
            parsed_colon.public_p2p,
            Some(("198.51.100.42".to_string(), 12345))
        );

        let zero_port = encode_zrtp_init_response(
            ZRTP_CMD_INIT_CALLER,
            0,
            7,
            99,
            Some(("198.51.100.42", 0)),
        );
        assert_eq!(parse_zrtp_init_response(&zero_port).unwrap().public_p2p, None);
    }

    #[test]
    fn relay_media_wrap_and_unwrap_covers_all_header_variants() {
        let rtp = [0x80u8, 112, 0x00, 0x01, 0, 0, 1, 64, 0x11, 0x22, 0x33, 0x44, 0xAA, 0xBB];
        // Raw RTP
        assert_eq!(unwrap_relay_media(&rtp), Some(&rtp[..]));

        // 5-byte audio RTP wrapper
        let wrapped = wrap_relay_audio_rtp(0x1234_5678, &rtp);
        assert_eq!(wrapped[0], RELAY_WRAP_AUDIO_RTP);
        assert_eq!(
            u32::from_le_bytes(wrapped[1..5].try_into().unwrap()),
            0x1234_5678
        );
        assert_eq!(unwrap_relay_media(&wrapped), Some(&rtp[..]));

        // 1-byte incoming audio wrapper (0x04)
        let mut one_byte = vec![0x04];
        one_byte.extend_from_slice(&rtp);
        assert_eq!(unwrap_relay_media(&one_byte), Some(&rtp[..]));

        // 9-byte 0x7F wrapper
        let mut h7f = vec![0x7F, 0x00, 0x01, 0, 0, 0, 0, 0x08, 0x00];
        h7f.extend_from_slice(&rtp);
        assert_eq!(unwrap_relay_media(&h7f), Some(&rtp[..]));

        // 25-byte 0x02 wrapper (cmd=0x0032)
        let mut h02 = vec![0u8; 25];
        h02[0] = 0x02;
        h02[18..20].copy_from_slice(&0x0032u16.to_le_bytes());
        h02.extend_from_slice(&rtp);
        assert_eq!(unwrap_relay_media(&h02), Some(&rtp[..]));

        // Minimal 8-byte RTCP RR inside 5-byte 0x05 wrapper
        let rtcp_rr = [0x80u8, 201, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44];
        let wrapped_rtcp = wrap_relay_audio_rtcp(0x9988_7766, &rtcp_rr);
        assert_eq!(unwrap_relay_media(&wrapped_rtcp), Some(&rtcp_rr[..]));

        // Garbage / short rejected
        assert!(unwrap_relay_media(&[0x04, 0x00, 0x01]).is_none());
        assert!(unwrap_relay_media(&[0xFF; 20]).is_none());
    }

    #[test]
    fn collect_relay_servers_deduplicates_across_all_sources() {
        let payload = json!({
            "servers": [
                { "rtpaddr": "42.113.206.4:10490", "rtcpaddr": "42.113.206.4:12490" },
                { "rtpaddr": "42.113.206.4:10490", "rtcpaddr": "42.113.206.4:12490" }
            ],
            "extendData": r#"{"serverAddr":[{"rtp":"171.244.128.62:4200","rtcp":"171.244.128.62:4200"}]}"#,
            "rtpIP": "14.238.143.137:3000",
            "rtcpIP": "14.238.143.137:3500"
        });
        let servers = collect_relay_servers(&payload);
        assert_eq!(servers.len(), 3);
        assert_eq!(servers[0].rtp, "42.113.206.4:10490");
        assert_eq!(servers[0].rtcp, "42.113.206.4:12490");
        assert!(servers[0].has_distinct_rtcp());
        assert_eq!(servers[1].rtp, "171.244.128.62:4200");
        assert!(!servers[1].has_distinct_rtcp());
        assert_eq!(servers[2].rtp, "14.238.143.137:3000");
        assert_eq!(servers[2].rtcp, "14.238.143.137:3500");
    }

    #[tokio::test]
    async fn mock_udp_relay_probe_handles_echo_init_rtcp_ping_and_end() {
        // Bind mock RTP + distinct RTCP relay sockets on localhost.
        let mock_rtp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mock_rtcp = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let rtp_addr = mock_rtp.local_addr().unwrap();
        let rtcp_addr = mock_rtcp.local_addr().unwrap();

        let rtp_echoes = Arc::new(AtomicUsize::new(0));
        let rtp_pings = Arc::new(AtomicUsize::new(0));
        let rtp_ends = Arc::new(AtomicUsize::new(0));
        let rtcp_inits = Arc::new(AtomicUsize::new(0));
        let rtcp_ends = Arc::new(AtomicUsize::new(0));

        let (stop_tx, mut stop_rx) = watch::channel(false);
        let srv_rtp = mock_rtp.clone();
        let srv_rtcp = mock_rtcp.clone();
        let c_echo = rtp_echoes.clone();
        let c_ping = rtp_pings.clone();
        let c_end = rtp_ends.clone();
        let c_rtcp_init = rtcp_inits.clone();
        let c_rtcp_end = rtcp_ends.clone();

        let server_task = tokio::spawn(async move {
            let mut b1 = [0u8; 1500];
            let mut b2 = [0u8; 1500];
            let mut sent_synthetic_rtcp = false;
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = srv_rtp.recv_from(&mut b1) => {
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([b1[18], b1[19]]);
                        match cmd {
                            ZRTP_CMD_ECHO => {
                                c_echo.fetch_add(1, Ordering::Relaxed);
                                let mut reply = b1[..n].to_vec();
                                reply[0] = ZRTP_MAGIC_RESP;
                                let _ = srv_rtp.send_to(&reply, peer).await;
                            }
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                let call_id = u32::from_le_bytes(b1[21..25].try_into().unwrap());
                                let reply = encode_zrtp_init_response(
                                    cmd,
                                    0,
                                    call_id,
                                    0x5566_7788,
                                    Some(("198.51.100.7", peer.port())),
                                );
                                let _ = srv_rtp.send_to(&reply, peer).await;
                            }
                            ZRTP_CMD_PING => {
                                c_ping.fetch_add(1, Ordering::Relaxed);
                            }
                            ZRTP_CMD_END => {
                                c_end.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                    Ok((n, peer)) = srv_rtcp.recv_from(&mut b2) => {
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([b2[18], b2[19]]);
                        match cmd {
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                c_rtcp_init.fetch_add(1, Ordering::Relaxed);
                                let call_id = u32::from_le_bytes(b2[21..25].try_into().unwrap());
                                let reply = encode_zrtp_init_response(
                                    cmd,
                                    0,
                                    call_id,
                                    0x5566_7788,
                                    None,
                                );
                                let _ = srv_rtcp.send_to(&reply, peer).await;
                                if !sent_synthetic_rtcp {
                                    sent_synthetic_rtcp = true;
                                    // Send a synthetic RTCP RR back to test RTCP inbox forwarding.
                                    let rr = wrap_relay_audio_rtcp(
                                        0x5566_7788,
                                        &[0x80, 201, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD],
                                    );
                                    let _ = srv_rtcp.send_to(&rr, peer).await;
                                }
                            }
                            ZRTP_CMD_END => {
                                c_rtcp_end.fetch_add(1, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        // 1. Standalone echo probe
        let echo_summary = probe_udp_echo(rtp_addr, 4, Duration::from_millis(400))
            .await
            .unwrap();
        assert_eq!(echo_summary.sent, 4);
        assert_eq!(echo_summary.received, 4);
        assert!(echo_summary.avg_rtt_ms.is_some());

        // 2. Full ZRTP relay session with distinct RTCP socket
        let cfg = ZrtpProbeConfig {
            echo_bursts: 2,
            echoes_per_burst: 3,
            burst_gap: Duration::from_millis(10),
            probe_timeout: Duration::from_millis(180),
            keepalive_interval: Duration::from_millis(80),
            sub_cmd: ZRTP_SUB_INIT_V3,
            trace: None,
        };
        let session = ZrtpRelaySession::start_with_config(
            vec![RelayServer::new(rtp_addr.to_string(), rtcp_addr.to_string())],
            12345,
            67890,
            464_564_007,
            true,
            "mock_sess_token",
            cfg,
        )
        .await
        .unwrap();

        assert_eq!(session.probe().token, 0x5566_7788);
        assert_eq!(
            session.probe().selected_rtp.as_deref(),
            Some(rtp_addr.to_string().as_str())
        );
        assert_eq!(
            session.probe().public_p2p,
            Some(("198.51.100.7".to_string(), session.probe().local_port))
        );
        assert!(session.probe().rtcp_acked);
        assert!(
            session
                .probe()
                .echo_stats
                .get(&rtp_addr.to_string())
                .map(|(cnt, _)| *cnt >= 4)
                .unwrap_or(false)
        );

        // Wait for at least one keepalive tick + synthetic RTCP packet
        tokio::time::sleep(Duration::from_millis(130)).await;
        let rtcp_msgs = session.drain_rtcp_inbox();
        assert!(
            !rtcp_msgs.is_empty(),
            "expected non-ZRTP RTCP datagram to be queued in RTCP inbox"
        );
        assert_eq!(
            unwrap_relay_media(&rtcp_msgs[0]),
            Some(&[0x80, 201, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD][..])
        );

        // Graceful shutdown must transmit End packets to both RTP and distinct RTCP ports
        session.shutdown().await;
        tokio::time::sleep(Duration::from_millis(40)).await;

        assert!(rtp_pings.load(Ordering::Relaxed) >= 1);
        assert!(rtp_ends.load(Ordering::Relaxed) >= 1);
        assert!(rtcp_inits.load(Ordering::Relaxed) >= 1);
        assert!(rtcp_ends.load(Ordering::Relaxed) >= 1);

        let _ = stop_tx.send(true);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn single_port_relay_never_sends_from_rtcp_socket() {
        // Verify that when rtcp == rtp, only ONE client source port hits the relay.
        let mock_srv = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let srv_addr = mock_srv.local_addr().unwrap();
        let seen_ports = Arc::new(Mutex::new(std::collections::HashSet::<u16>::new()));
        let (stop_tx, mut stop_rx) = watch::channel(false);

        let srv_c = mock_srv.clone();
        let ports_c = seen_ports.clone();
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = srv_c.recv_from(&mut buf) => {
                        ports_c.lock().unwrap().insert(peer.port());
                        if n >= ZRTP_HEADER_LEN {
                            let cmd = u16::from_le_bytes([buf[18], buf[19]]);
                            if matches!(cmd, ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE) {
                                let call_id = u32::from_le_bytes(buf[21..25].try_into().unwrap());
                                let reply = encode_zrtp_init_response(cmd, 0, call_id, 777, None);
                                let _ = srv_c.send_to(&reply, peer).await;
                            }
                        }
                    }
                }
            }
        });

        let session = ZrtpRelaySession::start_with_config(
            vec![RelayServer::new(srv_addr.to_string(), srv_addr.to_string())],
            10,
            20,
            30,
            true,
            "single_port_sess",
            ZrtpProbeConfig {
                echo_bursts: 1,
                echoes_per_burst: 2,
                burst_gap: Duration::ZERO,
                probe_timeout: Duration::from_millis(120),
                keepalive_interval: Duration::from_millis(60),
                sub_cmd: ZRTP_SUB_INIT_V3,
                trace: None,
            },
        )
        .await
        .unwrap();

        let expected_port = session.probe().local_port;
        tokio::time::sleep(Duration::from_millis(100)).await;
        session.shutdown().await;
        tokio::time::sleep(Duration::from_millis(30)).await;

        let _ = stop_tx.send(true);
        let _ = task.await;

        let ports = seen_ports.lock().unwrap();
        assert_eq!(ports.len(), 1, "single-port relay saw multiple client ports: {ports:?}");
        assert!(ports.contains(&expected_port));
    }

    #[tokio::test]
    async fn late_zrtp_init_ack_and_concurrent_std_udp_media_work_without_contention() {
        // Mock relay deliberately ignores initial probe bursts and only answers the 3rd InitZRTP,
        // forcing initial `probe.token == 0`, then also echoes wrapped RTP packets back.
        let mock_srv = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let srv_addr = mock_srv.local_addr().unwrap();
        let init_seen = Arc::new(AtomicUsize::new(0));
        let pings_seen = Arc::new(AtomicUsize::new(0));
        let rtp_wrapped_seen = Arc::new(AtomicUsize::new(0));
        let (stop_tx, mut stop_rx) = watch::channel(false);

        let srv_c = mock_srv.clone();
        let c_init = init_seen.clone();
        let c_ping = pings_seen.clone();
        let c_rtp = rtp_wrapped_seen.clone();
        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = srv_c.recv_from(&mut buf) => {
                        if n >= 5 && buf[0] == RELAY_WRAP_AUDIO_RTP {
                            let tok = u32::from_le_bytes(buf[1..5].try_into().unwrap());
                            if tok == 0x7788_99AA {
                                c_rtp.fetch_add(1, Ordering::Relaxed);
                                // Echo back with 1-byte incoming audio header (0x04)
                                let mut down = Vec::with_capacity(1 + n - 5);
                                down.push(0x04);
                                down.extend_from_slice(&buf[5..n]);
                                let _ = srv_c.send_to(&down, peer).await;
                            }
                            continue;
                        }
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([buf[18], buf[19]]);
                        match cmd {
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                let k = c_init.fetch_add(1, Ordering::Relaxed) + 1;
                                if k >= 2 {
                                    let call_id = u32::from_le_bytes(buf[21..25].try_into().unwrap());
                                    let reply = encode_zrtp_init_response(
                                        cmd,
                                        0,
                                        call_id,
                                        0x7788_99AA,
                                        None,
                                    );
                                    let _ = srv_c.send_to(&reply, peer).await;
                                }
                            }
                            ZRTP_CMD_PING => {
                                let tok = u32::from_le_bytes(buf[14..18].try_into().unwrap());
                                if tok == 0x7788_99AA {
                                    c_ping.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        let session = ZrtpRelaySession::start_with_config(
            vec![RelayServer::new(srv_addr.to_string(), srv_addr.to_string())],
            100,
            200,
            300,
            true,
            "late_ack_sess",
            ZrtpProbeConfig {
                echo_bursts: 1,
                echoes_per_burst: 0,
                burst_gap: Duration::ZERO,
                probe_timeout: Duration::from_millis(80),
                keepalive_interval: Duration::from_millis(60),
                sub_cmd: ZRTP_SUB_INIT_V3,
                trace: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(session.probe().token, 0, "first InitZRTP was deliberately dropped");

        let std_sock = session.std_rtp_socket();
        let srv_str = srv_addr.to_string();
        let live_tok = session.token_handle_for(&srv_str);
        let attached = session.media_attached_flag();
        attached.store(true, Ordering::Relaxed);

        // OS thread mirrors AudioMediaSession: sole reader of std_rtp_sock, captures late InitZRTP
        // token into live_tok, then sends/receives wrapped RTP concurrently with keepalive_task.
        let rtp_rx_count = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 2048];
            let mut rx_rtp = 0usize;
            let deadline = Instant::now() + Duration::from_millis(700);
            let mut seq: u16 = 0;
            while Instant::now() < deadline && rx_rtp < 4 {
                while let Ok((n, peer)) = std_sock.recv_from(&mut buf) {
                    if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                        if inner.len() >= 12 {
                            rx_rtp += 1;
                        }
                    } else if peer == srv_addr {
                        if let Some(init) = parse_zrtp_init_response(&buf[..n]) {
                            if init.res == 0 && init.token != 0 {
                                live_tok.store(init.token, Ordering::Relaxed);
                            }
                        }
                    }
                }
                let tok = live_tok.load(Ordering::Relaxed);
                if tok != 0 {
                    let rtp = [
                        0x80u8,
                        112,
                        (seq >> 8) as u8,
                        seq as u8,
                        0,
                        0,
                        1,
                        64,
                        1,
                        2,
                        3,
                        4,
                        0xAA,
                        0xBB,
                    ];
                    seq = seq.wrapping_add(1);
                    let wire = wrap_relay_audio_rtp(tok, &rtp);
                    let _ = std_sock.send_to(&wire, srv_addr);
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            attached.store(false, Ordering::Relaxed);
            rx_rtp
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(session.token_for(&srv_str), 0x7788_99AA);
        assert_eq!(session.token_handle_for(&srv_str).load(Ordering::Relaxed), 0x7788_99AA);
        assert!(rtp_rx_count >= 4, "expected >=4 echoed RTP packets, got {rtp_rx_count}");
        assert!(
            rtp_wrapped_seen.load(Ordering::Relaxed) >= 4,
            "relay should have seen wrapped RTP with late token"
        );
        assert!(
            pings_seen.load(Ordering::Relaxed) >= 1,
            "keepalive_task must observe late token and switch to Ping"
        );

        session.shutdown().await;
        let _ = stop_tx.send(true);
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn multi_relay_per_server_tokens_never_contaminate_and_late_server_b_retries_init() {
        const TOKEN_A: u32 = 0xAAAA_1111;
        const TOKEN_A_SPURIOUS: u32 = 0xAAAA_9999;
        const TOKEN_B: u32 = 0xBBBB_2222;
        const TOKEN_C: u32 = 0xCCCC_3333;

        let sock_a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_c = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr_a = sock_a.local_addr().unwrap();
        let addr_b = sock_b.local_addr().unwrap();
        let addr_c = sock_c.local_addr().unwrap();
        let str_a = addr_a.to_string();
        let str_b = addr_b.to_string();
        let str_c = addr_c.to_string();

        let a_pings_ok = Arc::new(AtomicUsize::new(0));
        let b_inits = Arc::new(AtomicUsize::new(0));
        let b_pings_ok = Arc::new(AtomicUsize::new(0));
        let b_rtp_ok = Arc::new(AtomicUsize::new(0));
        let b_rtp_wrong_token = Arc::new(AtomicUsize::new(0));
        let c_inits = Arc::new(AtomicUsize::new(0));
        let c_pings_ok = Arc::new(AtomicUsize::new(0));

        let (stop_tx, mut stop_rx) = watch::channel(false);

        let sa = sock_a.clone();
        let sb = sock_b.clone();
        let sc = sock_c.clone();
        let ca_ping = a_pings_ok.clone();
        let cb_init = b_inits.clone();
        let cb_ping = b_pings_ok.clone();
        let cb_rtp_ok = b_rtp_ok.clone();
        let cb_rtp_bad = b_rtp_wrong_token.clone();
        let cc_init = c_inits.clone();
        let cc_ping = c_pings_ok.clone();

        let relays_task = tokio::spawn(async move {
            let mut ba = [0u8; 1500];
            let mut bb = [0u8; 1500];
            let mut bc = [0u8; 1500];
            let mut sent_spurious_a = false;
            loop {
                tokio::select! {
                    _ = stop_rx.changed() => break,
                    Ok((n, peer)) = sa.recv_from(&mut ba) => {
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([ba[18], ba[19]]);
                        match cmd {
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                let cid = u32::from_le_bytes(ba[21..25].try_into().unwrap());
                                let rep = encode_zrtp_init_response(cmd, 0, cid, TOKEN_A, None);
                                let _ = sa.send_to(&rep, peer).await;
                            }
                            ZRTP_CMD_PING => {
                                let tok = u32::from_le_bytes(ba[14..18].try_into().unwrap());
                                if tok == TOKEN_A {
                                    let seen = ca_ping.fetch_add(1, Ordering::Relaxed) + 1;
                                    // Emit one spurious late InitZRTP from ServerA while media targets ServerB
                                    if seen >= 2 && !sent_spurious_a {
                                        sent_spurious_a = true;
                                        let sp = encode_zrtp_init_response(
                                            ZRTP_CMD_INIT_CALLER,
                                            0,
                                            500,
                                            TOKEN_A_SPURIOUS,
                                            None,
                                        );
                                        let _ = sa.send_to(&sp, peer).await;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok((n, peer)) = sb.recv_from(&mut bb) => {
                        if n >= 5 && bb[0] == RELAY_WRAP_AUDIO_RTP {
                            let tok = u32::from_le_bytes(bb[1..5].try_into().unwrap());
                            if tok == TOKEN_B {
                                cb_rtp_ok.fetch_add(1, Ordering::Relaxed);
                                let mut down = Vec::with_capacity(1 + n - 5);
                                down.push(0x04);
                                down.extend_from_slice(&bb[5..n]);
                                let _ = sb.send_to(&down, peer).await;
                            } else {
                                cb_rtp_bad.fetch_add(1, Ordering::Relaxed);
                            }
                            continue;
                        }
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([bb[18], bb[19]]);
                        match cmd {
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                // Drop initial probe burst (1st InitZRTP), answer on 2nd+ (keepalive retry!)
                                let k = cb_init.fetch_add(1, Ordering::Relaxed) + 1;
                                if k >= 2 {
                                    let cid = u32::from_le_bytes(bb[21..25].try_into().unwrap());
                                    let rep = encode_zrtp_init_response(cmd, 0, cid, TOKEN_B, None);
                                    let _ = sb.send_to(&rep, peer).await;
                                }
                            }
                            ZRTP_CMD_PING => {
                                let tok = u32::from_le_bytes(bb[14..18].try_into().unwrap());
                                if tok == TOKEN_B {
                                    cb_ping.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok((n, peer)) = sc.recv_from(&mut bc) => {
                        if n < ZRTP_HEADER_LEN { continue; }
                        let cmd = u16::from_le_bytes([bc[18], bc[19]]);
                        match cmd {
                            ZRTP_CMD_INIT_CALLER | ZRTP_CMD_INIT_CALLEE => {
                                // Drop initial ensure_target_registered packet; answer on keepalive retry!
                                let k = cc_init.fetch_add(1, Ordering::Relaxed) + 1;
                                if k >= 2 {
                                    let cid = u32::from_le_bytes(bc[21..25].try_into().unwrap());
                                    let rep = encode_zrtp_init_response(cmd, 0, cid, TOKEN_C, None);
                                    let _ = sc.send_to(&rep, peer).await;
                                }
                            }
                            ZRTP_CMD_PING => {
                                let tok = u32::from_le_bytes(bc[14..18].try_into().unwrap());
                                if tok == TOKEN_C {
                                    cc_ping.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        // Start session with [ServerA, ServerB]: ServerA acks during probe, ServerB drops initial probe.
        let session = ZrtpRelaySession::start_with_config(
            vec![
                RelayServer::new(str_a.clone(), str_a.clone()),
                RelayServer::new(str_b.clone(), str_b.clone()),
            ],
            100,
            200,
            500,
            true,
            "multi_relay_sess",
            ZrtpProbeConfig {
                echo_bursts: 1,
                echoes_per_burst: 0,
                burst_gap: Duration::ZERO,
                probe_timeout: Duration::from_millis(80),
                keepalive_interval: Duration::from_millis(60),
                sub_cmd: ZRTP_SUB_INIT_V3,
                trace: None,
            },
        )
        .await
        .unwrap();

        // ServerA acked, ServerB must remain 0 (NEVER contaminated by TOKEN_A!)
        assert_eq!(session.token_for(&str_a), TOKEN_A);
        assert_eq!(session.token_for(&str_b), 0);
        let tok_handle_b = session.token_handle_for(&str_b);
        assert_eq!(tok_handle_b.load(Ordering::Relaxed), 0);

        // Simulate AudioMediaSession selecting ServerB: only accepts InitZRTP from `addr_b`
        let std_sock = session.std_rtp_socket();
        let attached = session.media_attached_flag();
        attached.store(true, Ordering::Relaxed);
        let tok_b_thread = tok_handle_b.clone();
        let rx_from_b = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 2048];
            let mut rx_rtp = 0usize;
            let deadline = Instant::now() + Duration::from_millis(800);
            let mut seq: u16 = 0;
            while Instant::now() < deadline && rx_rtp < 4 {
                while let Ok((n, peer)) = std_sock.recv_from(&mut buf) {
                    if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                        if inner.len() >= 12 {
                            rx_rtp += 1;
                        }
                    } else if peer == addr_b {
                        if let Some(init) = parse_zrtp_init_response(&buf[..n]) {
                            if init.res == 0 && init.token != 0 {
                                tok_b_thread.store(init.token, Ordering::Relaxed);
                            }
                        }
                    }
                }
                let tok = tok_b_thread.load(Ordering::Relaxed);
                if tok != 0 {
                    let rtp = [
                        0x80u8,
                        112,
                        (seq >> 8) as u8,
                        seq as u8,
                        0,
                        0,
                        1,
                        64,
                        1,
                        2,
                        3,
                        4,
                        0xCC,
                        0xDD,
                    ];
                    seq = seq.wrapping_add(1);
                    let wire = wrap_relay_audio_rtp(tok, &rtp);
                    let _ = std_sock.send_to(&wire, addr_b);
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            attached.store(false, Ordering::Relaxed);
            rx_rtp
        })
        .await
        .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(session.token_for(&str_a), TOKEN_A);
        assert_eq!(session.token_for(&str_b), TOKEN_B);
        assert!(rx_from_b >= 4, "expected >=4 RTP packets echoed by ServerB, got {rx_from_b}");
        assert!(b_rtp_ok.load(Ordering::Relaxed) >= 4);
        assert_eq!(
            b_rtp_wrong_token.load(Ordering::Relaxed),
            0,
            "ServerB must NEVER receive RTP wrapped with ServerA's token"
        );
        assert!(
            b_pings_ok.load(Ordering::Relaxed) >= 1,
            "keepalive_task must switch ServerB to Ping(TOKEN_B) once ServerB acks"
        );
        assert!(a_pings_ok.load(Ordering::Relaxed) >= 1);

        // Now dynamically register a 3rd relay ServerC after A & B already have tokens:
        // keepalive_task must still retry InitZRTP for ServerC and pick up TOKEN_C cleanly.
        session
            .ensure_target_registered(RelayServer::new(str_c.clone(), str_c.clone()))
            .await;
        assert_eq!(session.token_for(&str_c), 0);

        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline && session.token_for(&str_c) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(session.token_for(&str_c), TOKEN_C);
        assert_eq!(session.token_for(&str_a), TOKEN_A);
        assert_eq!(session.token_for(&str_b), TOKEN_B);
        assert!(
            c_pings_ok.load(Ordering::Relaxed) >= 1,
            "keepalive_task must ping late-registered ServerC with TOKEN_C"
        );

        session.shutdown().await;
        let _ = stop_tx.send(true);
        let _ = relays_task.await;
    }

    #[tokio::test]
    async fn media_attached_notify_immediately_disarms_keepalive_rtp_recv_and_ignores_spoofed_peers() {
        // Bind a silent registered relay (token stays 0 so keepalive_task arms `rtp_loop.recv_from`)
        // plus an unregistered rogue socket.
        let silent_relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let rogue = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let silent_addr = silent_relay.local_addr().unwrap();
        let rogue_addr = rogue.local_addr().unwrap();

        let session = ZrtpRelaySession::start_with_config(
            vec![RelayServer::new(silent_addr.to_string(), silent_addr.to_string())],
            10,
            20,
            777,
            true,
            "disarm_test_sess",
            ZrtpProbeConfig {
                echo_bursts: 1,
                echoes_per_burst: 0,
                burst_gap: Duration::ZERO,
                probe_timeout: Duration::from_millis(40),
                // Long keepalive interval proves `media_attached_notify` wakes `keepalive_task`
                // immediately rather than waiting for the next timer tick!
                keepalive_interval: Duration::from_secs(10),
                sub_cmd: ZRTP_SUB_INIT_V3,
                trace: None,
            },
        )
        .await
        .unwrap();

        let client_rtp_addr: SocketAddr =
            format!("127.0.0.1:{}", session.probe().local_port).parse().unwrap();

        // 1. Rogue InitZRTP from unregistered UDP peer MUST be ignored by keepalive_task.
        let spoof = encode_zrtp_init_response(ZRTP_CMD_INIT_CALLER, 0, 777, 0xBAD0_CAFE, None);
        rogue.send_to(&spoof, client_rtp_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(session.token_for(&rogue_addr.to_string()), 0);
        assert_eq!(session.token_for(&silent_addr.to_string()), 0);

        // 2. Flip `media_attached` + notify -> keepalive_task must immediately stop calling
        // `rtp_loop.recv_from`, so ALL 8 incoming RTP packets reach `std_rtp_sock` with zero loss.
        let attached = session.media_attached_flag();
        let notify = session.media_attached_notify();
        attached.store(true, Ordering::Release);
        notify.notify_one();
        // Give Tokio a brief moment to process the wakeup before sending media.
        tokio::time::sleep(Duration::from_millis(25)).await;

        let std_sock = session.std_rtp_socket();
        for seq in 0u16..8 {
            let mut down = vec![
                0x04,
                0x80,
                112,
                (seq >> 8) as u8,
                seq as u8,
                0,
                0,
                1,
                64,
                1,
                2,
                3,
                4,
                seq as u8,
            ];
            let _ = silent_relay.send_to(&down, client_rtp_addr).await;
            down.clear();
        }
        tokio::time::sleep(Duration::from_millis(40)).await;

        let mut received = 0usize;
        let mut buf = [0u8; 256];
        while let Ok((n, _)) = std_sock.recv_from(&mut buf) {
            if unwrap_relay_media(&buf[..n]).is_some() {
                received += 1;
            }
        }
        assert_eq!(
            received, 8,
            "keepalive_task must not steal RTP packets once media_attached_notify fires"
        );

        attached.store(false, Ordering::Release);
        notify.notify_one();
        session.shutdown().await;
    }
}
