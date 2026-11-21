//! Real-time `zpw_ws` WebSocket client with AES-GCM + zlib/gzip frame decryption.

use std::io::Read;
use std::time::Duration;

use aes::{Aes128, Aes256};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::AesGcm;
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use generic_array::typenum::U16;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, COOKIE, ORIGIN, USER_AGENT};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, WebSocketStream};

use crate::session::{truncate, Session};

pub const CMD_AUTHEN: u16 = 1;
pub const CMD_PING: u16 = 2;
pub const SUB_KEEP_CONNECTION: u8 = 1;

pub const CMD_PUSH_MSG_1_1: u16 = 501;
pub const CMD_DELIVERED_1_1: u16 = 502;
pub const CMD_SEEN_1_1: u16 = 503;
pub const CMD_PULL_MSG_1_1: u16 = 510;
pub const CMD_PULL_MSG_GROUP: u16 = 511;
pub const CMD_PULL_MSG_513: u16 = 513;
pub const CMD_PULL_MSG_514: u16 = 514;
pub const CMD_PULL_MSG_515: u16 = 515;
pub const CMD_PULL_MSG_517: u16 = 517;
pub const CMD_PULL_MSG_518: u16 = 518;
pub const CMD_PUSH_MSG_GROUP: u16 = 521;
pub const CMD_DELIVERED_GROUP: u16 = 522;
pub const CMD_SEEN_GROUP: u16 = 523;
pub const CMD_HAS_MSG: u16 = 533;
pub const CMD_PUSH_MISS_MSG: u16 = 534;
pub const CMD_GET_MSG: u16 = 570;

pub const CMD_CTRL_PUSH_CTRL: u16 = 601;
pub const CMD_CTRL_ACTION: u16 = 602;
pub const CMD_CTRL_PULL_CTRL: u16 = 603;

const PING_SECS: u64 = 25;
const WS_WRITE_TIMEOUT_SECS: u64 = 10;
const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WsStatus {
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone)]
pub struct WsEvent {
    pub cmd: u16,
    pub sub_cmd: u8,
    pub version: u8,
    pub payload: Value,
}

impl WsEvent {
    pub fn is_chat_ish(&self) -> bool {
        matches!(
            self.cmd,
            CMD_PUSH_MSG_1_1
                | CMD_PUSH_MSG_GROUP
                | CMD_PULL_MSG_1_1
                | CMD_PULL_MSG_GROUP
                | CMD_PULL_MSG_513
                | CMD_PULL_MSG_514
                | CMD_PULL_MSG_515
                | CMD_PULL_MSG_517
                | CMD_PULL_MSG_518
                | CMD_PUSH_MISS_MSG
                | CMD_GET_MSG
        )
    }

    pub fn is_control_ish(&self) -> bool {
        matches!(self.cmd, CMD_CTRL_PUSH_CTRL | CMD_CTRL_ACTION | CMD_CTRL_PULL_CTRL)
    }

    pub fn is_receipt_ish(&self) -> bool {
        matches!(
            self.cmd,
            CMD_DELIVERED_1_1 | CMD_SEEN_1_1 | CMD_DELIVERED_GROUP | CMD_SEEN_GROUP
        )
    }
}

#[derive(Debug, Clone)]
pub enum WsSignal {
    Status(WsStatus),
    Event(WsEvent),
    Expired { code: i64, message: String },
}

enum WsRunOutcome {
    Disconnected,
    Expired { code: i64, message: String },
    UntrustedHost,
}

/// Encode `u8 version | u16 LE cmd | u8 subCmd | JSON body`.
pub fn encode_frame(cmd: u16, sub_cmd: u8, version: u8, body: Option<&Value>) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.push(version);
    out.extend_from_slice(&cmd.to_le_bytes());
    out.push(sub_cmd);
    if let Some(v) = body {
        out.extend_from_slice(v.to_string().as_bytes());
    }
    out
}

