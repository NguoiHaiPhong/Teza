//! Opt-in Local Automation HTTP/1.1 API server (Item #3).
//!
//! Security-by-default architecture:
//! - Disabled by default (`enabled: false`), persisted atomically in `<data_dir>/local_api.json`.
//! - Binds **STRICTLY** to `127.0.0.1:<port>` (never `0.0.0.0` or wildcard interfaces).
//! - Pure-Tokio minimal HTTP/1.1 handler with strict DoS caps (8 KiB headers, 64 KiB body,
//!   5s total read timeout, `Connection: close` per request, bounded concurrency).
//! - DNS rebinding protection (`Host` allowlist for loopback/localhost only).
//! - Browser CSRF protection (blocks any `Origin` header and any `Sec-Fetch-Site` != `none`;
//!   never emits CORS headers).
//! - Mandatory `Authorization: Bearer <token>` on ALL endpoints (including `/v1/health`).
//! - Fine-grained scopes (`read_threads`, `send`, `schedule`), `allowed_thread_ids` enforcement,
//!   per-client sliding-window rate limiting + global anti-spam send spacing (`429 Retry-After`),
//!   24h `Idempotency-Key` replay cache, and persistent audit logging.

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex, Semaphore};
use tokio::task::JoinHandle;

use crate::scheduler::{is_expired_err, SchedulerHandle};
use zalo_core::chat::messaging;
use zalo_core::chat::parser::format_thread_preview;
use zalo_core::chat::{strip_vi_accents, FolderFilter, ThreadKind};
use zalo_core::data_dir::{write_private_atomic, StoragePaths};
use zalo_core::session::Session;
use zalo_core::store::{ApiClientDto, MessageStore, NewScheduledMessageInput};

pub const DEFAULT_LOCAL_API_PORT: u16 = 17_820;
pub const MIN_LOCAL_API_PORT: u16 = 1_024;
pub const MAX_LOCAL_API_PORT: u16 = 65_535;
pub const DEFAULT_MIN_SEND_INTERVAL_MS: u64 = 1_500;
pub const MIN_SEND_INTERVAL_MS_FLOOR: u64 = 200;
pub const MAX_SEND_INTERVAL_MS_CEIL: u64 = 60_000;

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_HEADER_COUNT: usize = 64;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONCURRENT_CONNECTIONS: usize = 32;
const MAX_MESSAGE_TEXT_CHARS: usize = 4_000;
const MAX_THREAD_ID_CHARS: usize = 128;
/// Cap raw client idempotency keys at 118 UTF-8 bytes so prepending `"schedule:"` (+9B)
/// or `"send:"` (+5B) stays strictly within `store::put_api_idempotency`'s 128-byte limit.
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 118;
const MAX_BEARER_TOKEN_BYTES: usize = 512;
const IDEMPOTENCY_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

fn default_local_api_port() -> u16 {
    DEFAULT_LOCAL_API_PORT
}

fn default_min_send_interval_ms() -> u64 {
    DEFAULT_MIN_SEND_INTERVAL_MS
}

/// Persisted Local Automation API configuration (`<data_dir>/local_api.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_local_api_port")]
    pub port: u16,
    #[serde(default = "default_min_send_interval_ms")]
    pub min_send_interval_ms: u64,
}

impl Default for LocalApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_LOCAL_API_PORT,
            min_send_interval_ms: DEFAULT_MIN_SEND_INTERVAL_MS,
        }
    }
}

impl LocalApiConfig {
    pub fn config_path(paths: &StoragePaths) -> PathBuf {
        paths.root.join("local_api.json")
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.port < MIN_LOCAL_API_PORT {
            return Err(format!(
                "Cổng Local API phải nằm trong khoảng {MIN_LOCAL_API_PORT}..={MAX_LOCAL_API_PORT}"
            ));
        }
        if self.min_send_interval_ms < MIN_SEND_INTERVAL_MS_FLOOR
            || self.min_send_interval_ms > MAX_SEND_INTERVAL_MS_CEIL
        {
            return Err(format!(
                "Khoảng cách tối thiểu giữa các lần gửi phải từ {MIN_SEND_INTERVAL_MS_FLOOR} đến {MAX_SEND_INTERVAL_MS_CEIL} ms"
            ));
        }
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(mut cfg) = serde_json::from_str::<Self>(&raw) else {
            return Self::default();
        };
        if cfg.port < MIN_LOCAL_API_PORT {
            cfg.port = DEFAULT_LOCAL_API_PORT;
        }
        cfg.min_send_interval_ms = cfg
            .min_send_interval_ms
            .clamp(MIN_SEND_INTERVAL_MS_FLOOR, MAX_SEND_INTERVAL_MS_CEIL);
        cfg
    }

    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        write_private_atomic(path, &bytes)
    }
}

/// Live status snapshot returned to the frontend Settings UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalApiStatusDto {
    pub enabled: bool,
    pub running: bool,
    pub bind_addr: String,
    pub port: u16,
    pub active_clients: usize,
}

type EventEmitFn = dyn Fn(&str, serde_json::Value) + Send + Sync;

#[derive(Clone)]
struct EventSink {
    emit_fn: Arc<EventEmitFn>,
}

impl EventSink {
    fn from_app(app: AppHandle) -> Self {
        Self {
            emit_fn: Arc::new(move |event, payload| {
                let _ = app.emit(event, payload);
            }),
        }
    }

    #[cfg(test)]
    fn capture(buf: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>) -> Self {
        Self {
            emit_fn: Arc::new(move |event, payload| {
                if let Ok(mut guard) = buf.lock() {
                    guard.push((event.to_string(), payload));
                }
            }),
        }
    }

    fn emit_json<T: Serialize>(&self, event: &str, payload: &T) {
        if let Ok(val) = serde_json::to_value(payload) {
            (self.emit_fn)(event, val);
        }
    }
}

struct RunningServer {
    shutdown_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
    bound_addr: SocketAddr,
}

#[derive(Default)]
struct RateLimiterState {
    per_client: HashMap<String, VecDeque<Instant>>,
    last_send_at: Option<Instant>,
}

impl RateLimiterState {
    fn check_client_rate_limit(
        &mut self,
        client_id: &str,
        limit_per_min: u32,
        now: Instant,
    ) -> Result<(), u64> {
        let cap = limit_per_min.max(1) as usize;
        let queue = self.per_client.entry(client_id.to_string()).or_default();
        while let Some(&front) = queue.front() {
            if now.saturating_duration_since(front) >= RATE_LIMIT_WINDOW {
                queue.pop_front();
            } else {
                break;
            }
        }
        if queue.len() >= cap {
            let oldest = *queue.front().unwrap_or(&now);
            let elapsed = now.saturating_duration_since(oldest);
            let remaining = RATE_LIMIT_WINDOW.saturating_sub(elapsed);
            let retry_after_secs = remaining.as_millis().div_ceil(1_000).max(1) as u64;
            return Err(retry_after_secs);
        }
        queue.push_back(now);

        // Periodic hygiene so revoked client IDs don't accumulate indefinitely.
        if self.per_client.len() > 256 {
            self.per_client.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.saturating_duration_since(*t) < RATE_LIMIT_WINDOW)
            });
        }
        Ok(())
    }

    fn check_and_reserve_send_slot(
        &mut self,
        min_interval_ms: u64,
        now: Instant,
    ) -> Result<(), (u64, u64)> {
        if min_interval_ms == 0 {
            self.last_send_at = Some(now);
            return Ok(());
        }
        if let Some(prev) = self.last_send_at {
            let elapsed = now.saturating_duration_since(prev);
            let required = Duration::from_millis(min_interval_ms);
            if elapsed < required {
                let rem_ms = (required - elapsed).as_millis() as u64;
                let retry_after_secs = rem_ms.div_ceil(1_000).max(1);
                return Err((retry_after_secs, rem_ms));
            }
        }
        self.last_send_at = Some(now);
        Ok(())
    }
}

/// Manages lifecycle, Kill Switch, configuration persistence, and in-memory rate limiting
/// for the Local Automation HTTP/1.1 API server.
pub struct LocalApiManager {
    config_path: PathBuf,
    config: Arc<Mutex<LocalApiConfig>>,
    runtime: Arc<Mutex<Option<RunningServer>>>,
    rate_limiter: Arc<Mutex<RateLimiterState>>,
}

