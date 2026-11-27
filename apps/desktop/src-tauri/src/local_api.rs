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

use std::collections::{HashMap, HashSet, VecDeque};
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
use zalo_core::chat::parser::{format_thread_preview, normalize_ts_ms};
use zalo_core::chat::{strip_vi_accents, FolderFilter, ThreadKind};
use zalo_core::data_dir::{write_private_atomic, StoragePaths};
use zalo_core::session::Session;
use zalo_core::store::{
    ApiClientDto, MessageStore, NewScheduledMessageInput, ScheduledMessageDto,
};

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
/// Maximum schedule horizon (5 years in milliseconds) accepted by `POST /v1/messages/schedule`.
const MAX_SCHEDULE_FUTURE_MS: i64 = 5 * 365 * 24 * 3_600 * 1_000;
/// Cap unauthenticated 401/403 audit log writes per minute so floods cannot evict valid history.
const MAX_UNAUTH_AUDIT_PER_MIN: usize = 20;
/// Cap logged HTTP request paths in audit entries to 256 UTF-8 characters.
const MAX_AUDIT_PATH_CHARS: usize = 256;
/// Security-sensitive HTTP headers that MUST NOT appear more than once in a request.
const SINGLE_VALUE_SECURITY_HEADERS: [&str; 6] = [
    "host",
    "content-length",
    "sec-fetch-site",
    "origin",
    "authorization",
    "idempotency-key",
];

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
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Sanitized schedule payload returned by Local API endpoints (redacts local filesystem
/// `attachment_path` and exposes boolean `has_attachment` instead).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ApiScheduledMessageDto {
    id: String,
    thread_id: String,
    thread_title: String,
    thread_kind: String,
    text: String,
    has_attachment: bool,
    send_at_ts: i64,
    next_attempt_ts: Option<i64>,
    recurrence: String,
    misfire_policy: String,
    misfire_grace_secs: i64,
    status: String,
    retry_count: u32,
    last_error: String,
    created_at: i64,
    updated_at: i64,
    sent_at_ts: i64,
}

impl From<ScheduledMessageDto> for ApiScheduledMessageDto {
    fn from(item: ScheduledMessageDto) -> Self {
        Self {
            id: item.id,
            thread_id: item.thread_id,
            thread_title: item.thread_title,
            thread_kind: item.thread_kind,
            text: item.text,
            has_attachment: !item.attachment_path.trim().is_empty(),
            send_at_ts: item.send_at_ts,
            next_attempt_ts: item.next_attempt_ts,
            recurrence: item.recurrence,
            misfire_policy: item.misfire_policy,
            misfire_grace_secs: item.misfire_grace_secs,
            status: item.status,
            retry_count: item.retry_count,
            last_error: item.last_error,
            created_at: item.created_at,
            updated_at: item.updated_at,
            sent_at_ts: item.sent_at_ts,
        }
    }
}

/// Standard FIPS 180-4 SHA-256 hex digest used for canonical idempotency payload hashing.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in padded.as_chunks::<64>().0 {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{word:08x}");
    }
    out
}

/// RAII guard preventing concurrent requests with the same `(client_id, scoped_key)`
/// from racing past idempotency replay checks.
struct InFlightIdempotencyGuard {
    set: Arc<std::sync::Mutex<HashSet<(String, String)>>>,
    key: (String, String),
}

impl InFlightIdempotencyGuard {
    fn try_acquire(
        set: &Arc<std::sync::Mutex<HashSet<(String, String)>>>,
        client_id: &str,
        scoped_key: &str,
    ) -> Option<Self> {
        let key = (client_id.to_string(), scoped_key.to_string());
        let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
        if guard.insert(key.clone()) {
            Some(Self {
                set: set.clone(),
                key,
            })
        } else {
            None
        }
    }
}

impl Drop for InFlightIdempotencyGuard {
    fn drop(&mut self) {
        let mut guard = self.set.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&self.key);
    }
}

type EventEmitFn = dyn Fn(&str, serde_json::Value) + Send + Sync;
type SessionExpiredFn = dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
struct EventSink {
    emit_fn: Arc<EventEmitFn>,
    expire_session_fn: Option<Arc<SessionExpiredFn>>,
}

impl EventSink {
    fn from_app(app: AppHandle) -> Self {
        let app_for_emit = app.clone();
        let app_for_expire = app;
        Self {
            emit_fn: Arc::new(move |event, payload| {
                let _ = app_for_emit.emit(event, payload);
            }),
            expire_session_fn: Some(Arc::new(move |reason| {
                let app_handle = app_for_expire.clone();
                Box::pin(async move {
                    crate::clear_expired_session_for_app(&app_handle, reason, true).await;
                })
            })),
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
            expire_session_fn: None,
        }
    }

    fn emit_json<T: Serialize>(&self, event: &str, payload: &T) {
        if let Ok(val) = serde_json::to_value(payload) {
            (self.emit_fn)(event, val);
        }
    }