/// Decode `u8 version | u16 LE cmd | u8 subCmd | JSON body`.
pub fn decode_frame(buf: &[u8]) -> Option<(u8, u16, u8, Value)> {
    if buf.len() < 4 {
        return None;
    }
    let version = buf[0];
    let cmd = u16::from_le_bytes([buf[1], buf[2]]);
    let sub_cmd = buf[3];
    let body = &buf[4..];
    let val = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).ok()?
    };
    Some((version, cmd, sub_cmd, val))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = (bytes[i + 1] as char).to_digit(16);
            let h2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(a), Some(b)) = (h1, h2) {
                out.push(((a << 4) | b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

type DecompressFn = fn(&[u8]) -> Box<dyn Read + '_>;

fn inflate(buf: &[u8]) -> Result<Vec<u8>> {
    let attempts: [DecompressFn; 2] = [
        |b| Box::new(flate2::read::GzDecoder::new(b)),
        |b| Box::new(flate2::read::ZlibDecoder::new(b)),
    ];
    let order: [usize; 2] = if buf.starts_with(&[0x1f, 0x8b]) {
        [0, 1]
    } else {
        [1, 0]
    };
    let mut last_err = None;
    for idx in order {
        let mut out = Vec::new();
        match attempts[idx](buf).read_to_end(&mut out) {
            Ok(_) if !out.is_empty() => return Ok(out),
            Ok(_) => last_err = Some(anyhow!("empty compressed stream")),
            Err(e) => last_err = Some(anyhow!(e)),
        }
    }
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(buf).read_to_end(&mut out)?;
    if out.is_empty() {
        return Err(last_err.unwrap_or_else(|| anyhow!("empty inflate output")));
    }
    Ok(out)
}

/// AES-GCM with 16-byte nonce: `iv(16) | aad(16) | ciphertext||tag(16)`.
fn gcm_decrypt(key: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 48 {
        bail!("aes-gcm blob too short ({})", blob.len());
    }
    let nonce = aes_gcm::Nonce::<U16>::from_slice(&blob[..16]);
    let payload = Payload {
        msg: &blob[32..],
        aad: &blob[16..32],
    };
    let plain = match key.len() {
        32 => AesGcm::<Aes256, U16>::new_from_slice(key)
            .map_err(|e| anyhow!("aes key: {e}"))?
            .decrypt(nonce, payload)
            .map_err(|e| anyhow!("aes-gcm: {e}"))?,
        16 => AesGcm::<Aes128, U16>::new_from_slice(key)
            .map_err(|e| anyhow!("aes key: {e}"))?
            .decrypt(nonce, payload)
            .map_err(|e| anyhow!("aes-gcm: {e}"))?,
        n => bail!("unsupported AES key length {n}"),
    };
    Ok(plain)
}

pub fn decrypt_payload(cipher_key: Option<&[u8]>, encrypt: u8, data: &str) -> Result<Vec<u8>> {
    match encrypt {
        0 => Ok(data.as_bytes().to_vec()),
        1 => inflate(&B64.decode(data)?),
        2 => {
            let key = cipher_key.ok_or_else(|| anyhow!("missing ws cipher key"))?;
            let raw = B64.decode(percent_decode(data))?;
            inflate(&gcm_decrypt(key, &raw)?)
        }
        3 => {
            let key = cipher_key.ok_or_else(|| anyhow!("missing ws cipher key"))?;
            let raw = B64.decode(percent_decode(data))?;
            gcm_decrypt(key, &raw)
        }
        other => bail!("unsupported encrypt scheme {other}"),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn build_ws_url(sess: &Session, endpoint_idx: usize) -> String {
    let endpoints = &sess.domains.ws;
    let base = if endpoints.is_empty() {
        "wss://ws1-msg.chat.zalo.me".to_string()
    } else {
        endpoints[endpoint_idx % endpoints.len()].clone()
    };
    let (host, query) = match base.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (base.as_str(), None),
    };
    let after_scheme = host.split_once("://").map(|(_, rest)| rest).unwrap_or(host);
    let has_path = after_scheme.contains('/');
    let mut url = format!("{host}{}", if has_path { "" } else { "/" });
    url.push('?');
    if let Some(q) = query {
        url.push_str(q);
        url.push('&');
    }
    url.push_str(&format!("{}&t={}", sess.common_params(), now_ms()));
    url
}

fn queues_from_auth(auth: &Value) -> Vec<(u16, u8)> {
    let mut out = Vec::new();
    for key in ["ctrl_qCmds", "qCmds"] {
        if let Some(arr) = auth.pointer(&format!("/data/{key}")).and_then(Value::as_array) {
            for q in arr {
                let cmd = q.get("cmd").and_then(Value::as_u64);
                let sub = q.get("subCmd").and_then(Value::as_u64).unwrap_or(0);
                if let Some(c) = cmd {
                    let pair = (c as u16, sub as u8);
                    if !out.contains(&pair) {
                        out.push(pair);
                    }
                }
            }
        }
    }
    out
}

fn pull_frame(cmd: u16, sub_cmd: u8, req_id: &str) -> Vec<u8> {
    let data = json!({
        "first": true,
        "reqId": req_id,
        "lastId": 1,
        "preIds": [],
    });
    encode_frame(cmd, sub_cmd, 1, Some(&data))
}

fn pull_next_frame(cmd: u16, sub_cmd: u8, req_id: &str, last_id: Value, pre_ids: Value) -> Vec<u8> {
    let data = json!({
        "first": false,
        "reqId": req_id,
        "lastId": last_id,
        "preIds": pre_ids,
    });
    encode_frame(cmd, sub_cmd, 1, Some(&data))
}

/// Spawn background `zpw_ws` loop. Dropping the returned receiver terminates the loop.
pub fn spawn(sess: Session) -> mpsc::UnboundedReceiver<WsSignal> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut backoff = 2u64;
        let mut endpoint_idx = 0usize;
        loop {
            let _ = tx.send(WsSignal::Status(if endpoint_idx == 0 {
                WsStatus::Connecting
            } else {
                WsStatus::Reconnecting
            }));
            match run_once(&sess, &tx, endpoint_idx).await {
                Ok(WsRunOutcome::Disconnected) => {
                    backoff = 2;
                }
                Ok(WsRunOutcome::Expired { code, message }) => {
                    let _ = tx.send(WsSignal::Expired { code, message });
                    break;
                }
                Ok(WsRunOutcome::UntrustedHost) => {
                    break;
                }
                Err(_) => {}
            }
            if tx.is_closed() {
                break;
            }
            let _ = tx.send(WsSignal::Status(WsStatus::Reconnecting));
            endpoint_idx = endpoint_idx.wrapping_add(1);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    });
    rx
}

async fn run_once(
    sess: &Session,
    tx: &mpsc::UnboundedSender<WsSignal>,
    endpoint_idx: usize,
) -> Result<WsRunOutcome> {
    let url = build_ws_url(sess, endpoint_idx);
    if !crate::session::is_allowed_zalo_ws_url(&url) {
        return Ok(WsRunOutcome::UntrustedHost);
    }
    let mut req = url.clone().into_client_request()?;
    {
        let h = req.headers_mut();
        if let Some(sek) = sess
            .zpw_sek
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "EXPIRED")
        {
            h.insert(
                COOKIE,
                HeaderValue::from_str(&format!("zpw_sek={sek}; _zlang=vi"))?,
            );
            h.insert("zpw_sek", HeaderValue::from_str(sek)?);
        }
        h.insert(ORIGIN, HeaderValue::from_static("https://chat.zalo.me"));
        h.insert(USER_AGENT, HeaderValue::from_static(BROWSER_UA));
    }

    if sess.relay.enabled {
        let parsed = reqwest::Url::parse(&url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("Thiếu hostname WebSocket"))?
            .to_string();
        let port = parsed.port_or_known_default().unwrap_or(443);
        let tls_stream = sess.relay.connect_tls(&host, port).await?;
        let (ws, _resp) = tokio_tungstenite::client_async(req, tls_stream).await?;
        ws_loop(ws, tx).await
    } else {
        let (ws, _resp) = connect_async(req).await?;
        ws_loop(ws, tx).await
    }
}

fn unwrap_data_str(mut v: Value) -> Value {
    if let Some(d_str) = v.get("data").and_then(Value::as_str) {
        let trimmed = d_str.trim();
        if (trimmed.starts_with('{') && trimmed.ends_with('}'))
            || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        {
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                v["data"] = parsed;
            }
        }
    }
    v
}