impl LocalApiManager {
    /// Construct a manager backed by `<data_dir>/local_api.json`.
    pub fn new(paths: &StoragePaths) -> Self {
        Self::from_config_path(LocalApiConfig::config_path(paths))
    }

    /// Construct a manager backed by an explicit config file path.
    pub fn from_config_path(config_path: PathBuf) -> Self {
        let loaded = LocalApiConfig::load_from_file(&config_path);
        Self {
            config_path,
            config: Arc::new(Mutex::new(loaded)),
            runtime: Arc::new(Mutex::new(None)),
            rate_limiter: Arc::new(Mutex::new(RateLimiterState::default())),
        }
    }

    pub async fn config(&self) -> LocalApiConfig {
        self.config.lock().await.clone()
    }

    /// Return current status including live bound address and count of enabled API clients.
    pub async fn status(&self, store_arc: &Arc<Mutex<MessageStore>>) -> LocalApiStatusDto {
        let cfg = self.config.lock().await.clone();
        let (running, bound_addr, port) = {
            let mut rt = self.runtime.lock().await;
            if let Some(srv) = rt.as_ref() {
                if srv.join_handle.is_finished() {
                    rt.take();
                    (false, format!("127.0.0.1:{}", cfg.port), cfg.port)
                } else {
                    (true, srv.bound_addr.to_string(), srv.bound_addr.port())
                }
            } else {
                (false, format!("127.0.0.1:{}", cfg.port), cfg.port)
            }
        };
        let active_clients = {
            let store = store_arc.lock().await;
            store
                .list_api_clients()
                .map(|clients| clients.into_iter().filter(|c| c.enabled).count())
                .unwrap_or(0)
        };
        LocalApiStatusDto {
            enabled: cfg.enabled,
            running,
            bind_addr: bound_addr,
            port,
            active_clients,
        }
    }

    /// Start the HTTP/1.1 server only if `config.enabled == true` (for app startup).
    pub async fn start_if_enabled(
        &self,
        app: AppHandle,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        let enabled = self.config.lock().await.enabled;
        if !enabled {
            return Ok(self.status(&store_arc).await);
        }
        self.start(app, sess_arc, store_arc, scheduler).await
    }

    /// Bind strictly on `127.0.0.1:<cfg.port>` and start accepting Local API requests.
    /// Stops any existing listener first.
    pub async fn start(
        &self,
        app: AppHandle,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        let cfg = self.config.lock().await.clone();
        cfg.validate()?;
        self.start_internal(
            cfg.port,
            EventSink::from_app(app),
            sess_arc,
            store_arc.clone(),
            scheduler,
        )
        .await?;
        Ok(self.status(&store_arc).await)
    }

    /// Immediately stop the running HTTP server via the watch shutdown channel and await
    /// listener drop so the port is released cleanly. Returns `true` if a server was stopped.
    pub async fn stop(&self) -> bool {
        let running = { self.runtime.lock().await.take() };
        let Some(mut srv) = running else {
            return false;
        };
        let _ = srv.shutdown_tx.send(true);
        if tokio::time::timeout(SHUTDOWN_JOIN_TIMEOUT, &mut srv.join_handle)
            .await
            .is_err()
        {
            srv.join_handle.abort();
            let _ = srv.join_handle.await;
        }
        true
    }

    /// Update configuration atomically, persisting to `local_api.json`, and apply the
    /// live start / restart / Kill Switch stop transition.
    pub async fn apply_config(
        &self,
        new_cfg: LocalApiConfig,
        app: AppHandle,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        new_cfg.validate()?;
        let prev_cfg = self.config.lock().await.clone();

        if !new_cfg.enabled {
            self.stop().await;
            new_cfg
                .save_to_file(&self.config_path)
                .map_err(|e| format!("Không lưu được cấu hình Local API: {e}"))?;
            *self.config.lock().await = new_cfg;
            return Ok(self.status(&store_arc).await);
        }

        let need_rebind = {
            let rt = self.runtime.lock().await;
            match rt.as_ref() {
                Some(srv) if !srv.join_handle.is_finished() => {
                    srv.bound_addr.port() != new_cfg.port
                }
                _ => true,
            }
        };

        // Update in-memory config first so any newly spawned server sees `min_send_interval_ms`.
        *self.config.lock().await = new_cfg.clone();

        if need_rebind {
            if let Err(bind_err) = self
                .start_internal(
                    new_cfg.port,
                    EventSink::from_app(app.clone()),
                    sess_arc.clone(),
                    store_arc.clone(),
                    scheduler.clone(),
                )
                .await
            {
                // Roll back in-memory config and attempt to restore previous listener if it was active.
                *self.config.lock().await = prev_cfg.clone();
                if prev_cfg.enabled {
                    let _ = self
                        .start_internal(prev_cfg.port, EventSink::from_app(app), sess_arc, store_arc, scheduler)
                        .await;
                }
                return Err(bind_err);
            }
        }

        if let Err(save_err) = new_cfg.save_to_file(&self.config_path) {
            self.stop().await;
            *self.config.lock().await = prev_cfg;
            return Err(format!("Không lưu được cấu hình Local API: {save_err}"));
        }

        Ok(self.status(&store_arc).await)
    }

    /// Kill Switch: immediately disables `enabled = false`, persists config, and closes listener.
    #[allow(dead_code)]
    pub async fn kill_switch(
        &self,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> Result<LocalApiStatusDto, String> {
        self.stop().await;
        let updated = {
            let mut cfg = self.config.lock().await;
            cfg.enabled = false;
            cfg.clone()
        };
        updated
            .save_to_file(&self.config_path)
            .map_err(|e| format!("Không lưu được cấu hình Local API: {e}"))?;
        Ok(self.status(store_arc).await)
    }

    async fn start_internal(
        &self,
        port: u16,
        sink: EventSink,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<SocketAddr, String> {
        self.stop().await;

        // STRICT loopback-only bind: 127.0.0.1 ONLY, NEVER 0.0.0.0!
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(bind_addr).await.map_err(|e| {
            format!("Không thể mở Local API trên {bind_addr}: {e}")
        })?;
        let actual_addr = listener.local_addr().map_err(|e| {
            format!("Không đọc được địa chỉ cục bộ của Local API: {e}")
        })?;
        debug_assert!(actual_addr.ip().is_loopback());

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let ctx = Arc::new(ServerContext {
            bound_port: actual_addr.port(),
            config: self.config.clone(),
            rate_limiter: self.rate_limiter.clone(),
            sess_arc,
            store_arc,
            scheduler,
            sink,
        });
        let conn_limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let (stream, peer) = match accepted {
                            Ok(pair) => pair,
                            Err(err) => {
                                eprintln!("[teza][local-api] accept error: {err}");
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                        };
                        if !peer.ip().is_loopback() {
                            drop(stream);
                            continue;
                        }
                        let Ok(permit) = conn_limit.clone().try_acquire_owned() else {
                            let mut busy_stream = stream;
                            let resp = HttpResponse::error(
                                503,
                                "server_busy",
                                "Too many concurrent local API connections",
                            );
                            let _ = write_http_response(&mut busy_stream, &resp).await;
                            continue;
                        };
                        let ctx_conn = ctx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            handle_connection(stream, ctx_conn).await;
                        });
                    }
                }
            }
            drop(listener);
        });

        *self.runtime.lock().await = Some(RunningServer {
            shutdown_tx,
            join_handle,
            bound_addr: actual_addr,
        });

        Ok(actual_addr)
    }
}

struct ServerContext {
    bound_port: u16,
    config: Arc<Mutex<LocalApiConfig>>,
    rate_limiter: Arc<Mutex<RateLimiterState>>,
    sess_arc: Arc<Mutex<Session>>,
    store_arc: Arc<Mutex<MessageStore>>,
    scheduler: SchedulerHandle,
    sink: EventSink,
}

// ============================================================================
// Pure-Rust Minimal HTTP/1.1 Parser, Security Validators & Response Writer
// ============================================================================

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    extra_headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::raw_json(status, value.to_string())
    }

    fn raw_json(status: u16, body: String) -> Self {
        Self {
            status,
            reason: status_reason(status),
            extra_headers: Vec::new(),
            body,
        }
    }

    fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            &serde_json::json!({
                "ok": false,
                "error": code,
                "message": message,
            }),
        )
    }

    fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    }
}

enum ReadRequestResult {
    Request(HttpRequest),
    Error(HttpResponse),
    EmptyClose,
}