    async fn clear_expired_session(&self, reason: String) {
        if let Some(ref handler) = self.expire_session_fn {
            (handler)(reason).await;
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
    unauth_audit_timestamps: VecDeque<Instant>,
}

impl RateLimiterState {
    fn allow_unauth_audit(&mut self, now: Instant) -> bool {
        while let Some(&front) = self.unauth_audit_timestamps.front() {
            if now.saturating_duration_since(front) >= RATE_LIMIT_WINDOW {
                self.unauth_audit_timestamps.pop_front();
            } else {
                break;
            }
        }
        if self.unauth_audit_timestamps.len() >= MAX_UNAUTH_AUDIT_PER_MIN {
            return false;
        }
        self.unauth_audit_timestamps.push_back(now);
        true
    }

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
    last_error: Arc<Mutex<Option<String>>>,
    lifecycle_lock: Arc<Mutex<()>>,
    rate_limiter: Arc<Mutex<RateLimiterState>>,
    in_flight_idempotency: Arc<std::sync::Mutex<HashSet<(String, String)>>>,
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
            last_error: Arc::new(Mutex::new(None)),
            lifecycle_lock: Arc::new(Mutex::new(())),
            rate_limiter: Arc::new(Mutex::new(RateLimiterState::default())),
            in_flight_idempotency: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }

    pub async fn config(&self) -> LocalApiConfig {
        self.config.lock().await.clone()
    }

    /// Return current status including live bound address, last error, and count of enabled API clients.
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
        let last_error = self.last_error.lock().await.clone();
        LocalApiStatusDto {
            enabled: cfg.enabled,
            running,
            bind_addr: bound_addr,
            port,
            active_clients,
            last_error,
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
        let _lc = self.lifecycle_lock.lock().await;
        let cfg = self.config.lock().await.clone();
        if !cfg.enabled {
            return Ok(self.status(&store_arc).await);
        }
        if let Err(err) = cfg.validate() {
            *self.last_error.lock().await = Some(err.clone());
            return Err(err);
        }
        let sink = EventSink::from_app(app);
        match self
            .start_internal_locked(
                cfg.port,
                sink,
                sess_arc,
                store_arc.clone(),
                scheduler,
            )
            .await
        {
            Ok(_) => {
                *self.last_error.lock().await = None;
                Ok(self.status(&store_arc).await)
            }
            Err(err) => {
                *self.last_error.lock().await = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Bind strictly on `127.0.0.1:<cfg.port>` and start accepting Local API requests.
    /// Stops any existing listener first.
    #[allow(dead_code)]
    pub async fn start(
        &self,
        app: AppHandle,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        let _lc = self.lifecycle_lock.lock().await;
        let cfg = self.config.lock().await.clone();
        if let Err(err) = cfg.validate() {
            *self.last_error.lock().await = Some(err.clone());
            return Err(err);
        }
        let sink = EventSink::from_app(app);
        match self
            .start_internal_locked(
                cfg.port,
                sink,
                sess_arc,
                store_arc.clone(),
                scheduler,
            )
            .await
        {
            Ok(_) => {
                *self.last_error.lock().await = None;
                Ok(self.status(&store_arc).await)
            }
            Err(err) => {
                *self.last_error.lock().await = Some(err.clone());
                Err(err)
            }
        }
    }

    /// Immediately stop the running HTTP server via the watch shutdown channel and await
    /// listener drop so the port is released cleanly. Returns `true` if a server was stopped.
    pub async fn stop(&self) -> bool {
        let _lc = self.lifecycle_lock.lock().await;
        let stopped = self.stop_locked().await;
        *self.last_error.lock().await = None;
        stopped
    }

    async fn stop_locked(&self) -> bool {
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
    ///
    /// When changing ports while running, binds the NEW listener and saves config BEFORE
    /// stopping the old listener so an occupied port or disk write failure never disrupts
    /// a working listener (#35).
    pub async fn apply_config(
        &self,
        new_cfg: LocalApiConfig,
        app: AppHandle,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        let sink = EventSink::from_app(app);
        self.apply_config_with_sink(new_cfg, sink, sess_arc, store_arc, scheduler)
            .await
    }

    async fn apply_config_with_sink(
        &self,
        new_cfg: LocalApiConfig,
        sink: EventSink,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<LocalApiStatusDto, String> {
        let _lc = self.lifecycle_lock.lock().await;
        new_cfg.validate()?;

        if !new_cfg.enabled {
            new_cfg
                .save_to_file(&self.config_path)
                .map_err(|e| format!("Không lưu được cấu hình Local API: {e}"))?;
            *self.config.lock().await = new_cfg;
            self.stop_locked().await;
            *self.last_error.lock().await = None;
            return Ok(self.status(&store_arc).await);
        }

        let running_port = {
            let mut rt = self.runtime.lock().await;
            match rt.as_ref() {
                Some(srv) if !srv.join_handle.is_finished() => Some(srv.bound_addr.port()),
                Some(_) => {
                    rt.take();
                    None
                }
                None => None,
            }
        };

        // Already running on the exact target port (only e.g. `min_send_interval_ms` changed).
        if running_port == Some(new_cfg.port) {
            new_cfg
                .save_to_file(&self.config_path)
                .map_err(|e| format!("Không lưu được cấu hình Local API: {e}"))?;
            *self.config.lock().await = new_cfg;
            *self.last_error.lock().await = None;
            return Ok(self.status(&store_arc).await);
        }

        // Bind the NEW loopback listener BEFORE stopping any currently running listener (#35).
        let (new_listener, actual_addr) = match Self::bind_loopback_listener(new_cfg.port).await {
            Ok(pair) => pair,
            Err(bind_err) => {
                if running_port.is_none() {
                    *self.last_error.lock().await = Some(bind_err.clone());
                }
                return Err(bind_err);
            }
        };

        // Persist config BEFORE tearing down the old listener: if saving fails, dropping
        // `new_listener` cleanly restores the previous working listener and config with zero downtime.
        if let Err(save_err) = new_cfg.save_to_file(&self.config_path) {
            drop(new_listener);
            return Err(format!("Không lưu được cấu hình Local API: {save_err}"));
        }

        *self.config.lock().await = new_cfg;
        self.stop_locked().await;
        self.spawn_bound_listener_locked(
            new_listener,
            actual_addr,
            sink,
            sess_arc,
            store_arc.clone(),
            scheduler,
        )
        .await;
        *self.last_error.lock().await = None;

        Ok(self.status(&store_arc).await)
    }

    /// Kill Switch: immediately disables `enabled = false`, clears `last_error`, closes listener,
    /// and persists config.
    pub async fn kill_switch(
        &self,
        store_arc: &Arc<Mutex<MessageStore>>,
    ) -> Result<LocalApiStatusDto, String> {
        let _lc = self.lifecycle_lock.lock().await;
        self.stop_locked().await;
        *self.last_error.lock().await = None;
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

    async fn bind_loopback_listener(port: u16) -> Result<(TcpListener, SocketAddr), String> {
        // STRICT loopback-only bind: 127.0.0.1 ONLY, NEVER 0.0.0.0!
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("Không thể mở Local API trên {bind_addr}: {e}"))?;
        let actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Không đọc được địa chỉ cục bộ của Local API: {e}"))?;
        debug_assert!(actual_addr.ip().is_loopback());
        Ok((listener, actual_addr))
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_bound_listener_locked(
        &self,
        listener: TcpListener,
        actual_addr: SocketAddr,
        sink: EventSink,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let ctx = Arc::new(ServerContext {
            bound_port: actual_addr.port(),
            config: self.config.clone(),
            rate_limiter: self.rate_limiter.clone(),
            in_flight_idempotency: self.in_flight_idempotency.clone(),
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
    }

    async fn start_internal_locked(
        &self,
        port: u16,
        sink: EventSink,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<SocketAddr, String> {
        let running_same_port = {
            let mut rt = self.runtime.lock().await;
            match rt.as_ref() {
                Some(srv) if !srv.join_handle.is_finished() => {
                    port != 0 && srv.bound_addr.port() == port
                }
                Some(_) => {
                    rt.take();
                    false
                }
                None => false,
            }
        };
        if running_same_port {
            self.stop_locked().await;
        }
        let (listener, actual_addr) = Self::bind_loopback_listener(port).await?;
        self.stop_locked().await;
        self.spawn_bound_listener_locked(
            listener,
            actual_addr,
            sink,
            sess_arc,
            store_arc,
            scheduler,
        )
        .await;
        Ok(actual_addr)
    }

    #[cfg(test)]
    async fn start_internal(
        &self,
        port: u16,
        sink: EventSink,
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
    ) -> Result<SocketAddr, String> {
        let _lc = self.lifecycle_lock.lock().await;
        match self
            .start_internal_locked(port, sink, sess_arc, store_arc, scheduler)
            .await
        {
            Ok(addr) => {
                *self.last_error.lock().await = None;
                Ok(addr)
            }
            Err(err) => {
                *self.last_error.lock().await = Some(err.clone());
                Err(err)
            }
        }
    }
}

struct ServerContext {
    bound_port: u16,
    config: Arc<Mutex<LocalApiConfig>>,
    rate_limiter: Arc<Mutex<RateLimiterState>>,
    in_flight_idempotency: Arc<std::sync::Mutex<HashSet<(String, String)>>>,
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
        409 => "Conflict",
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
/// Safe on arbitrary UTF-8 input (never slices at a non-character byte boundary).
pub fn extract_bearer_token(auth_header: Option<&str>) -> Option<&str> {
    let raw = auth_header?.trim();
    let (scheme, rest) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
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

        if SINGLE_VALUE_SECURITY_HEADERS.contains(&name.as_str()) && headers.contains_key(&name) {
            return ReadRequestResult::Error(HttpResponse::error(
                400,
                "duplicate_header",
                "Duplicate single-value security header",
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

    // #14: Support `Expect: 100-continue` (sent by curl/.NET/Java for bodies > 1 KiB)
    // after headers & Content-Length <= MAX_BODY_BYTES have passed validation.
    if let Some(expect_val) = headers.get("expect") {
        if expect_val.trim().eq_ignore_ascii_case("100-continue") && body.len() < content_length {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return ReadRequestResult::Error(HttpResponse::error(
                    408,
                    "request_timeout",
                    "Request timed out before 100 Continue",
                ));
            };
            let continue_fut = async {
                stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
                stream.flush().await
            };
            match tokio::time::timeout(remaining, continue_fut).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return ReadRequestResult::EmptyClose,
                Err(_) => {
                    return ReadRequestResult::Error(HttpResponse::error(
                        408,
                        "request_timeout",
                        "Timed out sending 100 Continue",
                    ));
                }
            }
        }
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
    let now_ms = now_epoch_ms();

    // 1. DNS Rebinding Protection (mandatory Host header validation)
    let Some(host) = req.headers.get("host").map(String::as_str) else {
        record_unauth_audit(
            ctx,
            "",
            "anonymous",
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_host",
            "Missing Host header",
            now_ms,
        )
        .await;
        return HttpResponse::error(403, "forbidden_host", "Missing Host header");
    };
    if !is_valid_local_host(host, ctx.bound_port) {
        record_unauth_audit(
            ctx,
            "",
            "anonymous",
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_host",
            "Forbidden Host header (loopback only)",
            now_ms,
        )
        .await;
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
        record_unauth_audit(
            ctx,
            "",
            "anonymous",
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_origin",
            "Browser cross-origin requests are blocked",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            403,
            "forbidden_origin",
            "Browser cross-origin requests are blocked",
        );
    }

    // 3. Bearer Token Authentication (required on ALL endpoints including /v1/health)
    let Some(raw_token) = extract_bearer_token(req.headers.get("authorization").map(String::as_str))
    else {
        record_unauth_audit(
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
        record_unauth_audit(
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

fn truncate_audit_path(path: &str) -> String {
    if path.chars().count() <= MAX_AUDIT_PATH_CHARS {
        path.to_string()
    } else {
        path.chars().take(MAX_AUDIT_PATH_CHARS).collect()
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
    let safe_path = truncate_audit_path(path);
    let logged = {
        let store = ctx.store_arc.lock().await;
        store
            .append_api_audit_log(
                client_id,
                client_name,
                method,
                &safe_path,
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

/// Rate-limited audit logger for unauthenticated 401/403 rejections (#38) so an
/// unauthenticated flood cannot evict the 500-row authenticated audit history.
#[allow(clippy::too_many_arguments)]
async fn record_unauth_audit(
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
    let allowed = {
        let mut limiter = ctx.rate_limiter.lock().await;
        limiter.allow_unauth_audit(Instant::now())
    };
    if !allowed {
        return;
    }
    record_audit(
        ctx,
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
    .await;
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

    // #4: Reject unauthenticated access consistently with /v1/messages/send.
    let is_authed = {
        let sess = ctx.sess_arc.lock().await;
        sess.is_authenticated()
    };
    if !is_authed {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
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
    // #37: Permit EITHER `read_threads` OR `schedule` scope.
    if !client_has_scope(client, "read_threads") && !client_has_scope(client, "schedule") {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
            403,
            "forbidden_scope",
            "Missing required scope: read_threads or schedule",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            403,
            "forbidden_scope",
            "Missing required scope: read_threads or schedule",
        );
    }

    // #4: Reject unauthenticated access consistently with /v1/messages/send.
    let is_authed = {
        let sess = ctx.sess_arc.lock().await;
        sess.is_authenticated()
    };
    if !is_authed {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
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

    // #37: Filter `allowed_thread_ids` BEFORE SQL `LIMIT` and redact local `attachment_path`.
    let allowed_slice = (!client.allowed_thread_ids.is_empty())
        .then_some(client.allowed_thread_ids.as_slice());
    let schedules = {
        let store = ctx.store_arc.lock().await;
        match store.list_scheduled_messages_filtered(
            status_filter.as_deref(),
            allowed_slice,
            limit,
        ) {
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

    let sanitized: Vec<ApiScheduledMessageDto> = schedules
        .into_iter()
        .map(ApiScheduledMessageDto::from)
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
        &format!("returned {} schedules", sanitized.len()),
        now_ms,
    )
    .await;

    HttpResponse::json(
        200,
        &serde_json::json!({
            "ok": true,
            "schedules": sanitized,
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

    let request_hash = sha256_hex(
        serde_json::json!({
            "op": "send",
            "thread_id": thread_id,
            "kind": explicit_kind.map(thread_kind_str),
            "text": trimmed_text,
        })
        .to_string()
        .as_bytes(),
    );

    // #13: Prevent concurrent duplicate execution for the same (client_id, scoped_key)
    // and check 24h idempotency cache with canonical payload hash BEFORE send throttling.
    let _in_flight_guard = if let Some(ref scoped_key) = idem_key {
        let Some(guard) =
            InFlightIdempotencyGuard::try_acquire(&ctx.in_flight_idempotency, &client.id, scoped_key)
        else {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                409,
                "idempotency_in_flight",
                "Concurrent request with same Idempotency-Key already in flight",
                now_ms,
            )
            .await;
            return HttpResponse::error(
                409,
                "idempotency_in_flight",
                "A request with this Idempotency-Key is already in progress",
            );
        };
        let cached = {
            let store = ctx.store_arc.lock().await;
            store
                .get_api_idempotency_record(&client.id, scoped_key, IDEMPOTENCY_TTL_MS, now_ms)
                .ok()
                .flatten()
        };
        if let Some(rec) = cached {
            if !rec.request_hash.is_empty() && rec.request_hash != request_hash {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    thread_id,
                    409,
                    "idempotency_payload_mismatch",
                    "Idempotency-Key reused with different request payload",
                    now_ms,
                )
                .await;
                return HttpResponse::error(
                    409,
                    "idempotency_payload_mismatch",
                    "Idempotency-Key was already used with a different request payload",
                );
            }
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                rec.status_code,
                "idempotent_replay",
                "Returned cached idempotent response",
                now_ms,
            )
            .await;
            return HttpResponse::raw_json(rec.status_code, rec.response_json)
                .with_header("Idempotency-Replayed", "true");
        }
        Some(guard)
    } else {
        None
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

    let expected_uid = sess_snapshot.profile.uid.clone();

    let kind = match explicit_kind {
        Some(k) => k,
        None => {
            let store = ctx.store_arc.lock().await;
            store
                .get_thread_kind(thread_id)
                .ok()
                .flatten()
                .unwrap_or(ThreadKind::Direct)
        }
    };

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
            // #6: Re-check that the active account UID has not changed mid-send before
            // mutating `store_arc` or emitting `core://message-new`.
            let same_account = {
                let sess = ctx.sess_arc.lock().await;
                sess.is_authenticated() && sess.profile.uid == expected_uid
            };
            if !same_account {
                return HttpResponse::error(
                    409,
                    "account_switched",
                    "Teza account changed while sending message",
                );
            }

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
                // #32: Preserve Group/Cloud thread kind when creating uncached thread preview rows.
                let _ = store.update_thread_preview_with_fallback_kind_and_meta(
                    &msg.thread_id,
                    &preview,
                    msg.ts,
                    false,
                    true,
                    Some(kind),
                    None,
                    None,
                );
                if let Some(ref scoped_key) = idem_key {
                    if let Err(err) = store.put_api_idempotency_with_hash(
                        &client.id,
                        scoped_key,
                        200,
                        &resp_body,
                        &request_hash,
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
            let expired = is_expired_err(&err);
            let same_account = {
                let sess = ctx.sess_arc.lock().await;
                sess.is_authenticated() && sess.profile.uid == expected_uid
            };
            let (status, code, detail) = if expired {
                (
                    503,
                    "session_expired",
                    "Session expired; user must re-authenticate".to_string(),
                )
            } else {
                (502, "send_failed", format!("Upstream send failed: {err}"))
            };
            if same_account {
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
                // #5: Clear expired session, swap store to anonymous DB, and emit `core://auth-expired`.
                if expired {
                    ctx.sink
                        .clear_expired_session(
                            "Phiên đăng nhập đã hết hạn. Vui lòng quét lại mã QR.".to_string(),
                        )
                        .await;
                }
            }
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

    // #4: Reject unauthenticated access consistently with /v1/messages/send.
    let is_authed = {
        let sess = ctx.sess_arc.lock().await;
        sess.is_authenticated()
    };
    if !is_authed {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            "",
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

    // #10: Reject past timestamps (<= now_ms) and timestamps > 5 years in the future.
    let normalized_send_at_ts = normalize_ts_ms(body.send_at_ts);
    let max_allowed_ts = now_ms.saturating_add(MAX_SCHEDULE_FUTURE_MS);
    if normalized_send_at_ts <= now_ms || normalized_send_at_ts > max_allowed_ts {
        record_audit(
            ctx,
            &client.id,
            &client.name,
            &req.method,
            &req.path,
            thread_id,
            400,
            "invalid_send_at_ts",
            "send_at_ts must be in the future and within 5 years",
            now_ms,
        )
        .await;
        return HttpResponse::error(
            400,
            "invalid_send_at_ts",
            "send_at_ts must be a future timestamp within 5 years",
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

    let explicit_title = body
        .thread_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let effective_grace_secs = body.misfire_grace_secs.unwrap_or(600).max(0);

    let request_hash = sha256_hex(
        serde_json::json!({
            "op": "schedule",
            "thread_id": thread_id,
            "kind": explicit_kind.map(thread_kind_str),
            "thread_title": explicit_title.as_deref(),
            "text": trimmed_text,
            "send_at_ts": normalized_send_at_ts,
            "recurrence": &recurrence,
            "misfire_policy": &misfire_policy,
            "misfire_grace_secs": effective_grace_secs,
        })
        .to_string()
        .as_bytes(),
    );

    // #13: In-flight concurrency guard + 24h idempotency replay with payload hash check.
    let _in_flight_guard = if let Some(ref scoped_key) = idem_key {
        let Some(guard) =
            InFlightIdempotencyGuard::try_acquire(&ctx.in_flight_idempotency, &client.id, scoped_key)
        else {
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                409,
                "idempotency_in_flight",
                "Concurrent request with same Idempotency-Key already in flight",
                now_ms,
            )
            .await;
            return HttpResponse::error(
                409,
                "idempotency_in_flight",
                "A request with this Idempotency-Key is already in progress",
            );
        };
        let cached = {
            let store = ctx.store_arc.lock().await;
            store
                .get_api_idempotency_record(&client.id, scoped_key, IDEMPOTENCY_TTL_MS, now_ms)
                .ok()
                .flatten()
        };
        if let Some(rec) = cached {
            if !rec.request_hash.is_empty() && rec.request_hash != request_hash {
                record_audit(
                    ctx,
                    &client.id,
                    &client.name,
                    &req.method,
                    &req.path,
                    thread_id,
                    409,
                    "idempotency_payload_mismatch",
                    "Idempotency-Key reused with different request payload",
                    now_ms,
                )
                .await;
                return HttpResponse::error(
                    409,
                    "idempotency_payload_mismatch",
                    "Idempotency-Key was already used with a different request payload",
                );
            }
            record_audit(
                ctx,
                &client.id,
                &client.name,
                &req.method,
                &req.path,
                thread_id,
                rec.status_code,
                "idempotent_replay",
                "Returned cached idempotent response",
                now_ms,
            )
            .await;
            return HttpResponse::raw_json(rec.status_code, rec.response_json)
                .with_header("Idempotency-Replayed", "true");
        }
        Some(guard)
    } else {
        None
    };

    let created = {
        let store = ctx.store_arc.lock().await;
        // #67b: Rely on `create_scheduled_message`'s built-in `lookup_thread_title_and_kind`
        // instead of scanning and decrypting the entire `threads` table.
        let input = NewScheduledMessageInput {
            thread_id: thread_id.to_string(),
            thread_title: explicit_title.unwrap_or_default(),
            thread_kind: explicit_kind
                .map(|k| thread_kind_str(k).to_string())
                .unwrap_or_default(),
            text: trimmed_text.to_string(),
            attachment_path: String::new(),
            send_at_ts: normalized_send_at_ts,
            recurrence,
            misfire_policy,
            misfire_grace_secs: effective_grace_secs,
        };
        let res = store.create_scheduled_message(&input);
        match res {
            Ok(sched) => {
                let api_sched = ApiScheduledMessageDto::from(sched.clone());
                let resp_body = serde_json::json!({
                    "ok": true,
                    "schedule": api_sched,
                })
                .to_string();
                if let Some(ref scoped_key) = idem_key {
                    if let Err(err) = store.put_api_idempotency_with_hash(
                        &client.id,
                        scoped_key,
                        200,
                        &resp_body,
                        &request_hash,
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
        sess_arc: Arc<Mutex<Session>>,
        store_arc: Arc<Mutex<MessageStore>>,
        scheduler: SchedulerHandle,
        events: Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn set_test_authenticated(sess_arc: &Arc<Mutex<Session>>, authed: bool) {
        let mut sess = sess_arc.lock().await;
        if authed {
            sess.zpw_sek = Some("test_zpw_sek".to_string());
            sess.secret_key = Some(vec![0x11u8; 32]);
            sess.profile.uid = "test_uid_1001".to_string();
        } else {
            sess.zpw_sek = None;
            sess.secret_key = None;
            sess.profile.uid.clear();
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
                sess_arc.clone(),
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
            sess_arc,
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

            // 4. GET /v1/health with valid token (200) while unauthenticated,
            //    and verify #4: threads / schedules / send / schedule all reject with 503 when unauthenticated.
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

            let (st_unauth_threads, _, body_unauth_threads) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/threads HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st_unauth_threads, 503);
            assert!(body_unauth_threads.contains("not_authenticated"));

            let (st_unauth_scheds, _, body_unauth_scheds) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/schedules HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\n\r\n"),
            )
            .await;
            assert_eq!(st_unauth_scheds, 503);
            assert!(body_unauth_scheds.contains("not_authenticated"));

            let future_ts_unauth = now_epoch_ms() + 300_000;
            let unauth_sched_body = format!(
                r#"{{"thread_id":"g_ops","text":"Blocked unauth","send_at_ts":{future_ts_unauth}}}"#
            );
            let (st_unauth_create, _, body_unauth_create) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {restricted_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{unauth_sched_body}",
                    unauth_sched_body.len()
                ),
            )
            .await;
            assert_eq!(st_unauth_create, 503);
            assert!(body_unauth_create.contains("not_authenticated"));

            // Valid text when Teza account is unauthenticated -> 503
            let send_payload = r#"{"thread_id":"u_allowed","text":"Hello"}"#;
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

            // Now authenticate session to test authenticated endpoints
            set_test_authenticated(&env.sess_arc, true).await;

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

    #[test]
    fn sha256_hex_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn non_ascii_bearer_header_never_panics_and_returns_401() {
        // #11: Byte offset 7 lands inside multi-byte UTF-8 char 'ê' — must never panic!
        assert_eq!(extract_bearer_token(Some("Bearerê1")), None);
        assert_eq!(extract_bearer_token(Some("Bearêr token")), None);
        assert_eq!(extract_bearer_token(Some("Bearer   valid_tok")), Some("valid_tok"));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            let port = env.bound_addr.port();
            let (st, _, body) = send_raw_http(
                env.bound_addr,
                &format!(
                    "GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearerê1\r\n\r\n"
                ),
            )
            .await;
            assert_eq!(st, 401);
            assert!(body.contains("unauthorized"));
        });
    }

    #[test]
    fn expect_100_continue_writes_continue_and_reads_body_cleanly() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            set_test_authenticated(&env.sess_arc, true).await;
            let port = env.bound_addr.port();
            let token = {
                let store = env.store_arc.lock().await;
                store
                    .create_api_client("Curl Client", &["schedule".into()], &[], 30)
                    .unwrap()
                    .raw_token
            };

            let future_ts = now_epoch_ms() + 600_000;
            let long_msg = "Xin chào ".repeat(150);
            let payload = format!(
                r#"{{"thread_id":"u_allowed","text":"{long_msg}","send_at_ts":{future_ts}}}"#
            );
            assert!(payload.len() > 1024);

            let mut stream = TcpStream::connect(env.bound_addr).await.unwrap();
            let headers = format!(
                "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nExpect: 100-continue\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                payload.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();

            // Server MUST reply `HTTP/1.1 100 Continue\r\n\r\n` promptly before client sends body.
            let mut cont_buf = [0u8; 64];
            let n = tokio::time::timeout(Duration::from_millis(500), stream.read(&mut cont_buf))
                .await
                .expect("timed out waiting for 100 Continue")
                .unwrap();
            let cont_str = std::str::from_utf8(&cont_buf[..n]).unwrap();
            assert_eq!(cont_str, "HTTP/1.1 100 Continue\r\n\r\n");

            // Now send body and verify final 200 OK response.
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            let mut resp_buf = Vec::new();
            stream.read_to_end(&mut resp_buf).await.unwrap();
            let resp_text = String::from_utf8(resp_buf).unwrap();
            assert!(resp_text.starts_with("HTTP/1.1 200 OK\r\n"), "got: {resp_text}");
            let (_, body) = resp_text.split_once("\r\n\r\n").unwrap();
            let j: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(j["ok"], true);
            assert_eq!(j["schedule"]["thread_id"], "u_allowed");
        });
    }

    #[test]
    fn duplicate_security_headers_rejected_with_400() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            let port = env.bound_addr.port();

            let cases = [
                format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: evil.test\r\n\r\n"),
                format!("POST /v1/messages/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{{}}"),
                format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nSec-Fetch-Site: cross-site\r\nSec-Fetch-Site: none\r\n\r\n"),
                format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.test\r\nOrigin: https://other.test\r\n\r\n"),
                format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer a\r\nAuthorization: Bearer b\r\n\r\n"),
                format!("POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nIdempotency-Key: k1\r\nIdempotency-Key: k2\r\n\r\n"),
            ];

            for raw in cases {
                let (st, _, body) = send_raw_http(env.bound_addr, &raw).await;
                assert_eq!(st, 400, "expected 400 for duplicate header, got {st}: {body}");
                assert!(body.contains("duplicate_header"), "body={body}");
            }
        });
    }

    #[test]
    fn schedule_rejects_past_and_too_far_future_timestamps() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            set_test_authenticated(&env.sess_arc, true).await;
            let port = env.bound_addr.port();
            let token = {
                let store = env.store_arc.lock().await;
                store
                    .create_api_client("Sched Only", &["schedule".into()], &[], 30)
                    .unwrap()
                    .raw_token
            };

            let now = now_epoch_ms();
            let invalid_timestamps = [
                1_i64,
                now - 10_000,
                now.saturating_add(MAX_SCHEDULE_FUTURE_MS + 60_000),
            ];
            for bad_ts in invalid_timestamps {
                let payload = format!(
                    r#"{{"thread_id":"u_allowed","text":"Past or too far","send_at_ts":{bad_ts}}}"#
                );
                let (st, _, body) = send_raw_http(
                    env.bound_addr,
                    &format!(
                        "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                        payload.len()
                    ),
                )
                .await;
                assert_eq!(st, 400, "ts={bad_ts} should be rejected with 400");
                assert!(body.contains("invalid_send_at_ts"), "body={body}");
            }
        });
    }

    #[test]
    fn idempotency_payload_mismatch_and_in_flight_return_409() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            set_test_authenticated(&env.sess_arc, true).await;
            let port = env.bound_addr.port();
            let created = {
                let store = env.store_arc.lock().await;
                store
                    .create_api_client("Idem Bot", &["schedule".into()], &[], 30)
                    .unwrap()
            };
            let token = created.raw_token;
            let client_id = created.client.id;

            let future_ts = now_epoch_ms() + 600_000;
            let payload_a = format!(
                r#"{{"thread_id":"u_allowed","text":"Payload A","send_at_ts":{future_ts}}}"#
            );
            let payload_b = format!(
                r#"{{"thread_id":"u_allowed","text":"Payload B DIFFERENT","send_at_ts":{future_ts}}}"#
            );

            // First request succeeds
            let (st1, _, body1) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: teza-job-001\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload_a}",
                    payload_a.len()
                ),
            )
            .await;
            assert_eq!(st1, 200, "body={body1}");

            // Same key + DIFFERENT payload -> 409 idempotency_payload_mismatch
            let (st2, _, body2) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: teza-job-001\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload_b}",
                    payload_b.len()
                ),
            )
            .await;
            assert_eq!(st2, 409, "body={body2}");
            assert!(body2.contains("idempotency_payload_mismatch"), "body={body2}");

            // Simulate concurrent in-flight key held -> 409 idempotency_in_flight
            let _held = InFlightIdempotencyGuard::try_acquire(
                &env.manager.in_flight_idempotency,
                &client_id,
                "schedule:in-flight-key",
            )
            .expect("guard should acquire");

            let (st3, _, body3) = send_raw_http(
                env.bound_addr,
                &format!(
                    "POST /v1/messages/schedule HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: in-flight-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload_a}",
                    payload_a.len()
                ),
            )
            .await;
            assert_eq!(st3, 409, "body={body3}");
            assert!(body3.contains("idempotency_in_flight"), "body={body3}");
        });
    }

    #[test]
    fn list_schedules_redacts_attachment_path_allows_schedule_scope_and_filters_before_limit() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            set_test_authenticated(&env.sess_arc, true).await;
            let port = env.bound_addr.port();

            let sched_only_token = {
                let store = env.store_arc.lock().await;
                let client = store
                    .create_api_client(
                        "Schedule Scope Only",
                        &["schedule".into()],
                        &["u_allowed".into()],
                        30,
                    )
                    .unwrap();

                // Seed 3 earlier schedules on `u_secret` and 1 later schedule on `u_allowed`
                // with a sensitive Windows local attachment path.
                let base = now_epoch_ms() + 100_000;
                for i in 0..3 {
                    store
                        .create_scheduled_message(&NewScheduledMessageInput {
                            thread_id: "u_secret".into(),
                            thread_title: "Riêng Tư".into(),
                            thread_kind: "direct".into(),
                            text: format!("Secret {i}"),
                            attachment_path: String::new(),
                            send_at_ts: base + (i * 1_000),
                            recurrence: "once".into(),
                            misfire_policy: "send_on_resume".into(),
                            misfire_grace_secs: 600,
                        })
                        .unwrap();
                }
                store
                    .create_scheduled_message(&NewScheduledMessageInput {
                        thread_id: "u_allowed".into(),
                        thread_title: "Nguyễn Văn An".into(),
                        thread_kind: "direct".into(),
                        text: "Allowed with attachment".into(),
                        attachment_path: r"C:\Users\Alice\Private\invoice.pdf".into(),
                        send_at_ts: base + 50_000,
                        recurrence: "once".into(),
                        misfire_policy: "send_on_resume".into(),
                        misfire_grace_secs: 600,
                    })
                    .unwrap();

                client.raw_token
            };

            // Client with ONLY `schedule` scope (no `read_threads`) + `limit=1` MUST get `u_allowed`
            // (proving allowed_thread_ids is filtered BEFORE SQL LIMIT) with `attachment_path` redacted.
            let (st, _, body) = send_raw_http(
                env.bound_addr,
                &format!(
                    "GET /v1/schedules?status=pending&limit=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {sched_only_token}\r\n\r\n"
                ),
            )
            .await;
            assert_eq!(st, 200, "body={body}");
            assert!(!body.contains("attachment_path"), "must not leak attachment_path: {body}");
            assert!(!body.contains("invoice.pdf"), "must not leak local file path: {body}");

            let j: serde_json::Value = serde_json::from_str(&body).unwrap();
            let list = j["schedules"].as_array().unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0]["thread_id"], "u_allowed");
            assert_eq!(list[0]["has_attachment"], true);
            assert!(list[0].get("attachment_path").is_none());
        });
    }

    #[test]
    fn status_exposes_last_error_and_occupied_port_preserves_working_listener() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            let working_port = env.bound_addr.port();

            // Initially no error
            let st0 = env.manager.status(&env.store_arc).await;
            assert!(st0.running);
            assert_eq!(st0.last_error, None);

            // Occupy another ephemeral port so binding it MUST fail
            let blocker = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap();
            let occupied_port = blocker.local_addr().unwrap().port();

            // #35: Attempting to switch running listener to an occupied port MUST fail
            // WITHOUT killing the already working listener on `working_port`!
            let bad_cfg = LocalApiConfig {
                enabled: true,
                port: occupied_port,
                min_send_interval_ms: 1_500,
            };
            let switch_res = env
                .manager
                .apply_config_with_sink(
                    bad_cfg,
                    EventSink::capture(env.events.clone()),
                    env.sess_arc.clone(),
                    env.store_arc.clone(),
                    env.scheduler.clone(),
                )
                .await;
            assert!(switch_res.is_err(), "switching to occupied port must fail");

            let st_still_running = env.manager.status(&env.store_arc).await;
            assert!(st_still_running.running, "old working listener must stay running");
            assert_eq!(st_still_running.port, working_port);
            assert!(
                TcpStream::connect(env.bound_addr).await.is_ok(),
                "old working listener must still accept connections"
            );

            // #15: Stopping then failing to start on occupied port exposes `last_error` in status DTO!
            assert!(env.manager.stop().await);
            let fail_start = env
                .manager
                .start_internal(
                    occupied_port,
                    EventSink::capture(env.events.clone()),
                    env.sess_arc.clone(),
                    env.store_arc.clone(),
                    env.scheduler.clone(),
                )
                .await;
            assert!(fail_start.is_err());

            let st_err = env.manager.status(&env.store_arc).await;
            assert!(!st_err.running);
            assert!(
                st_err
                    .last_error
                    .as_deref()
                    .is_some_and(|e| e.contains(&occupied_port.to_string())),
                "status.last_error must expose bind failure: {:?}",
                st_err.last_error
            );

            // Clearing via stop/kill_switch clears `last_error`
            let st_cleared = env.manager.kill_switch(&env.store_arc).await.unwrap();
            assert_eq!(st_cleared.last_error, None);
            drop(blocker);
        });
    }

    #[test]
    fn unauth_audit_rate_limit_path_truncation_and_403_auditing() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let env = setup_test_env().await;
            let port = env.bound_addr.port();

            // 1. 403 forbidden_host and 403 forbidden_origin are audited, and long UTF-8 paths truncated to 256 chars.
            let long_path = format!("/v1/{}", "ê".repeat(300));
            let (st_host, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET {long_path} HTTP/1.1\r\nHost: evil.attacker.test:{port}\r\n\r\n"),
            )
            .await;
            assert_eq!(st_host, 403);

            let (st_origin, _, _) = send_raw_http(
                env.bound_addr,
                &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.test\r\n\r\n"),
            )
            .await;
            assert_eq!(st_origin, 403);

            // 2. Flood 30 unauthenticated 401 requests -> total unauth audit rows MUST be capped at MAX_UNAUTH_AUDIT_PER_MIN (20).
            for _ in 0..30 {
                let (st_401, _, _) = send_raw_http(
                    env.bound_addr,
                    &format!("GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
                )
                .await;
                assert_eq!(st_401, 401);
            }

            let logs = {
                let store = env.store_arc.lock().await;
                store.list_api_audit_logs(100).unwrap()
            };
            assert_eq!(
                logs.len(),
                MAX_UNAUTH_AUDIT_PER_MIN,
                "unauthenticated audit writes must be capped at {MAX_UNAUTH_AUDIT_PER_MIN}/min"
            );
            assert!(
                logs.iter().any(|l| l.outcome == "forbidden_host" && l.status_code == 403),
                "403 forbidden_host must be audited"
            );
            assert!(
                logs.iter().any(|l| l.outcome == "forbidden_origin" && l.status_code == 403),
                "403 forbidden_origin must be audited"
            );
            let truncated = logs
                .iter()
                .find(|l| l.outcome == "forbidden_host")
                .expect("host audit entry");
            assert_eq!(
                truncated.path.chars().count(),
                MAX_AUDIT_PATH_CHARS,
                "logged audit path must be truncated to {MAX_AUDIT_PATH_CHARS} chars"
            );
        });
    }
}