async fn ws_loop<S>(
    mut ws: WebSocketStream<S>,
    tx: &mpsc::UnboundedSender<WsSignal>,
) -> Result<WsRunOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut cipher_key: Option<Vec<u8>> = None;
    let mut authed = false;
    let mut req_id = 0u64;

    let mut ping = tokio::time::interval(Duration::from_secs(PING_SECS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if authed {
                    let body = json!({ "eventId": now_ms() });
                    let f = encode_frame(CMD_PING, SUB_KEEP_CONNECTION, 1, Some(&body));
                    tokio::time::timeout(
                        Duration::from_secs(WS_WRITE_TIMEOUT_SECS),
                        ws.send(Message::Binary(f)),
                    )
                    .await
                    .map_err(|_| anyhow!("ws ping write timeout"))??;
                }
            }
            incoming = ws.next() => {
                let Some(incoming) = incoming else {
                    return Ok(WsRunOutcome::Disconnected);
                };
                let incoming = incoming?;
                let buf = match incoming {
                    Message::Binary(b) => b,
                    Message::Ping(p) => {
                        ws.send(Message::Pong(p)).await?;
                        continue;
                    }
                    Message::Close(_) => return Ok(WsRunOutcome::Disconnected),
                    _ => continue,
                };

                let Some((version, cmd, sub_cmd, envelope)) = decode_frame(&buf) else {
                    continue;
                };
                let encrypt = envelope.get("encrypt").and_then(Value::as_u64).unwrap_or(0) as u8;

                if cmd == CMD_AUTHEN {
                    if let Some(key) = envelope.get("key").and_then(Value::as_str).filter(|k| !k.is_empty()) {
                        cipher_key = Some(B64.decode(key)?);
                    }
                    let raw = envelope.get("data").and_then(Value::as_str).unwrap_or_default();
                    let plain = decrypt_payload(cipher_key.as_deref(), encrypt, raw)?;
                    let auth: Value = unwrap_data_str(serde_json::from_slice(&plain)?);
                    let code = auth.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                    if code != 0 {
                        if crate::session::is_session_expired_code(code) || code == 401 || code == 403 {
                            let msg = auth
                                .get("error_message")
                                .and_then(Value::as_str)
                                .unwrap_or("Phiên đăng nhập đã hết hạn")
                                .to_string();
                            return Ok(WsRunOutcome::Expired { code, message: msg });
                        }
                        bail!("ws auth rejected: {}", truncate(&auth.to_string(), 200));
                    }
                    authed = true;
                    let _ = tx.send(WsSignal::Status(WsStatus::Connected));
                    let _ = tx.send(WsSignal::Event(WsEvent { cmd, sub_cmd, version, payload: auth.clone() }));

                    for (qcmd, qsub) in queues_from_auth(&auth) {
                        let id = format!("req_{req_id}");
                        req_id += 1;
                        ws.send(Message::Binary(pull_frame(qcmd, qsub, &id))).await?;
                    }
                    continue;
                }

                let raw_payload = if encrypt == 0 {
                    envelope
                } else {
                    match envelope.get("data").and_then(Value::as_str) {
                        Some(d) => decrypt_payload(cipher_key.as_deref(), encrypt, d)
                            .ok()
                            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                            .unwrap_or(envelope),
                        None => envelope,
                    }
                };
                let payload = unwrap_data_str(raw_payload);

                if cmd == CMD_HAS_MSG {
                    if let (Some(qid), Some(ids)) = (
                        payload.pointer("/data/queueId").cloned(),
                        payload
                            .pointer("/data/ids")
                            .or_else(|| payload.pointer("/data/msgIds"))
                            .cloned(),
                    ) {
                        let id = format!("req_{req_id}");
                        req_id += 1;
                        let get_req = json!({
                            "queueId": qid,
                            "ids": ids,
                            "lastId": "1",
                            "reqId": id,
                        });
                        let _ = ws.send(Message::Binary(encode_frame(CMD_GET_MSG, 0, 1, Some(&get_req)))).await;
                    }
                }

                if payload.pointer("/data/more").and_then(Value::as_i64) == Some(1) {
                    let qkey = format!("{cmd}_{sub_cmd}");
                    let last_id = payload
                        .pointer("/data/lastActionId")
                        .or_else(|| payload.pointer(&format!("/data/queueStatus/{qkey}/lastId")))
                        .cloned()
                        .unwrap_or(json!(1));
                    let pre_ids = payload
                        .pointer(&format!("/data/queueStatus/{qkey}/ids"))
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    let id = format!("req_{req_id}");
                    req_id += 1;
                    let _ = ws.send(Message::Binary(pull_next_frame(cmd, sub_cmd, &id, last_id, pre_ids))).await;
                }

                if tx.send(WsSignal::Event(WsEvent { cmd, sub_cmd, version, payload })).is_err() {
                    return Ok(WsRunOutcome::Disconnected);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let body = json!({ "a": 1, "b": "x" });
        let f = encode_frame(501, 0, 1, Some(&body));
        assert_eq!(f[0], 1);
        assert_eq!(u16::from_le_bytes([f[1], f[2]]), 501);
        let (v, cmd, sub, val) = decode_frame(&f).unwrap();
        assert_eq!((v, cmd, sub), (1, 501, 0));
        assert_eq!(val, body);
    }

    #[test]
    fn gcm_rejects_short_blob() {
        assert!(gcm_decrypt(&[0u8; 32], &[0u8; 16]).is_err());
    }

    #[test]
    fn unwraps_stringified_ws_data_json() {
        let v = json!({
            "data": "{\"queueId\":\"q1\",\"ids\":[101,102]}"
        });
        let unwrapped = unwrap_data_str(v);
        assert_eq!(unwrapped.pointer("/data/queueId").unwrap(), "q1");
        assert_eq!(unwrapped.pointer("/data/ids/0").unwrap(), 101);

        let arr = json!({ "data": "[{\"msgId\":\"m1\"}]" });
        assert_eq!(unwrap_data_str(arr).pointer("/data/0/msgId").unwrap(), "m1");

        let plain = json!({ "data": "not-json" });
        assert_eq!(unwrap_data_str(plain).get("data").unwrap(), "not-json");
    }

    #[test]
    fn decode_frame_rejects_short_or_corrupt_json_buffers() {
        assert!(decode_frame(&[1, 2, 3]).is_none());
        assert!(decode_frame(&[1, 1, 0, 0, b'{', b'b']).is_none());
        let (_, _, _, empty_val) = decode_frame(&[1, 2, 0, 1]).unwrap();
        assert_eq!(empty_val, Value::Null);
    }

    #[test]
    fn decrypt_payload_handles_plain_compressed_and_missing_key_errors() {
        assert_eq!(decrypt_payload(None, 0, "hello").unwrap(), b"hello");

        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, b"{\"ok\":true}").unwrap();
        let compressed = enc.finish().unwrap();
        let b64 = B64.encode(compressed);
        assert_eq!(decrypt_payload(None, 1, &b64).unwrap(), b"{\"ok\":true}");

        assert!(decrypt_payload(None, 2, "abc").is_err());
        assert!(decrypt_payload(None, 3, "abc").is_err());
        assert!(decrypt_payload(None, 99, "abc").is_err());
    }

    #[test]
    fn queues_from_auth_deduplicates_ctrl_and_chat_queues() {
        let auth = json!({
            "data": {
                "ctrl_qCmds": [
                    { "cmd": 603, "subCmd": 0 },
                    { "cmd": 603, "subCmd": 0 }
                ],
                "qCmds": [
                    { "cmd": 510, "subCmd": 1 },
                    { "cmd": 511, "subCmd": 0 },
                    { "cmd": 603, "subCmd": 0 }
                ]
            }
        });
        assert_eq!(queues_from_auth(&auth), vec![(603, 0), (510, 1), (511, 0)]);
    }
}