/// DNS rebinding protection: allow ONLY exact loopback / localhost `Host` headers.
pub fn is_valid_local_host(host: &str, bound_port: u16) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    normalized == "127.0.0.1"
        || normalized == "localhost"
        || normalized == format!("127.0.0.1:{bound_port}")
        || normalized == format!("localhost:{bound_port}")
}

/// Browser CSRF protection: reject ANY `Origin` header and any `Sec-Fetch-Site` other than `none`.
pub fn has_forbidden_browser_headers(
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
) -> bool {
    if origin.is_some() {
        return true;
    }
    if let Some(sfs) = sec_fetch_site {
        if !sfs.trim().eq_ignore_ascii_case("none") {
            return true;
        }
    }
    false
}

/// Extract a Bearer token from an `Authorization` header value.
pub fn extract_bearer_token(auth_header: Option<&str>) -> Option<&str> {
    let raw = auth_header?.trim();
    if raw.len() < 8 {
        return None;
    }
    let (scheme, rest) = raw.split_at(7);
    if !scheme.eq_ignore_ascii_case("bearer ") {
        return None;
    }
    let token = rest.trim();
    if token.is_empty()
        || token.len() > MAX_BEARER_TOKEN_BYTES
        || token.bytes().any(|b| b <= 0x20 || b == 0x7f)
    {
        return None;
    }
    Some(token)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn read_http_request(stream: &mut TcpStream) -> ReadRequestResult {
    let deadline = Instant::now() + REQUEST_READ_TIMEOUT;
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];

    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return ReadRequestResult::Error(HttpResponse::error(
                431,
                "headers_too_large",
                "HTTP headers exceed 8 KiB limit",
            ));
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return ReadRequestResult::Error(HttpResponse::error(
                408,
                "request_timeout",
                "Request read timed out",
            ));
        };
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                if buf.is_empty() {
                    return ReadRequestResult::EmptyClose;
                }
                return ReadRequestResult::Error(HttpResponse::error(
                    400,
                    "incomplete_request",
                    "Connection closed before HTTP headers completed",
                ));
            }
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES + 4 {
                    return ReadRequestResult::Error(HttpResponse::error(
                        413,
                        "payload_too_large",
                        "HTTP request exceeds maximum allowed size",
                    ));
                }
            }
            Ok(Err(_)) => {
                return ReadRequestResult::EmptyClose;
            }
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::error(
                    408,
                    "request_timeout",
                    "Request read timed out",
                ));
            }
        }
    };

    if header_end + 4 > MAX_HEADER_BYTES {
        return ReadRequestResult::Error(HttpResponse::error(
            431,
            "headers_too_large",
            "HTTP headers exceed 8 KiB limit",
        ));
    }

    let head_str = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(_) => {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "invalid_encoding",
                "HTTP headers must be valid UTF-8",
            ));
        }
    };

    let mut lines = head_str.split("\r\n");
    let Some(request_line) = lines.next() else {
        return ReadRequestResult::Error(HttpResponse::error(
            400,
            "bad_request",
            "Missing HTTP request line",
        ));
    };

    let mut rl_parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        rl_parts.next(),
        rl_parts.next(),
        rl_parts.next(),
        rl_parts.next(),
    ) else {
        return ReadRequestResult::Error(HttpResponse::error(
            400,
            "bad_request_line",
            "Malformed HTTP request line",
        ));
    };

    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return ReadRequestResult::Error(HttpResponse::error(
            505,
            "http_version_not_supported",
            "Only HTTP/1.0 and HTTP/1.1 are supported",
        ));
    }

    if method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || !target.starts_with('/')
        || target.bytes().any(|b| b < 0x21 || b == 0x7f)
    {
        return ReadRequestResult::Error(HttpResponse::error(
            400,
            "invalid_target",
            "Invalid HTTP method or request target",
        ));
    }

    let method = method.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let mut headers: HashMap<String, String> = HashMap::new();
    let mut header_count = 0usize;
    for line in lines {
        header_count += 1;
        if header_count > MAX_HEADER_COUNT {
            return ReadRequestResult::Error(HttpResponse::error(
                431,
                "too_many_headers",
                "Too many HTTP headers",
            ));
        }
        let Some((raw_name, raw_val)) = line.split_once(':') else {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "malformed_header",
                "Malformed HTTP header line",
            ));
        };
        if raw_name.is_empty()
            || !raw_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "invalid_header_name",
                "Invalid HTTP header name",
            ));
        }
        let name = raw_name.to_ascii_lowercase();
        let val = raw_val.trim_matches(|c| c == ' ' || c == '\t');
        if val.bytes().any(|b| b < 0x20 && b != b'\t' || b == 0x7f) {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "invalid_header_value",
                "Invalid control characters in HTTP header value",
            ));
        }

        if name == "transfer-encoding" {
            return ReadRequestResult::Error(HttpResponse::error(
                501,
                "transfer_encoding_unsupported",
                "Transfer-Encoding is not supported; send explicit Content-Length",
            ));
        }

        if (name == "host" || name == "content-length") && headers.contains_key(&name) {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "duplicate_header",
                "Duplicate Host or Content-Length header",
            ));
        }
        headers.insert(name, val.to_string());
    }

    let content_length = match headers.get("content-length") {
        Some(raw_len) => match raw_len.parse::<usize>() {
            Ok(n) if n <= MAX_BODY_BYTES => n,
            Ok(_) => {
                return ReadRequestResult::Error(HttpResponse::error(
                    413,
                    "payload_too_large",
                    "Request body exceeds 64 KiB limit",
                ));
            }
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::error(
                    400,
                    "invalid_content_length",
                    "Invalid Content-Length header",
                ));
            }
        },
        None => 0,
    };

    let mut body = buf.split_off(header_end + 4);
    if body.len() > content_length {
        body.truncate(content_length);
    }

    while body.len() < content_length {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return ReadRequestResult::Error(HttpResponse::error(
                408,
                "request_timeout",
                "Request body read timed out",
            ));
        };
        let to_read = (content_length - body.len()).min(chunk.len());
        match tokio::time::timeout(remaining, stream.read(&mut chunk[..to_read])).await {
            Ok(Ok(0)) => {
                return ReadRequestResult::Error(HttpResponse::error(
                    400,
                    "incomplete_body",
                    "Connection closed before full request body was received",
                ));
            }
            Ok(Ok(n)) => body.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => return ReadRequestResult::EmptyClose,
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::error(
                    408,
                    "request_timeout",
                    "Request body read timed out",
                ));
            }
        }
    }

    ReadRequestResult::Request(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

async fn write_http_response(stream: &mut TcpStream, resp: &HttpResponse) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n",
        resp.status,
        resp.reason,
        resp.body.len()
    );
    for (k, v) in &resp.extra_headers {
        let clean_k: String = k.chars().filter(|&c| c != '\r' && c != '\n').collect();
        let clean_v: String = v.chars().filter(|&c| c != '\r' && c != '\n').collect();
        head.push_str(&clean_k);
        head.push_str(": ");
        head.push_str(&clean_v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");

    let write_fut = async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(resp.body.as_bytes()).await?;
        stream.flush().await?;
        let _ = stream.shutdown().await;
        Ok::<(), std::io::Error>(())
    };
    match tokio::time::timeout(RESPONSE_WRITE_TIMEOUT, write_fut).await {
        Ok(res) => res,
        Err(_) => Ok(()),
    }
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((hi << 4) | lo);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_query_params(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if query.is_empty() {
        return map;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_k, raw_v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let k = percent_decode(raw_k);
        let v = percent_decode(raw_v);
        if !k.is_empty() {
            map.insert(k, v);
        }
    }
    map
}

// ============================================================================
// Request Handling & Endpoint Implementations
// ============================================================================

async fn handle_connection(mut stream: TcpStream, ctx: Arc<ServerContext>) {
    let req = match read_http_request(&mut stream).await {
        ReadRequestResult::Request(r) => r,
        ReadRequestResult::Error(resp) => {
            let _ = write_http_response(&mut stream, &resp).await;
            return;
        }
        ReadRequestResult::EmptyClose => return,
    };

    let resp = route_and_handle_request(&req, &ctx).await;
    let _ = write_http_response(&mut stream, &resp).await;
}

async fn route_and_handle_request(req: &HttpRequest, ctx: &ServerContext) -> HttpResponse {
    // 1. DNS Rebinding Protection (mandatory Host header validation)
    let Some(host) = req.headers.get("host").map(String::as_str) else {
        return HttpResponse::error(403, "forbidden_host", "Missing Host header");
    };
    if !is_valid_local_host(host, ctx.bound_port) {
        return HttpResponse::error(
            403,
            "forbidden_host",
            "Forbidden Host header (loopback only)",
        );
    }

    // 2. Browser CSRF Protection (block Origin, cross-site Sec-Fetch-Site, and OPTIONS preflight)
    if has_forbidden_browser_headers(
        req.headers.get("origin").map(String::as_str),
        req.headers.get("sec-fetch-site").map(String::as_str),
    ) || req.method == "OPTIONS"
    {
        return HttpResponse::error(
            403,
            "forbidden_origin",
            "Browser cross-origin requests are blocked",
        );
    }

    let now_ms = now_epoch_ms();

    // 3. Bearer Token Authentication (required on ALL endpoints including /v1/health)
    let Some(raw_token) = extract_bearer_token(req.headers.get("authorization").map(String::as_str))
    else {
        record_audit(
            ctx,
            "",
            "anonymous",
            &req.method,
            &req.path,
            "",
            401,
            "unauthorized",
            "Missing or malformed Authorization Bearer token",
            now_ms,
        )
        .await;
        return HttpResponse::error(401, "unauthorized", "Missing or invalid Bearer token")
            .with_header("WWW-Authenticate", "Bearer realm=\"Teza Local API\"");
    };

    let verified_client = {
        let store = ctx.store_arc.lock().await;
        store.verify_api_token(raw_token, now_ms).ok().flatten()
    };

    let Some(client) = verified_client else {
        record_audit(
            ctx,
            "",
            "unknown",
            &req.method,
            &req.path,
            "",
            401,
            "unauthorized",
            "Invalid or disabled API token",
            now_ms,
        )
        .await;
        return HttpResponse::error(401, "unauthorized", "Invalid or disabled API token")
            .with_header("WWW-Authenticate", "Bearer realm=\"Teza Local API\"");
    };

    // 4. Per-client sliding 60-second rate limiter
    {
        let mut limiter = ctx.rate_limiter.lock().await;
        if let Err(retry_after_secs) =
            limiter.check_client_rate_limit(&client.id, client.rate_limit_per_min, Instant::now())
        {
            drop(limiter);
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                429,
                "rate_limited",
                &format!("Exceeded {} req/min", client.rate_limit_per_min),
                now_ms,
            )
            .await;
            return HttpResponse::json(
                429,
                &serde_json::json!({
                    "ok": false,
                    "error": "rate_limited",
                    "retry_after_secs": retry_after_secs,
                    "message": "Rate limit exceeded",
                }),
            )
            .with_header("Retry-After", retry_after_secs.to_string());
        }
    }

    // 5. Route dispatch
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/health") => handle_health(req, &client, ctx).await,
        ("GET", "/v1/threads") => handle_list_threads(req, &client, ctx).await,
        ("GET", "/v1/schedules") => handle_list_schedules(req, &client, ctx).await,
        ("POST", "/v1/messages/send") => handle_send_message(req, &client, ctx).await,
        ("POST", "/v1/messages/schedule") => handle_schedule_message(req, &client, ctx).await,
        (_, "/v1/health" | "/v1/threads" | "/v1/schedules" | "/v1/messages/send" | "/v1/messages/schedule") => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                405,
                "method_not_allowed",
                "HTTP method not allowed for this endpoint",
                now_ms,
            )
            .await;
            HttpResponse::error(405, "method_not_allowed", "Method not allowed")
        }
        _ => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                404,
                "not_found",
                "Unknown API endpoint",
                now_ms,
            )
            .await;
            HttpResponse::error(404, "not_found", "Endpoint not found")
        }
    }
}

fn client_has_scope(client: &ApiClientDto, required_scope: &str) -> bool {
    client
        .scopes
        .iter()
        .any(|s| s.trim().eq_ignore_ascii_case(required_scope))
}

fn is_thread_allowed(client: &ApiClientDto, thread_id: &str) -> bool {
    client.allowed_thread_ids.is_empty()
        || client
            .allowed_thread_ids
            .iter()
            .any(|allowed| allowed == thread_id)
}

fn resolve_idempotency_key(
    header_val: Option<&str>,
    body_val: Option<&str>,
) -> Result<Option<String>, &'static str> {
    let h = header_val.map(str::trim).filter(|s| !s.is_empty());
    let b = body_val.map(str::trim).filter(|s| !s.is_empty());
    let key = match (h, b) {
        (Some(hk), Some(bk)) if hk != bk => {
            return Err("Idempotency-Key header does not match body idempotency_key");
        }
        (Some(hk), _) => hk,
        (None, Some(bk)) => bk,
        (None, None) => return Ok(None),
    };
    if key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        return Err("Idempotency key exceeds 118 bytes");
    }
    if key.chars().any(|c| c.is_control()) {
        return Err("Idempotency key contains invalid control characters");
    }
    Ok(Some(key.to_string()))
}

fn parse_explicit_thread_kind(raw: &str) -> Result<ThreadKind, &'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "direct" => Ok(ThreadKind::Direct),
        "group" => Ok(ThreadKind::Group),
        "cloud" => Ok(ThreadKind::Cloud),
        _ => Err("Invalid kind; expected 'direct', 'group', or 'cloud'"),
    }
}

fn thread_kind_str(kind: ThreadKind) -> &'static str {
    match kind {
        ThreadKind::Direct => "direct",
        ThreadKind::Group => "group",
        ThreadKind::Cloud => "cloud",
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_audit(
    ctx: &ServerContext,
    client_id: &str,
    client_name: &str,
    method: &str,
    path: &str,
    target_thread_id: &str,
    status_code: u16,
    outcome: &str,
    detail: &str,
    now_ms: i64,
) {
    let logged = {
        let store = ctx.store_arc.lock().await;
        store
            .append_api_audit_log(
                client_id,
                client_name,
                method,
                path,
                target_thread_id,
                status_code,
                outcome,
                detail,
                now_ms,
            )
            .ok()
    };
    if let Some(entry) = logged {
        ctx.sink.emit_json("core://local-api-audit", &entry);
    }
}

async fn handle_health(
    req: &HttpRequest,
    client: &ApiClientDto,
    ctx: &ServerContext,
) -> HttpResponse {
    let authenticated = {
        let sess = ctx.sess_arc.lock().await;
        sess.is_authenticated()
    };
    let now_ms = now_epoch_ms();
    record_audit(
        ctx,
        &client.id,
        &client.name,
        &req.method,
        &req.path,
        "",
        200,
        "ok",
        "",
        now_ms,
    )
    .await;
    HttpResponse::json(
        200,
        &serde_json::json!({
            "ok": true,
            "authenticated": authenticated,
            "version": env!("CARGO_PKG_VERSION"),
            "server_time_ms": now_ms,
        }),
    )
}

async fn handle_list_threads(
    req: &HttpRequest,
    client: &ApiClientDto,
    ctx: &ServerContext,
) -> HttpResponse {
    let now_ms = now_epoch_ms();
    if !client_has_scope(client, "read_threads") {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_scope",
            "Missing required scope: read_threads",
            now_ms,
        )
        .await;
        return HttpResponse::error(403, "forbidden_scope", "Missing required scope: read_threads");
    }

    let params = parse_query_params(&req.query);
    let folder = match params.get("kind").map(|s| s.trim().to_ascii_lowercase()) {
        None => FolderFilter::All,
        Some(ref s) if s.is_empty() || s == "all" => FolderFilter::All,
        Some(ref s) if s == "direct" => FolderFilter::Direct,
        Some(ref s) if s == "group" => FolderFilter::Group,
        Some(ref s) if s == "cloud" => FolderFilter::Cloud,
        Some(ref s) if s == "unread" => FolderFilter::Unread,
        Some(_) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                400,
                "invalid_query",
                "Invalid kind query parameter",
                now_ms,
            )
            .await;
            return HttpResponse::error(
                400,
                "invalid_query",
                "Invalid kind query parameter (expected direct, group, cloud, unread, or all)",
            );
        }
    };

    let limit = match params.get("limit").map(|s| s.trim()) {
        None | Some("") => 50usize,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(200),
            _ => {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    "",
                    400,
                    "invalid_query",
                    "Invalid limit query parameter",
                    now_ms,
                )
                .await;
                return HttpResponse::error(
                    400,
                    "invalid_query",
                    "Invalid limit parameter (must be integer >= 1)",
                );
            }
        },
    };

    let query_norm = params
        .get("q")
        .map(|s| strip_vi_accents(s.trim()).to_lowercase())
        .filter(|s| !s.is_empty());

    let raw_threads = {
        let store = ctx.store_arc.lock().await;
        match store.list_threads(folder) {
            Ok(list) => list,
            Err(err) => {
                drop(store);
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    "",
                    500,
                    "store_error",
                    &err.to_string(),
                    now_ms,
                )
                .await;
                return HttpResponse::error(500, "store_error", "Failed to load threads");
            }
        }
    };

    let filtered: Vec<serde_json::Value> = raw_threads
        .into_iter()
        .filter(|t| is_thread_allowed(client, &t.id))
        .filter(|t| match query_norm.as_deref() {
            None => true,
            Some(q) => {
                strip_vi_accents(&t.title).to_lowercase().contains(q)
                    || t.id.to_lowercase().contains(q)
            }
        })
        .take(limit)
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "title": t.title,
                "kind": thread_kind_str(t.kind),
                "unread_count": t.unread_count,
                "last_ts": t.last_ts,
            })
        })
        .collect();

    record_audit(
        ctx,
        &client.id,
        &client.name,
        &req.method,
        &req.path,
        "",
        200,
        "ok",
        &format!("returned {} threads", filtered.len()),
        now_ms,
    )
    .await;

    HttpResponse::json(
        200,
        &serde_json::json!({
            "ok": true,
            "threads": filtered,
        }),
    )
}

async fn handle_list_schedules(
    req: &HttpRequest,
    client: &ApiClientDto,
    ctx: &ServerContext,
) -> HttpResponse {
    let now_ms = now_epoch_ms();
    if !client_has_scope(client, "read_threads") {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_scope",
            "Missing required scope: read_threads",
            now_ms,
        )
        .await;
        return HttpResponse::error(403, "forbidden_scope", "Missing required scope: read_threads");
    }

    let params = parse_query_params(&req.query);
    let status_filter = match params.get("status").map(|s| s.trim().to_ascii_lowercase()) {
        None => None,
        Some(ref s) if s.is_empty() || s == "all" => None,
        Some(s) => {
            let valid = s.split(',').all(|part| {
                matches!(
                    part.trim(),
                    "pending" | "sending" | "sent" | "failed" | "cancelled" | "missed"
                )
            });
            if valid {
                Some(s)
            } else {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    "",
                    400,
                    "invalid_query",
                    "Invalid status query parameter",
                    now_ms,
                )
                .await;
                return HttpResponse::error(
                    400,
                    "invalid_query",
                    "Invalid status parameter (expected pending, sending, sent, failed, cancelled, or missed)",
                );
            }
        }
    };

    let limit = match params.get("limit").map(|s| s.trim()) {
        None | Some("") => 100usize,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(500),
            _ => {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    "",
                    400,
                    "invalid_query",
                    "Invalid limit query parameter",
                    now_ms,
                )
                .await;
                return HttpResponse::error(
                    400,
                    "invalid_query",
                    "Invalid limit parameter (must be integer >= 1)",
                );
            }
        },
    };

    let schedules = {
        let store = ctx.store_arc.lock().await;
        match store.list_scheduled_messages(status_filter.as_deref(), limit) {
            Ok(list) => list,
            Err(err) => {
                drop(store);
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    "",
                    500,
                    "store_error",
                    &err.to_string(),
                    now_ms,
                )
                .await;
                return HttpResponse::error(500, "store_error", "Failed to load schedules");
            }
        }
    };

    let filtered: Vec<_> = schedules
        .into_iter()
        .filter(|s| is_thread_allowed(client, &s.thread_id))
        .collect();

    record_audit(
        ctx,
        &client.id,
        &client.name,
        &req.method,
        &req.path,
        "",
        200,
        "ok",
        &format!("returned {} schedules", filtered.len()),
        now_ms,
    )
    .await;

    HttpResponse::json(
        200,
        &serde_json::json!({
            "ok": true,
            "schedules": filtered,
        }),
    )
}

#[derive(Debug, Deserialize)]
struct SendMessageRequestBody {
    thread_id: String,
    #[serde(default)]
    kind: Option<String>,
    text: String,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn handle_send_message(
    req: &HttpRequest,
    client: &ApiClientDto,
    ctx: &ServerContext,
) -> HttpResponse {
    let now_ms = now_epoch_ms();
    if !client_has_scope(client, "send") {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_scope",
            "Missing required scope: send",
            now_ms,
        )
        .await;
        return HttpResponse::error(403, "forbidden_scope", "Missing required scope: send");
    }

    let body: SendMessageRequestBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                400,
                "invalid_json",
                "Invalid JSON body for /v1/messages/send",
                now_ms,
            )
            .await;
            return HttpResponse::error(400, "invalid_json", "Invalid JSON request body");
        }
    };

    let thread_id = body.thread_id.trim();
    if thread_id.is_empty() || thread_id.chars().count() > MAX_THREAD_ID_CHARS {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_thread_id",
            "thread_id must be non-empty and <= 128 characters",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_thread_id",
            "thread_id must be non-empty and <= 128 characters",
        );
    }

    if !is_thread_allowed(client, thread_id) {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            403,
            "forbidden_thread",
            "Target thread_id is not in client allowlist",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            403,
            "forbidden_thread",
            "Target thread_id is not permitted for this API client",
        );
    }

    let trimmed_text = body.text.trim();
    let char_count = trimmed_text.chars().count();
    if trimmed_text.is_empty() || char_count > MAX_MESSAGE_TEXT_CHARS {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_text",
            "Message text must be 1..=4000 characters",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_text",
            "Message text must be non-empty and <= 4000 characters",
        );
    }

    let idem_key = match resolve_idempotency_key(
        req.headers.get("idempotency-key").map(String::as_str),
        body.idempotency_key.as_deref(),
    ) {
        Ok(k) => k.map(|raw| format!("send:{raw}")),
        Err(msg) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                400,
                "invalid_idempotency_key",
                msg,
                now_ms,
            )
            .await;
            return HttpResponse::error(400, "invalid_idempotency_key", msg);
        }
    };

    // Check 24h idempotency cache BEFORE anti-spam send throttling so safe retries succeed.
    if let Some(ref scoped_key) = idem_key {
        let cached = {
            let store = ctx.store_arc.lock().await;
            store
                .get_api_idempotency(&client.id, scoped_key, IDEMPOTENCY_TTL_MS, now_ms)
                .ok()
                .flatten()
        };
        if let Some((cached_status, cached_json)) = cached {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                cached_status,
                "idempotent_replay",
                "Returned cached idempotent response",
                now_ms,
            )
            .await;
            return HttpResponse::raw_json(cached_status, cached_json)
                .with_header("Idempotency-Replayed", "true");
        }
    }

    let kind = match body.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => match parse_explicit_thread_kind(raw) {
            Ok(k) => k,
            Err(msg) => {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    thread_id,
                    400,
                    "invalid_kind",
                    msg,
                    now_ms,
                )
                .await;
                return HttpResponse::error(400, "invalid_kind", msg);
            }
        },
        None => {
            let store = ctx.store_arc.lock().await;
            store
                .get_thread_kind(thread_id)
                .ok()
                .flatten()
                .unwrap_or(ThreadKind::Direct)
        }
    };

    let sess_snapshot = {
        let sess = ctx.sess_arc.lock().await;
        sess.clone_shallow()
    };
    if !sess_snapshot.is_authenticated() {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            503,
            "not_authenticated",
            "Teza account is not authenticated",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            503,
            "not_authenticated",
            "Teza account is not signed in",
        );
    }

    // Global anti-spam minimum send spacing check & atomic reservation.
    let min_interval_ms = ctx.config.lock().await.min_send_interval_ms;
    {
        let mut limiter = ctx.rate_limiter.lock().await;
        if let Err((retry_after_secs, retry_after_ms)) =
            limiter.check_and_reserve_send_slot(min_interval_ms, Instant::now())
        {
            drop(limiter);
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                429,
                "send_throttled",
                &format!("Global anti-spam spacing ({min_interval_ms}ms) exceeded"),
                now_ms,
            )
            .await;
            return HttpResponse::json(
                429,
                &serde_json::json!({
                    "ok": false,
                    "error": "send_throttled",
                    "retry_after_secs": retry_after_secs,
                    "retry_after_ms": retry_after_ms,
                    "message": "Global minimum send spacing active; please retry shortly",
                }),
            )
            .with_header("Retry-After", retry_after_secs.to_string());
        }
    }

    match messaging::send_text(&sess_snapshot, thread_id, kind, trimmed_text).await {
        Ok(msg) => {
            let finish_ms = now_epoch_ms();
            let resp_val = serde_json::json!({
                "ok": true,
                "msg_id": msg.msg_id,
                "thread_id": msg.thread_id,
                "ts": msg.ts,
            });
            let resp_body = resp_val.to_string();

            {
                let mut store = ctx.store_arc.lock().await;
                let _ = store.upsert_message(&msg);
                let preview = format_thread_preview(&msg.text, true, "", false);
                let _ = store.update_thread_preview_returning_created(
                    &msg.thread_id,
                    &preview,
                    msg.ts,
                    false,
                    true,
                );
                if let Some(ref scoped_key) = idem_key {
                    if let Err(err) = store.put_api_idempotency(
                        &client.id,
                        scoped_key,
                        200,
                        &resp_body,
                        finish_ms,
                    ) {
                        eprintln!("[teza][local-api] failed to persist send idempotency: {err}");
                    }
                }
            }

            ctx.sink.emit_json("core://message-new", &msg);
            ctx.sink.emit_json("core://threads-updated", &());
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                &msg.thread_id,
                200,
                "ok",
                &format!("sent msg_id={}", msg.msg_id),
                finish_ms,
            )
            .await;

            HttpResponse::raw_json(200, resp_body)
        }
        Err(err) => {
            let finish_ms = now_epoch_ms();
            let (status, code, detail) = if is_expired_err(&err) {
                (
                    503,
                    "session_expired",
                    "Session expired; user must re-authenticate".to_string(),
                )
            } else {
                (502, "send_failed", format!("Upstream send failed: {err}"))
            };
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                status,
                code,
                &detail,
                finish_ms,
            )
            .await;
            HttpResponse::error(status, code, &detail)
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScheduleMessageRequestBody {
    thread_id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    thread_title: Option<String>,
    text: String,
    send_at_ts: i64,
    #[serde(default)]
    recurrence: Option<String>,
    #[serde(default)]
    misfire_policy: Option<String>,
    #[serde(default)]
    misfire_grace_secs: Option<i64>,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn handle_schedule_message(
    req: &HttpRequest,
    client: &ApiClientDto,
    ctx: &ServerContext,
) -> HttpResponse {
    let now_ms = now_epoch_ms();
    if !client_has_scope(client, "schedule") {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_scope",
            "Missing required scope: schedule",
            now_ms,
        )
        .await;
        return HttpResponse::error(403, "forbidden_scope", "Missing required scope: schedule");
    }

    let body: ScheduleMessageRequestBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                "",
                400,
                "invalid_json",
                "Invalid JSON body for /v1/messages/schedule",
                now_ms,
            )
            .await;
            return HttpResponse::error(400, "invalid_json", "Invalid JSON request body");
        }
    };

    let thread_id = body.thread_id.trim();
    if thread_id.is_empty() || thread_id.chars().count() > MAX_THREAD_ID_CHARS {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_thread_id",
            "thread_id must be non-empty and <= 128 characters",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_thread_id",
            "thread_id must be non-empty and <= 128 characters",
        );
    }

    if !is_thread_allowed(client, thread_id) {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            403,
            "forbidden_thread",
            "Target thread_id is not in client allowlist",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            403,
            "forbidden_thread",
            "Target thread_id is not permitted for this API client",
        );
    }

    let trimmed_text = body.text.trim();
    if trimmed_text.is_empty() || trimmed_text.chars().count() > MAX_MESSAGE_TEXT_CHARS {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_text",
            "Message text must be 1..=4000 characters",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_text",
            "Message text must be non-empty and <= 4000 characters",
        );
    }

    if body.send_at_ts <= 0 {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_send_at_ts",
            "send_at_ts must be positive epoch milliseconds",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_send_at_ts",
            "send_at_ts must be positive epoch milliseconds",
        );
    }

    let recurrence = match body
        .recurrence
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        None => "once".to_string(),
        Some(ref s) if s.is_empty() => "once".to_string(),
        Some(s) if matches!(s.as_str(), "once" | "daily" | "weekly" | "monthly") => s,
        Some(_) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                400,
                "invalid_recurrence",
                "recurrence must be once, daily, weekly, or monthly",
                now_ms,
            )
            .await;
            return HttpResponse::error(
                400,
                "invalid_recurrence",
                "Invalid recurrence (expected once, daily, weekly, or monthly)",
            );
        }
    };

    let misfire_policy = match body
        .misfire_policy
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        None => "send_on_resume".to_string(),
        Some(ref s) if s.is_empty() => "send_on_resume".to_string(),
        Some(s) if matches!(s.as_str(), "skip" | "send_on_resume" | "ask") => s,
        Some(_) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                400,
                "invalid_misfire_policy",
                "misfire_policy must be skip, send_on_resume, or ask",
                now_ms,
            )
            .await;
            return HttpResponse::error(
                400,
                "invalid_misfire_policy",
                "Invalid misfire_policy (expected skip, send_on_resume, or ask)",
            );
        }
    };

    let idem_key = match resolve_idempotency_key(
        req.headers.get("idempotency-key").map(String::as_str),
        body.idempotency_key.as_deref(),
    ) {
        Ok(k) => k.map(|raw| format!("schedule:{raw}")),
        Err(msg) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                400,
                "invalid_idempotency_key",
                msg,
                now_ms,
            )
            .await;
            return HttpResponse::error(400, "invalid_idempotency_key", msg);
        }
    };

    // Check 24h idempotency cache first
    if let Some(ref scoped_key) = idem_key {
        let cached = {
            let store = ctx.store_arc.lock().await;
            store
                .get_api_idempotency(&client.id, scoped_key, IDEMPOTENCY_TTL_MS, now_ms)
                .ok()
                .flatten()
        };
        if let Some((cached_status, cached_json)) = cached {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                cached_status,
                "idempotent_replay",
                "Returned cached idempotent response",
                now_ms,
            )
            .await;
            return HttpResponse::raw_json(cached_status, cached_json)
                .with_header("Idempotency-Replayed", "true");
        }
    }

    let explicit_kind = match body.kind.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => match parse_explicit_thread_kind(raw) {
            Ok(k) => Some(k),
            Err(msg) => {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    thread_id,
                    400,
                    "invalid_kind",
                    msg,
                    now_ms,
                )
                .await;
                return HttpResponse::error(400, "invalid_kind", msg);
            }
        },
        None => None,
    };

    let created = {
        let store = ctx.store_arc.lock().await;
        let resolved_kind = explicit_kind.unwrap_or_else(|| {
            store
                .get_thread_kind(thread_id)
                .ok()
                .flatten()
                .unwrap_or(ThreadKind::Direct)
        });
        let resolved_title = body
            .thread_title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                store
                    .list_threads(FolderFilter::All)
                    .ok()
                    .and_then(|list| list.into_iter().find(|t| t.id == thread_id).map(|t| t.title))
            })
            .unwrap_or_else(|| thread_id.to_string());

        let input = NewScheduledMessageInput {
            thread_id: thread_id.to_string(),
            thread_title: resolved_title,
            thread_kind: thread_kind_str(resolved_kind).to_string(),
            text: trimmed_text.to_string(),
            attachment_path: String::new(),
            send_at_ts: body.send_at_ts,
            recurrence,
            misfire_policy,
            misfire_grace_secs: body.misfire_grace_secs.unwrap_or(600).max(0),
        };
        let res = store.create_scheduled_message(&input);
        match res {
            Ok(sched) => {
                let resp_body = serde_json::json!({
                    "ok": true,
                    "schedule": sched,
                })
                .to_string();
                if let Some(ref scoped_key) = idem_key {
                    if let Err(err) = store.put_api_idempotency(
                        &client.id,
                        scoped_key,
                        200,
                        &resp_body,
                        now_ms,
                    ) {
                        eprintln!(
                            "[teza][local-api] failed to persist schedule idempotency: {err}"
                        );
                    }
                }
                Ok((sched, resp_body))
            }
            Err(err) => Err(err),
        }
    };

    match created {
        Ok((sched, resp_body)) => {
            // Wake background scheduler immediately!
            ctx.scheduler.notify_changed();
            ctx.sink.emit_json("core://schedule-updated", &sched);
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                &sched.thread_id,
                200,
                "ok",
                &format!("created schedule id={}", sched.id),
                now_ms,
            )
            .await;
            HttpResponse::raw_json(200, resp_body)
        }
        Err(err) => {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                500,
                "store_error",
                &err.to_string(),
                now_ms,
            )
            .await;
            HttpResponse::error(500, "store_error", "Failed to create scheduled message")
        }
    }
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use zalo_core::chat::ThreadDto;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(1);

    struct TestEnv {
        dir: PathBuf,
        manager: LocalApiManager,
        bound_addr: SocketAddr,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
        events: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn setup_test_env() -> TestEnv {
        let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "teza-local-api-test-{}-{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let paths = StoragePaths::from_dir(&dir).unwrap();
        let db_path = paths.history_db(Some("test_local_api"));
        let store = MessageStore::open(&db_path).unwrap();

        // Seed sample threads
        for thread in [
            ThreadDto {
                id: "u_allowed".into(),
                title: "Nguyễn Văn An".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Xin chào".into(),
                last_ts: 1_700_000_000_000,
                unread_count: 2,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "g_ops".into(),
                title: "Nhóm Vận Hành".into(),
                avatar: String::new(),
                kind: ThreadKind::Group,
                last_preview: "Cảnh báo server".into(),
                last_ts: 1_700_000_100_000,
                unread_count: 0,
                is_pinned: true,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
            ThreadDto {
                id: "u_secret".into(),
                title: "Riêng Tư".into(),
                avatar: String::new(),
                kind: ThreadKind::Direct,
                last_preview: "Bí mật".into(),
                last_ts: 1_700_000_200_000,
                unread_count: 1,
                is_pinned: false,
                is_muted: false,
                last_read_in_ts: 0,
                peer_read_ts: 0,
                muted_until_ts: 0,
                is_archived: false,
            },
        ] {
            store.upsert_thread(&thread).unwrap();
        }

        let store_arc = Arc::new(Mutex::new(store));
        let sess_arc = Arc::new(Mutex::new(Session::new().unwrap()));
        let scheduler = SchedulerHandle::new();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let manager = LocalApiManager::new(&paths);

        // Bind strictly on 127.0.0.1:0 (ephemeral loopback port)
        let bound_addr = manager
            .start_internal(
                0,
                EventSink::capture(events.clone()),
                sess_arc,
                store_arc.clone(),
                scheduler.clone(),
            )
            .await
            .unwrap();
        assert!(bound_addr.ip().is_loopback());

        TestEnv {
            dir,
            manager,
            bound_addr,
            store_arc,
            scheduler,
            events,
        }
    }

    async fn send_raw_http(addr: SocketAddr, raw: &str) -> (u16, HashMap<String, String>, String) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8(buf).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        (status, headers, body.to_string())
    }

    #[test]
    fn config_validation_and_persistence_work() {
        let mut cfg = LocalApiConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.port, DEFAULT_LOCAL_API_PORT);
        assert_eq!(cfg.min_send_interval_ms, DEFAULT_MIN_SEND_INTERVAL_MS);
        assert!(cfg.validate().is_ok());

        cfg.port = 80;
        assert!(cfg.validate().is_err());
        cfg.port = 17821;
        cfg.min_send_interval_ms = 50;
        assert!(cfg.validate().is_err());

        cfg.min_send_interval_ms = 2_000;
        cfg.enabled = true;
        assert!(cfg.validate().is_ok());

        let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "teza-local-api-cfg-test-{}-{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("local_api.json");
        cfg.save_to_file(&path).unwrap();
        let loaded = LocalApiConfig::load_from_file(&path);
        assert_eq!(loaded, cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_and_csrf_validators_block_dns_rebinding_and_browsers() {
        assert!(is_valid_local_host("127.0.0.1:17820", 17820));
        assert!(is_valid_local_host("localhost:17820", 17820));
        assert!(is_valid_local_host("127.0.0.1", 17820));
        assert!(is_valid_local_host("LOCALHOST", 17820));
        assert!(!is_valid_local_host("evil.com", 17820));
        assert!(!is_valid_local_host("127.0.0.1.evil.com:17820", 17820));
        assert!(!is_valid_local_host("127.0.0.1:9999", 17820));
        assert!(!is_valid_local_host("0.0.0.0:17820", 17820));

        assert!(!has_forbidden_browser_headers(None, None));
        assert!(!has_forbidden_browser_headers(None, Some("none")));
        assert!(!has_forbidden_browser_headers(None, Some("NONE")));
        assert!(has_forbidden_browser_headers(Some("http://localhost"), None));
        assert!(has_forbidden_browser_headers(Some("null"), None));
        assert!(has_forbidden_browser_headers(None, Some("cross-site")));
        assert!(has_forbidden_browser_headers(None, Some("same-origin")));
    }

    #[test]
    fn resolve_idempotency_key_enforces_118_utf8_byte_cap() {
        let ok_ascii_118 = "a".repeat(118);
        assert_eq!(
            resolve_idempotency_key(Some(&ok_ascii_118), None).unwrap(),
            Some(ok_ascii_118.clone())
        );
        assert!(format!("schedule:{ok_ascii_118}").len() <= 128);

        let ok_utf8_118 = "ê".repeat(59); // 59 chars * 2 bytes = 118 bytes
        assert_eq!(ok_utf8_118.len(), 118);
        assert_eq!(
            resolve_idempotency_key(None, Some(&ok_utf8_118)).unwrap(),
            Some(ok_utf8_118)
        );

        let bad_ascii_119 = "a".repeat(119);
        assert!(resolve_idempotency_key(Some(&bad_ascii_119), None).is_err());

        let bad_utf8_120 = "ê".repeat(60); // 60 chars * 2 bytes = 120 bytes
        assert_eq!(bad_utf8_120.len(), 120);
        assert!(resolve_idempotency_key(None, Some(&bad_utf8_120)).is_err());
    }

    #[test]
    fn global_anti_spam_spacing_throttles_rapid_sends() {
        let mut limiter = RateLimiterState::default();
        let t0 = Instant::now();
        assert!(limiter.check_and_reserve_send_slot(1_500, t0).is_ok());

        let too_soon = t0 + Duration::from_millis(400);
        let err = limiter.check_and_reserve_send_slot(1_500, too_soon).unwrap_err();
        assert!(err.0 >= 1, "retry_after_secs must be >= 1");
        assert!(err.1 >= 1_000 && err.1 <= 1_100);

        let after_spacing = t0 + Duration::from_millis(1_500);
        assert!(limiter.check_and_reserve_send_slot(1_500, after_spacing).is_ok());
    }

    #[test]
    fn integration_security_endpoints_idempotency_rate_limit_and_kill_switch() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            let port = env.bound_addr.port();

            // Create two clients: full-access client and restricted client
            let (full_token, restricted_token, rate_limited_token) = {
                let store = env.store_arc.lock().await;
                let full = store
                    .create_api_client(
                        "CI Bot",
                        &["read_threads".into(), "send".into(), "schedule".into()],
                        &[],
                        30,
                    )
                    .unwrap();
                let restricted = store
                    .create_api_client(
                        "Alerts Only",
                        &["read_threads".into(), "schedule".into()],
                        &["u_allowed".into(), "g_ops".into()],
                        30,
                    )
                    .unwrap();
                let tight = store
                    .create_api_client("Tight Limit", &["read_threads".into()], &[], 2)
                    .unwrap();
                (full.raw_token, restricted.raw_token, tight.raw_token)
            };

            // 1. DNS Rebinding rejection (403)
            let (st, hdrs, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: evil.attacker.test:{port}\r\nAuthorization: Bearer {full_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 403);
            assert_eq!(hdrs.get("connection").map(String::as_str), Some("close"));
            assert!(!hdrs.contains_key("access-control-allow-origin"));

            // 2. Origin / Sec-Fetch-Site CSRF rejection (403)
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\nAuthorization: Bearer {full_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 403);

            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nSec-Fetch-Site: cross-site\r\nAuthorization: Bearer {full_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 403);

            // 3. Missing & invalid Bearer token rejection (401)
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 401);

            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer teza_wrong_token\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 401);

            // 4. GET /v1/health with valid token (200)
            let (st, _, body) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: localhost:{port}\r\nAuthorization: Bearer {full_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 200);
            let health: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(health["ok"], true);
            assert_eq!(health["authenticated"], false);
            assert!(health["server_time_ms"].as_i64().unwrap() > 0);

            // 5. GET /v1/threads respects allowed_thread_ids & query filters
            let (st, _, body) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/threads HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 200);
            let threads_json: serde_json::Value = serde_json::from_str(&body).unwrap();
            let ids: Vec<&str> = threads_json["threads"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&"u_allowed"));
            assert!(ids.contains(&"g_ops"));
            assert!(!ids.contains(&"u_secret"), "unlisted thread must be hidden");

            let (st, _, body) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/threads?kind=group&q=van+hanh HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {full_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st, 200);
            let filtered: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(filtered["threads"].as_array().unwrap().len(), 1);
            assert_eq!(filtered["threads"][0]["id"], "g_ops");

            // 6. Scope enforcement (restricted client lacks `send`) & send validation
            let send_payload = r#"{"thread_id":"u_allowed","text":"Hello"}"#;
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{send_payload}",
                    send_payload.len()
                ),
            )
            .await;
            assert_eq!(st, 403);

            // Empty text -> 400
            let empty_send = r#"{"thread_id":"u_allowed","text":"   "}"#;
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {full_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{empty_send}",
                    empty_send.len()
                ),
            )
            .await;
            assert_eq!(st, 400);

            // > 4000 chars text -> 400
            let huge_text = "a".repeat(4_001);
            let huge_send = format!(r#"{{"thread_id":"u_allowed","text":"{huge_text}"}}"#);
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {full_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{huge_send}",
                    huge_send.len()
                ),
            )
            .await;
            assert_eq!(st, 400);

            // Valid text when Teza account is unauthenticated -> 503
            let (st, _, body_unauth) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {full_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{send_payload}",
                    send_payload.len()
                ),
            )
            .await;
            assert_eq!(st, 503);
            assert!(body_unauth.contains("not_authenticated"));

            // 7. allowed_thread_ids enforcement on POST /v1/messages/schedule (403)
            let future_ts = now_epoch_ms() + 300_000;
            let forbidden_sched = format!(
                r#"{{"thread_id":"u_secret","text":"Blocked","send_at_ts":{future_ts}}}"#
            );
            let (st, _, _) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{forbidden_sched}",
                    forbidden_sched.len()
                ),
            )
            .await;
            assert_eq!(st, 403);

            // 8. POST /v1/messages/schedule + Idempotency replay + scheduler notify
            let notify_arc = env.scheduler.notify_arc();
            let sched_payload = format!(
                r#"{{"thread_id":"g_ops","text":"Daily report","send_at_ts":{future_ts},"recurrence":"daily","misfire_policy":"ask"}}"#
            );
            let req1 = format!(
                "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nIdempotency-Key: deploy-alert-001\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{sched_payload}",
                sched_payload.len()
            );
            let (st1, hdrs1, body1) = send_raw_http(env.bound_addr, &req1).await;
            assert_eq!(st1, 200);
            assert!(!hdrs1.contains_key("idempotency-replayed"));
            let j1: serde_json::Value = serde_json::from_str(&body1).unwrap();
            assert_eq!(j1["ok"], true);
            assert_eq!(j1["schedule"]["thread_id"], "g_ops");
            assert_eq!(j1["schedule"]["thread_kind"], "group");

            // Scheduler must have been notified immediately!
            assert!(
                tokio::time::timeout(Duration::from_millis(200), notify_arc.notified())
                    .await
                    .is_ok()
            );

            // Replay exact same Idempotency-Key -> identical response body & no duplicate DB row!
            let (st2, hdrs2, body2) = send_raw_http(env.bound_addr, &req1).await;
            assert_eq!(st2, 200);
            assert_eq!(
                hdrs2.get("idempotency-replayed").map(String::as_str),
                Some("true")
            );
            assert_eq!(body1, body2);

            // 8b. Idempotency byte-length boundary regression:
            // - 118-byte key ("schedule:" + 118B = 127B <= 128B) persists & replays cleanly!
            // - 119-byte ASCII key and 120-byte UTF-8 key (60 multi-byte chars) return 400!
            let key_118 = "k".repeat(118);
            let req_118 = format!(
                "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nIdempotency-Key: {key_118}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{sched_payload}",
                sched_payload.len()
            );
            let (st_118_a, hdrs_118_a, body_118_a) = send_raw_http(env.bound_addr, &req_118).await;
            assert_eq!(st_118_a, 200);
            assert!(!hdrs_118_a.contains_key("idempotency-replayed"));
            let (st_118_b, hdrs_118_b, body_118_b) = send_raw_http(env.bound_addr, &req_118).await;
            assert_eq!(st_118_b, 200);
            assert_eq!(
                hdrs_118_b.get("idempotency-replayed").map(String::as_str),
                Some("true")
            );
            assert_eq!(body_118_a, body_118_b);

            let key_119 = "k".repeat(119);
            let req_119 = format!(
                "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nIdempotency-Key: {key_119}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{sched_payload}",
                sched_payload.len()
            );
            let (st_119, _, body_119) = send_raw_http(env.bound_addr, &req_119).await;
            assert_eq!(st_119, 400);
            assert!(body_119.contains("invalid_idempotency_key"));

            // 60 multi-byte UTF-8 chars ("ê" is 2 bytes each => 120 bytes > 118 bytes) via JSON body -> 400
            let key_utf8_120 = "ê".repeat(60);
            let sched_utf8_payload = format!(
                r#"{{"thread_id":"g_ops","text":"Daily report","send_at_ts":{future_ts},"idempotency_key":"{key_utf8_120}"}}"#
            );
            let req_utf8_120 = format!(
                "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{sched_utf8_payload}",
                sched_utf8_payload.len()
            );
            let (st_utf8, _, body_utf8) = send_raw_http(env.bound_addr, &req_utf8_120).await;
            assert_eq!(st_utf8, 400);
            assert!(body_utf8.contains("invalid_idempotency_key"));

            let (st_list, _, body_list) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/schedules?status=pending,missed HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st_list, 200);
            let list_json: serde_json::Value = serde_json::from_str(&body_list).unwrap();
            assert_eq!(list_json["schedules"].as_array().unwrap().len(), 2);

            // 9. Per-client rate limit -> 429 with Retry-After
            let rl_req = format!(
                "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {rate_limited_token}\r\n\r\n"
            );
            assert_eq!(send_raw_http(env.bound_addr, &rl_req).await.0, 200);
            assert_eq!(send_raw_http(env.bound_addr, &rl_req).await.0, 200);
            let (st_rl, hdrs_rl, _) = send_raw_http(env.bound_addr, &rl_req).await;
            assert_eq!(st_rl, 429);
            assert!(hdrs_rl.contains_key("retry-after"));

            // 10. HTTP smuggling, oversized header (>8 KiB), and oversized body (>64 KiB) protection
            let (st_te, _, _) = send_raw_http(
                env.bound_addr,
                &format!("POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nTransfer-Encoding: chunked\r\n\r\n"),
            )
            .await;
            assert_eq!(st_te, 501);

            let huge_hdr = "X".repeat(9 * 1024);
            let (st_hdr, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Pad: {huge_hdr}\r\n\r\n"),
            )
            .await;
            assert_eq!(st_hdr, 431);

            let (st_big, _, _) = send_raw_http(
                env.bound_addr,
                &format!("POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 999999\r\n\r\n"),
            )
            .await;
            assert_eq!(st_big, 413);

            // Verify audit events were recorded & emitted
            {
                let store = env.store_arc.lock().await;
                let audits = store.list_api_audit_logs(50).unwrap();
                assert!(audits.len() >= 8, "audit logs should record auth failures and API calls");
            }
            {
                let emitted = env.events.lock().unwrap();
                assert!(emitted.iter().any(|(ev, _)| ev == "core://schedule-updated"));
                assert!(emitted.iter().any(|(ev, _)| ev == "core://local-api-audit"));
            }

            // 11. Kill Switch shutdown immediately closes listener and persists enabled=false
            let killed_status = env.manager.kill_switch(&env.store_arc).await.unwrap();
            assert!(!killed_status.enabled);
            assert!(!killed_status.running);
            assert_eq!(killed_status.active_clients, 3);
            assert!(
                TcpStream::connect(env.bound_addr).await.is_err(),
                "listener must reject connections immediately after Kill Switch"
            );
            let persisted = LocalApiConfig::load_from_file(&env.dir.join("local_api.json"));
            assert!(!persisted.enabled);
        });
    }
}
