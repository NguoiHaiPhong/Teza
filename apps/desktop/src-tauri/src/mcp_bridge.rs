//! Built-in loopback MCP (Model Context Protocol, JSON-RPC 2.0 over HTTP/1.1) server
//! connecting ZeroClaw v0.9+ ACP sessions to Teza's encrypted SQLite store, Zalo session,
//! MediaVault, scheduler, and controlled Windows/Office system tools.
//!
//! Security & Architecture invariants:
//! - Binds **STRICTLY** to `127.0.0.1:0` (OS-assigned ephemeral loopback port).
//! - Requires per-launch CSPRNG 256-bit hex `Authorization: Bearer <token>` verified in constant time.
//! - DNS rebinding protection (`Host` must be `127.0.0.1:<port>` or `localhost:<port>`).
//! - Browser CSRF protection (rejects ANY `Origin` header and any `Sec-Fetch-Site` != `none`).
//! - Dynamic 2-axis tool exposure (`TezaAccessLevel` T0–T3 × `SystemAccessLevel` S0–S2).
//! - Interactive 1-click Approval Queue (`McpApprovalManager`) executed directly inside the bridge
//!   for `Supervised` tiers and high-risk destructive actions (`leave_group` / `kick_group_member`
//!   ALWAYS require per-action approval even in T3).
//! - Hard `forbidden_paths` protection blocking OS system folders, `~/.ssh`, `~/.gnupg`, `~/.aws`,
//!   `~/.config`, and Teza's private data directory (except `<data_dir>/ai_workspace`) even in S2.
//! - Active PowerShell process-tree tracking (`ActivePsTracker`) killed via `taskkill /F /T` on
//!   timeout, E-Stop, or graceful app shutdown.
//! - Zero direct references to `AppState` or `crate::clear_expired_session_for_app` inside code
//!   reachable from unit tests (uses `McpEventSink`, matching `local_api.rs`), keeping
//!   `cargo test -p litechat-desktop --lib` 100% native on Windows without loading `wry`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as B64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch, Mutex, RwLock, Semaphore};
use tokio::task::JoinHandle;

use crate::file_dialog;
use crate::scheduler::SchedulerHandle;
use zalo_core::ai::{
    managed_ai_workspace_dir, sanitize_zeroclaw_allowed_roots, AiConfig, SystemAccessLevel,
    TezaAccessLevel, ZeroClawMcpBridgeConfig,
};
use zalo_core::chat::messaging;
use zalo_core::chat::parser::{
    canonicalize_reaction_emoji, clean_group_id, format_thread_preview, normalize_ts_ms,
};
use zalo_core::chat::threads;
use zalo_core::chat::{strip_vi_accents, FolderFilter, MessageDto, ThreadDto, ThreadKind};
use zalo_core::data_dir::StoragePaths;
use zalo_core::media_vault::{constant_time_eq, generate_protocol_token, MediaVault};
use zalo_core::session::{Session, SessionError};
use zalo_core::store::{
    MessageStore, NewScheduledMessageInput, MEDIA_LOCAL_STATE_LOCAL,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MCP_SERVER_NAME: &str = "teza";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024; // 2 MiB
const MAX_HEADER_COUNT: usize = 64;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const MAX_BEARER_TOKEN_BYTES: usize = 512;
const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 180;
const MAX_AUDIT_ENTRIES: usize = 200;
const MAX_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024; // 50 MiB
const MAX_PS_COMMAND_BYTES: usize = 32 * 1024;
const MAX_PS_OUTPUT_BYTES: usize = 128 * 1024; // 128 KiB per stream
const MAX_WIN_READ_BYTES_DEFAULT: usize = 131_072;
const MAX_WIN_READ_BYTES_CAP: usize = 524_288;
const MAX_SCHEDULE_FUTURE_MS: i64 = 5 * 365 * 24 * 3_600 * 1_000;

const SINGLE_VALUE_SECURITY_HEADERS: [&str; 6] = [
    "host",
    "content-length",
    "sec-fetch-site",
    "origin",
    "authorization",
    "transfer-encoding",
];

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static MCP_SEQ: AtomicU64 = AtomicU64::new(1);

const TEZA_READ_TOOLS: [&str; 10] = [
    "get_account_info",
    "list_threads",
    "get_thread_details",
    "get_messages",
    "search_messages",
    "list_friends",
    "list_folders",
    "list_schedules",
    "export_attachment",
    "draft_message",
];

const TEZA_CONTROL_TOOLS: [&str; 11] = [
    "send_message",
    "send_attachment",
    "react_message",
    "recall_or_delete_message",
    "manage_thread_state",
    "pin_chat_message",
    "create_schedule",
    "manage_schedule",
    "create_poll",
    "create_reminder",
    "manage_social",
];

const SYSTEM_TOOLS: [&str; 5] = [
    "win_list_dir",
    "win_read_file",
    "win_write_file",
    "win_run_powershell",
    "win_open_path",
];

// ============================================================================
// Public DTOs (Exact Contract with Frontend `aiPermissions.ts` & `bridge.ts`)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiToolApprovalRequest {
    pub approval_id: String,
    pub session_id: String,
    pub tool_name: String,
    pub category: String,
    pub risk: String,
    pub title: String,
    pub summary: String,
    pub details: Value,
    pub created_ts: i64,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiToolApprovalResolvedEvent {
    pub approval_id: String,
    pub session_id: String,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiToolAuditEntry {
    pub id: String,
    pub ts: i64,
    pub session_id: String,
    pub tool_name: String,
    pub category: String,
    pub decision: String,
    pub ok: bool,
    pub summary: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiMcpStatusDto {
    pub running: bool,
    pub exposed_tools_count: usize,
    pub pending_approvals_count: usize,
    pub active_powershell_count: usize,
    pub workspace_dir: String,
    pub forbidden_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiEmergencyStopResult {
    pub cancelled_approvals: usize,
    pub killed_processes: usize,
}

// ============================================================================
// Event Sink & Dependency Bundle (Zero `AppState` / `wry` coupling in tests)
// ============================================================================

type EventEmitFn = dyn Fn(&str, Value) + Send + Sync;
type SessionExpiredFn =
    dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;
type GDriveNotifyFn = dyn Fn() + Send + Sync;

#[derive(Clone)]
pub struct McpEventSink {
    emit_fn: Arc<EventEmitFn>,
    expire_session_fn: Option<Arc<SessionExpiredFn>>,
    gdrive_notify_fn: Option<Arc<GDriveNotifyFn>>,
}

impl McpEventSink {
    pub fn from_app(app: AppHandle, gdrive_notify: Arc<GDriveNotifyFn>) -> Self {
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
            gdrive_notify_fn: Some(gdrive_notify),
        }
    }

    #[cfg(test)]
    pub fn capture(buf: Arc<std::sync::Mutex<Vec<(String, Value)>>>) -> Self {
        Self {
            emit_fn: Arc::new(move |event, payload| {
                if let Ok(mut guard) = buf.lock() {
                    guard.push((event.to_string(), payload));
                }
            }),
            expire_session_fn: None,
            gdrive_notify_fn: None,
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

    fn notify_gdrive_sync(&self) {
        if let Some(ref f) = self.gdrive_notify_fn {
            (f)();
        }
    }
}

#[derive(Clone)]
pub struct McpBridgeDeps {
    pub session: Arc<Mutex<Session>>,
    pub store: Arc<Mutex<MessageStore>>,
    pub media_vault: Arc<RwLock<Arc<MediaVault>>>,
    pub ai_config: Arc<Mutex<AiConfig>>,
    pub active_thread_id: Arc<Mutex<Option<String>>>,
    pub scheduler: SchedulerHandle,
}

// ============================================================================
// Approval Manager (`McpApprovalManager`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalOutcome {
    Auto,
    AllowOnce,
    AllowSession,
    Deny,
    Timeout,
    Cancelled,
}

impl ApprovalOutcome {
    fn audit_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AllowOnce => "allow_once",
            Self::AllowSession => "allow_session",
            Self::Deny => "deny",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_allowed(self) -> bool {
        matches!(self, Self::Auto | Self::AllowOnce | Self::AllowSession)
    }
}

struct PendingApproval {
    tx: oneshot::Sender<ApprovalOutcome>,
    session_id: String,
    risk: String,
    grant_key: String,
}

pub struct McpApprovalManager {
    pending: std::sync::Mutex<HashMap<String, PendingApproval>>,
    session_grants: std::sync::Mutex<HashSet<(String, String)>>,
    timeout_secs: u64,
}

impl McpApprovalManager {
    fn new(timeout_secs: u64) -> Self {
        Self {
            pending: std::sync::Mutex::new(HashMap::new()),
            session_grants: std::sync::Mutex::new(HashSet::new()),
            timeout_secs: timeout_secs.max(1),
        }
    }

    fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn clear_session_grants(&self) {
        self.session_grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn is_high_risk_or_destructive(risk: &str, tool_name: &str, grant_key: &str) -> bool {
        risk.eq_ignore_ascii_case("high")
            || matches!(
                tool_name,
                "leave_group" | "kick_group_member" | "kick_member"
            )
            || grant_key.ends_with(":leave_group")
            || grant_key.ends_with(":kick_group_member")
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_approval(
        &self,
        sink: &McpEventSink,
        session_id: &str,
        tool_name: &str,
        grant_key: &str,
        category: &str,
        risk: &str,
        title: String,
        summary: String,
        details: Value,
    ) -> ApprovalOutcome {
        let high_risk = Self::is_high_risk_or_destructive(risk, tool_name, grant_key);
        if !high_risk {
            let grants = self
                .session_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if grants.contains(&(session_id.to_string(), grant_key.to_string())) {
                return ApprovalOutcome::AllowSession;
            }
        }

        let created_ts = now_ms();
        let seq = MCP_SEQ.fetch_add(1, Ordering::Relaxed);
        let approval_id = format!("apr-{created_ts}-{seq}");
        let (tx, rx) = oneshot::channel::<ApprovalOutcome>();

        {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                approval_id.clone(),
                PendingApproval {
                    tx,
                    session_id: session_id.to_string(),
                    risk: risk.to_string(),
                    grant_key: grant_key.to_string(),
                },
            );
        }

        let payload = AiToolApprovalRequest {
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            tool_name: tool_name.to_string(),
            category: category.to_string(),
            risk: risk.to_string(),
            title,
            summary,
            details,
            created_ts,
            timeout_secs: self.timeout_secs,
        };
        sink.emit_json("core://ai-tool-approval", &payload);

        match tokio::time::timeout(Duration::from_secs(self.timeout_secs), rx).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => ApprovalOutcome::Cancelled,
            Err(_) => {
                let removed = {
                    let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                    map.remove(&approval_id)
                };
                if let Some(entry) = removed {
                    sink.emit_json(
                        "core://ai-tool-approval-resolved",
                        &AiToolApprovalResolvedEvent {
                            approval_id,
                            session_id: entry.session_id,
                            decision: "timeout".to_string(),
                        },
                    );
                }
                ApprovalOutcome::Timeout
            }
        }
    }

    fn resolve_approval(
        &self,
        sink: Option<&McpEventSink>,
        approval_id: &str,
        decision: &str,
    ) -> bool {
        let normalized = decision.trim().to_ascii_lowercase();
        let requested = match normalized.as_str() {
            "allow_once" => ApprovalOutcome::AllowOnce,
            "allow_session" => ApprovalOutcome::AllowSession,
            "deny" => ApprovalOutcome::Deny,
            _ => return false,
        };

        let entry = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(approval_id.trim())
        };
        let Some(entry) = entry else {
            return false;
        };

        // Never allow `allow_session` escalation on high-risk or destructive operations.
        let effective = if requested == ApprovalOutcome::AllowSession
            && Self::is_high_risk_or_destructive(&entry.risk, "", &entry.grant_key)
        {
            ApprovalOutcome::AllowOnce
        } else {
            requested
        };

        if effective == ApprovalOutcome::AllowSession {
            let mut grants = self
                .session_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            grants.insert((entry.session_id.clone(), entry.grant_key.clone()));
        }

        if let Some(s) = sink {
            s.emit_json(
                "core://ai-tool-approval-resolved",
                &AiToolApprovalResolvedEvent {
                    approval_id: approval_id.trim().to_string(),
                    session_id: entry.session_id.clone(),
                    decision: effective.audit_str().to_string(),
                },
            );
        }

        let _ = entry.tx.send(effective);
        true
    }

    fn cancel_session_approvals(
        &self,
        sink: Option<&McpEventSink>,
        session_id: &str,
        clear_grants: bool,
    ) -> usize {
        let clean_sid = session_id.trim();
        let removed: Vec<(String, PendingApproval)> = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<String> = map
                .iter()
                .filter(|(_, entry)| entry.session_id == clean_sid)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| map.remove(&k).map(|v| (k, v)))
                .collect()
        };
        if clear_grants {
            let mut grants = self
                .session_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            grants.retain(|(sid, _)| sid != clean_sid);
        }
        let count = removed.len();
        for (approval_id, entry) in removed {
            if let Some(s) = sink {
                s.emit_json(
                    "core://ai-tool-approval-resolved",
                    &AiToolApprovalResolvedEvent {
                        approval_id,
                        session_id: entry.session_id,
                        decision: "cancelled".to_string(),
                    },
                );
            }
            let _ = entry.tx.send(ApprovalOutcome::Cancelled);
        }
        count
    }

    fn cancel_all_approvals(&self, sink: Option<&McpEventSink>) -> usize {
        let drained: Vec<(String, PendingApproval)> = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().collect()
        };
        let count = drained.len();
        for (approval_id, entry) in drained {
            if let Some(s) = sink {
                s.emit_json(
                    "core://ai-tool-approval-resolved",
                    &AiToolApprovalResolvedEvent {
                        approval_id,
                        session_id: entry.session_id,
                        decision: "cancelled".to_string(),
                    },
                );
            }
            let _ = entry.tx.send(ApprovalOutcome::Cancelled);
        }
        count
    }
}

// ============================================================================
// Active PowerShell Process Tree Tracker (`ActivePsTracker`)
// ============================================================================

struct ActivePsTracker {
    pids: std::sync::Mutex<HashSet<u32>>,
    estop_tx: watch::Sender<u64>,
}

impl ActivePsTracker {
    fn new() -> Self {
        let (estop_tx, _) = watch::channel(0u64);
        Self {
            pids: std::sync::Mutex::new(HashSet::new()),
            estop_tx,
        }
    }

    fn register(&self, pid: u32) {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid);
    }

    fn unregister(&self, pid: u32) {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
    }

    fn count(&self) -> usize {
        self.pids.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn subscribe_estop(&self) -> watch::Receiver<u64> {
        self.estop_tx.subscribe()
    }

    async fn kill_all_processes(&self) -> usize {
        let next = self.estop_tx.borrow().wrapping_add(1);
        let _ = self.estop_tx.send(next);
        let pids: Vec<u32> = {
            let mut set = self.pids.lock().unwrap_or_else(|e| e.into_inner());
            set.drain().collect()
        };
        let count = pids.len();
        if count > 0 {
            let _ = tokio::task::spawn_blocking(move || {
                for pid in pids {
                    kill_process_tree_sync(pid);
                }
            })
            .await;
        }
        count
    }
}

struct TrackedPidGuard {
    tracker: Arc<ActivePsTracker>,
    pid: u32,
}

impl TrackedPidGuard {
    fn new(tracker: Arc<ActivePsTracker>, pid: u32) -> Self {
        tracker.register(pid);
        Self { tracker, pid }
    }
}

impl Drop for TrackedPidGuard {
    fn drop(&mut self) {
        self.tracker.unregister(self.pid);
    }
}

fn kill_process_tree_sync(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let taskkill_path = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|r| r.join("System32").join("taskkill.exe"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
        let _ = std::process::Command::new(taskkill_path)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

// ============================================================================
// TezaMcpBridge Manager
// ============================================================================

struct RunningMcpServer {
    shutdown_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
    bound_addr: SocketAddr,
    sink: McpEventSink,
}

pub struct TezaMcpBridge {
    paths: StoragePaths,
    bearer_token: String,
    runtime: Arc<Mutex<Option<RunningMcpServer>>>,
    lifecycle_lock: Arc<Mutex<()>>,
    approvals: Arc<McpApprovalManager>,
    ps_tracker: Arc<ActivePsTracker>,
    audit_log: Arc<std::sync::Mutex<VecDeque<AiToolAuditEntry>>>,
    active_session_id: Arc<std::sync::Mutex<String>>,
}

impl TezaMcpBridge {
    pub fn new(paths: StoragePaths) -> Self {
        Self::new_with_approval_timeout(paths, DEFAULT_APPROVAL_TIMEOUT_SECS)
    }

    fn new_with_approval_timeout(paths: StoragePaths, approval_timeout_secs: u64) -> Self {
        let bearer_token = generate_protocol_token().unwrap_or_else(|_| {
            // Fallback CSPRNG-free emergency token should never be reached on Windows/Unix.
            let t = now_ms();
            let s = MCP_SEQ.fetch_add(1, Ordering::Relaxed);
            format!("{t:032x}{s:032x}")
        });
        Self {
            paths,
            bearer_token,
            runtime: Arc::new(Mutex::new(None)),
            lifecycle_lock: Arc::new(Mutex::new(())),
            approvals: Arc::new(McpApprovalManager::new(approval_timeout_secs)),
            ps_tracker: Arc::new(ActivePsTracker::new()),
            audit_log: Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(
                MAX_AUDIT_ENTRIES,
            ))),
            active_session_id: Arc::new(std::sync::Mutex::new("default".to_string())),
        }
    }

    pub async fn start_with_app(
        &self,
        app: AppHandle,
        gdrive_notify: Arc<GDriveNotifyFn>,
        deps: McpBridgeDeps,
    ) -> Result<SocketAddr, String> {
        let sink = McpEventSink::from_app(app, gdrive_notify);
        self.start_with_sink(sink, deps).await
    }

    pub async fn start_with_sink(
        &self,
        sink: McpEventSink,
        deps: McpBridgeDeps,
    ) -> Result<SocketAddr, String> {
        let _lc = self.lifecycle_lock.lock().await;
        {
            let mut rt = self.runtime.lock().await;
            if let Some(srv) = rt.as_ref() {
                if !srv.join_handle.is_finished() {
                    return Ok(srv.bound_addr);
                }
                rt.take();
            }
        }

        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("Không thể khởi chạy Teza MCP Bridge trên loopback: {e}"))?;
        let actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Không đọc được địa chỉ cục bộ Teza MCP Bridge: {e}"))?;
        debug_assert!(actual_addr.ip().is_loopback());

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let ctx = Arc::new(McpServerContext {
            bound_port: actual_addr.port(),
            bearer_token: self.bearer_token.clone(),
            paths: self.paths.clone(),
            workspace_dir: managed_ai_workspace_dir(&self.paths.root),
            deps,
            sink: sink.clone(),
            approvals: self.approvals.clone(),
            ps_tracker: self.ps_tracker.clone(),
            audit_log: self.audit_log.clone(),
            active_session_id: self.active_session_id.clone(),
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
                                eprintln!("[teza][mcp-bridge] accept error: {err}");
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
                            let resp = HttpResponse::json(
                                503,
                                &json!({ "error": "server_busy", "message": "Too many concurrent MCP requests" }),
                            );
                            let _ = write_http_response(&mut busy_stream, &resp).await;
                            continue;
                        };
                        let ctx_conn = ctx.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            handle_mcp_connection(stream, ctx_conn).await;
                        });
                    }
                }
            }
            drop(listener);
        });

        *self.runtime.lock().await = Some(RunningMcpServer {
            shutdown_tx,
            join_handle,
            bound_addr: actual_addr,
            sink,
        });

        Ok(actual_addr)
    }

    pub async fn stop(&self) {
        let _lc = self.lifecycle_lock.lock().await;
        let running = { self.runtime.lock().await.take() };
        let sink = running.as_ref().map(|r| r.sink.clone());
        self.approvals.cancel_all_approvals(sink.as_ref());
        self.approvals.clear_session_grants();
        let _ = self.ps_tracker.kill_all_processes().await;
        if let Some(mut srv) = running {
            let _ = srv.shutdown_tx.send(true);
            if tokio::time::timeout(SHUTDOWN_JOIN_TIMEOUT, &mut srv.join_handle)
                .await
                .is_err()
            {
                srv.join_handle.abort();
                let _ = srv.join_handle.await;
            }
        }
    }

    pub fn set_active_session_id(&self, session_id: &str) {
        let clean = session_id.trim();
        let mut guard = self
            .active_session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = if clean.is_empty() {
            "default".to_string()
        } else {
            clean.to_string()
        };
    }

    pub async fn endpoint_config_for(&self, cfg: &AiConfig) -> Option<ZeroClawMcpBridgeConfig> {
        let exposed = exposed_tool_names_for(cfg.teza_access_level, cfg.system_access_level);
        if exposed.is_empty() {
            return None;
        }
        let addr = {
            let mut rt = self.runtime.lock().await;
            match rt.as_ref() {
                Some(srv) if !srv.join_handle.is_finished() => srv.bound_addr,
                Some(_) => {
                    rt.take();
                    return None;
                }
                None => return None,
            }
        };
        Some(ZeroClawMcpBridgeConfig {
            url: format!("http://127.0.0.1:{}/mcp", addr.port()),
            bearer_token: self.bearer_token.clone(),
            exposed_tool_names: exposed,
            workspace_dir: managed_ai_workspace_dir(&self.paths.root),
            teza_data_root: self.paths.root.clone(),
        })
    }

    pub async fn resolve_approval(&self, approval_id: &str, decision: &str) -> bool {
        let sink = {
            let rt = self.runtime.lock().await;
            rt.as_ref().map(|r| r.sink.clone())
        };
        self.approvals
            .resolve_approval(sink.as_ref(), approval_id, decision)
    }

    pub async fn emergency_stop(&self) -> AiEmergencyStopResult {
        let sink = {
            let rt = self.runtime.lock().await;
            rt.as_ref().map(|r| r.sink.clone())
        };
        let cancelled_approvals = self.approvals.cancel_all_approvals(sink.as_ref());
        self.approvals.clear_session_grants();
        let killed_processes = self.ps_tracker.kill_all_processes().await;
        AiEmergencyStopResult {
            cancelled_approvals,
            killed_processes,
        }
    }

    pub async fn reset_for_logout(&self) {
        let sink = {
            let rt = self.runtime.lock().await;
            rt.as_ref().map(|r| r.sink.clone())
        };
        self.approvals.cancel_all_approvals(sink.as_ref());
        self.approvals.clear_session_grants();
        let _ = self.ps_tracker.kill_all_processes().await;
    }

    #[allow(dead_code)]
    pub async fn clear_session_grants(&self) {
        self.approvals.clear_session_grants();
    }

    pub async fn cancel_session_approvals(&self, session_id: &str, clear_grants: bool) -> usize {
        let sink = {
            let rt = self.runtime.lock().await;
            rt.as_ref().map(|r| r.sink.clone())
        };
        self.approvals
            .cancel_session_approvals(sink.as_ref(), session_id, clear_grants)
    }

    pub fn list_audit_logs(&self, limit: Option<usize>) -> Vec<AiToolAuditEntry> {
        let capped = limit.unwrap_or(100).clamp(1, MAX_AUDIT_ENTRIES);
        let guard = self.audit_log.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().rev().take(capped).cloned().collect()
    }

    pub async fn status(&self, cfg: &AiConfig) -> AiMcpStatusDto {
        let server_running = {
            let mut rt = self.runtime.lock().await;
            match rt.as_ref() {
                Some(srv) if !srv.join_handle.is_finished() => true,
                Some(_) => {
                    rt.take();
                    false
                }
                None => false,
            }
        };
        let exposed = exposed_tool_names_for(cfg.teza_access_level, cfg.system_access_level);
        let active = server_running && !exposed.is_empty();
        let workspace_dir = file_dialog::normalize_os_display_path(&managed_ai_workspace_dir(
            &self.paths.root,
        ))
        .to_string_lossy()
        .into_owned();
        AiMcpStatusDto {
            running: active,
            exposed_tools_count: if active { exposed.len() } else { 0 },
            pending_approvals_count: self.approvals.pending_count(),
            active_powershell_count: self.ps_tracker.count(),
            workspace_dir,
            forbidden_paths: forbidden_paths_display(&self.paths.root),
        }
    }

    #[cfg(test)]
    fn bearer_token_for_test(&self) -> &str {
        &self.bearer_token
    }
}

struct McpServerContext {
    bound_port: u16,
    bearer_token: String,
    paths: StoragePaths,
    workspace_dir: PathBuf,
    deps: McpBridgeDeps,
    sink: McpEventSink,
    approvals: Arc<McpApprovalManager>,
    ps_tracker: Arc<ActivePsTracker>,
    audit_log: Arc<std::sync::Mutex<VecDeque<AiToolAuditEntry>>>,
    active_session_id: Arc<std::sync::Mutex<String>>,
}

impl McpServerContext {
    fn current_session_id(&self) -> String {
        self.active_session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn record_audit(
        &self,
        session_id: &str,
        tool_name: &str,
        category: &str,
        decision: &str,
        ok: bool,
        summary: String,
        error: Option<String>,
    ) {
        let ts = now_ms();
        let seq = MCP_SEQ.fetch_add(1, Ordering::Relaxed);
        let entry = AiToolAuditEntry {
            id: format!("aud-{ts}-{seq}"),
            ts,
            session_id: session_id.to_string(),
            tool_name: tool_name.to_string(),
            category: category.to_string(),
            decision: decision.to_string(),
            ok,
            summary: truncate_chars(&summary, 280),
            error: error.map(|e| truncate_chars(&e, 320)),
        };
        let mut guard = self.audit_log.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= MAX_AUDIT_ENTRIES {
            guard.pop_front();
        }
        guard.push_back(entry);
    }
}

// ============================================================================
// Tool Exposure Matrix & Schemas (26 Tools)
// ============================================================================

pub fn exposed_tool_names_for(
    teza: TezaAccessLevel,
    system: SystemAccessLevel,
) -> Vec<String> {
    let mut out = Vec::with_capacity(26);
    match teza {
        TezaAccessLevel::Isolated => {}
        TezaAccessLevel::ReadOnly => {
            out.extend(TEZA_READ_TOOLS.iter().map(|s| (*s).to_string()));
        }
        TezaAccessLevel::Supervised | TezaAccessLevel::Autonomous => {
            out.extend(TEZA_READ_TOOLS.iter().map(|s| (*s).to_string()));
            out.extend(TEZA_CONTROL_TOOLS.iter().map(|s| (*s).to_string()));
        }
    }
    match system {
        SystemAccessLevel::Safe => {}
        SystemAccessLevel::Supervised | SystemAccessLevel::Full => {
            out.extend(SYSTEM_TOOLS.iter().map(|s| (*s).to_string()));
        }
    }
    out
}

fn tool_category(tool_name: &str) -> &'static str {
    if SYSTEM_TOOLS.contains(&tool_name) {
        "system"
    } else {
        "teza"
    }
}

fn is_tool_exposed(tool_name: &str, teza: TezaAccessLevel, system: SystemAccessLevel) -> bool {
    if TEZA_READ_TOOLS.contains(&tool_name) {
        return teza != TezaAccessLevel::Isolated;
    }
    if TEZA_CONTROL_TOOLS.contains(&tool_name) {
        return matches!(teza, TezaAccessLevel::Supervised | TezaAccessLevel::Autonomous);
    }
    if SYSTEM_TOOLS.contains(&tool_name) {
        return matches!(system, SystemAccessLevel::Supervised | SystemAccessLevel::Full);
    }
    false
}

fn build_tool_schema(name: &str) -> Value {
    let (description, schema) = match name {
        // ---- Group A: Teza Read & Draft (T1+) ----
        "get_account_info" => (
            "Get current Teza account profile, authentication state, active conversation ID, total conversations, and unread counts.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        "list_threads" => (
            "List Teza conversations (direct friends, groups, Cloud) with optional kind, unread_only, query, and limit filters.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["all", "direct", "group", "cloud"] },
                    "unread_only": { "type": "boolean" },
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                }
            }),
        ),
        "get_thread_details" => (
            "Get metadata, pinned messages, shared media/files/links, and group members (for group chats) of a conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string", "description": "Conversation ID" }
                },
                "required": ["thread_id"]
            }),
        ),
        "get_messages" => (
            "Read decrypted chat message history of a Teza conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 150 },
                    "before_ts": { "type": "integer", "description": "Optional epoch timestamp (ms) cursor to load older messages" }
                },
                "required": ["thread_id"]
            }),
        ),
        "search_messages" => (
            "Accent-insensitive FTS5 full-text search across decrypted Teza messages (supports Vietnamese and Korean Choseong) plus matching threads.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "thread_id": { "type": "string", "description": "Optional thread ID to restrict message search" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "required": ["query"]
            }),
        ),
        "list_friends" => (
            "List direct contacts/friends from Teza and optionally live incoming/outgoing friend requests.",
            json!({
                "type": "object",
                "properties": {
                    "include_requests": { "type": "boolean" }
                }
            }),
        ),
        "list_folders" => (
            "List custom chat folders configured in Teza.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        "list_schedules" => (
            "List scheduled outgoing messages (pending, missed, failed, sent, cancelled).",
            json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                }
            }),
        ),
        "export_attachment" => (
            "Export a chat message attachment (e.g. .xlsx, .docx, .pdf, image) safely into the AI workspace folder so Windows/PowerShell tools can process it.",
            json!({
                "type": "object",
                "properties": {
                    "msg_id": { "type": "string" },
                    "thread_id": { "type": "string", "description": "Optional thread ID to speed up lookup" }
                },
                "required": ["msg_id"]
            }),
        ),
        "draft_message" => (
            "Pre-fill a draft message in Teza's chat composer without sending it.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "text": { "type": "string" },
                    "open_thread": { "type": "boolean" }
                },
                "required": ["thread_id", "text"]
            }),
        ),

        // ---- Group B: Teza Control (T2+) ----
        "send_message" => (
            "Send a text message (optionally replying to a previous message) in a Teza conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "text": { "type": "string" },
                    "reply_to_msg_id": { "type": "string" }
                },
                "required": ["thread_id", "text"]
            }),
        ),
        "send_attachment" => (
            "Send a local file or image (<= 50 MiB, outside forbidden system paths) to a Teza conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "file_path": { "type": "string" },
                    "caption": { "type": "string" }
                },
                "required": ["thread_id", "file_path"]
            }),
        ),
        "react_message" => (
            "Toggle an emoji reaction on a message in a Teza conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "msg_id": { "type": "string" },
                    "emoji": { "type": "string" }
                },
                "required": ["thread_id", "msg_id", "emoji"]
            }),
        ),
        "recall_or_delete_message" => (
            "Recall a sent message for everyone or delete a message locally.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "msg_id": { "type": "string" },
                    "action": { "type": "string", "enum": ["recall", "delete_local"] }
                },
                "required": ["thread_id", "msg_id", "action"]
            }),
        ),
        "manage_thread_state" => (
            "Update local/UI state of a conversation: mark_read, mute, unmute, pin, unpin, archive, unarchive, or open_thread.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "action": {
                        "type": "string",
                        "enum": ["mark_read", "mute", "unmute", "pin", "unpin", "archive", "unarchive", "open_thread"]
                    },
                    "mute_duration_secs": { "type": "integer", "minimum": 0 }
                },
                "required": ["thread_id", "action"]
            }),
        ),
        "pin_chat_message" => (
            "Pin or unpin a message in a Teza conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "msg_id": { "type": "string" },
                    "unpin": { "type": "boolean" }
                },
                "required": ["thread_id", "msg_id"]
            }),
        ),
        "create_schedule" => (
            "Schedule a future text and/or file message to be sent automatically by Teza.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "text": { "type": "string" },
                    "attachment_path": { "type": "string" },
                    "send_at_ts": { "type": "integer", "description": "Future Unix timestamp in ms or seconds" },
                    "recurrence": { "type": "string", "enum": ["once", "daily", "weekly", "monthly"] },
                    "misfire_policy": { "type": "string", "enum": ["ask", "skip", "send_on_resume"] }
                },
                "required": ["thread_id", "send_at_ts"]
            }),
        ),
        "manage_schedule" => (
            "Cancel, delete, or immediately trigger ('send_now') an existing scheduled message.",
            json!({
                "type": "object",
                "properties": {
                    "schedule_id": { "type": "string" },
                    "action": { "type": "string", "enum": ["cancel", "delete", "send_now"] }
                },
                "required": ["schedule_id", "action"]
            }),
        ),
        "create_poll" => (
            "Create a poll inside a Teza group conversation.",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" }, "minItems": 2 },
                    "allow_multi_choices": { "type": "boolean" },
                    "allow_add_new_option": { "type": "boolean" },
                    "is_anonymous": { "type": "boolean" },
                    "expired_time_ms": { "type": "integer" }
                },
                "required": ["thread_id", "question", "options"]
            }),
        ),
        "create_reminder" => (
            "Create a scheduled reminder in a Teza conversation (must be at least 30s in the future).",
            json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "title": { "type": "string" },
                    "start_time_ms": { "type": "integer", "description": "Future Unix epoch timestamp in milliseconds" },
                    "repeat_mode": { "description": "0=none, 1=daily, 2=weekly, 3=monthly (or string)" }
                },
                "required": ["thread_id", "title", "start_time_ms"]
            }),
        ),
        "manage_social" => (
            "Lookup users by phone, manage friend requests, or add/remove group members and leave groups.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "find_user_by_phone",
                            "send_friend_request",
                            "accept_friend_request",
                            "reject_friend_request",
                            "add_group_members",
                            "kick_group_member",
                            "leave_group"
                        ]
                    },
                    "phone": { "type": "string" },
                    "country_code": { "type": "string" },
                    "user_id": { "type": "string" },
                    "user_ids": { "type": "array", "items": { "type": "string" } },
                    "thread_id": { "type": "string" },
                    "message": { "type": "string" },
                    "silent": { "type": "boolean" }
                },
                "required": ["action"]
            }),
        ),

        // ---- Group C: Windows & Office System Tools (S1+) ----
        "win_list_dir" => (
            "List files and subdirectories inside an allowed Windows folder.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_entries": { "type": "integer", "minimum": 1, "maximum": 500 }
                },
                "required": ["path"]
            }),
        ),
        "win_read_file" => (
            "Read UTF-8 text content from an allowed Windows file (up to 512 KiB).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 524288 }
                },
                "required": ["path"]
            }),
        ),
        "win_write_file" => (
            "Create, overwrite, or append UTF-8 text content to a file inside an allowed Windows folder.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "append": { "type": "boolean" }
                },
                "required": ["path", "content"]
            }),
        ),
        "win_run_powershell" => (
            "Execute a UTF-8 Windows PowerShell command/script (supports Excel/Word COM, Python, PDF processing, automation) with output capture and timeout.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
                },
                "required": ["command"]
            }),
        ),
        "win_open_path" => (
            "Open a non-executable document (.xlsx, .docx, .pdf, image) or folder with the default Windows application.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
        _ => (
            "Teza MCP tool",
            json!({ "type": "object", "properties": {} }),
        ),
    };

    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

// ============================================================================
// Path Security (`allowed_roots` & Hard `forbidden_paths`)
// ============================================================================

fn normalize_path_lexical(path: &Path) -> Result<PathBuf, String> {
    let clean = file_dialog::normalize_os_display_path(path);
    let mut out = PathBuf::new();
    for comp in clean.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err("Đường dẫn vượt ra ngoài thư mục gốc cho phép".into());
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    Ok(out)
}

/// Canonicalizes the deepest existing ancestor to defeat symlink escapes while
/// still supporting not-yet-created target files/subdirectories (e.g. `win_write_file`).
fn resolve_path_safely(raw_path: &Path) -> Result<PathBuf, String> {
    let lexical = normalize_path_lexical(raw_path)?;
    let mut existing = lexical.as_path();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        tail.push(name.to_os_string());
        match existing.parent() {
            Some(p) if !p.as_os_str().is_empty() => existing = p,
            _ => break,
        }
    }

    let mut base = if existing.exists() {
        let canon = std::fs::canonicalize(existing)
            .map_err(|e| format!("Không thể chuẩn hóa đường dẫn: {e}"))?;
        file_dialog::normalize_os_display_path(&canon)
    } else {
        existing.to_path_buf()
    };

    for seg in tail.into_iter().rev() {
        base.push(seg);
    }
    Ok(base)
}

fn path_starts_with_ci(candidate: &Path, prefix: &Path) -> bool {
    let cand_norm = file_dialog::normalize_os_display_path(candidate);
    let pref_norm = file_dialog::normalize_os_display_path(prefix);
    let mut cand_it = cand_norm.components();
    for pref_comp in pref_norm.components() {
        let Some(cand_comp) = cand_it.next() else {
            return false;
        };
        #[cfg(target_os = "windows")]
        {
            let a = pref_comp.as_os_str().to_string_lossy().to_ascii_lowercase();
            let b = cand_comp.as_os_str().to_string_lossy().to_ascii_lowercase();
            if a != b {
                return false;
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            if pref_comp != cand_comp {
                return false;
            }
        }
    }
    true
}

fn user_home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
}

fn collect_hard_forbidden_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push_unique = |p: PathBuf| {
        if let Ok(res) = resolve_path_safely(&p) {
            if !roots.iter().any(|existing| path_starts_with_ci(&res, existing) && path_starts_with_ci(existing, &res)) {
                roots.push(res);
            }
        } else if let Ok(lex) = normalize_path_lexical(&p) {
            roots.push(lex);
        }
    };

    #[cfg(target_os = "windows")]
    {
        push_unique(PathBuf::from(r"C:\Windows"));
        push_unique(PathBuf::from(r"C:\Program Files"));
        push_unique(PathBuf::from(r"C:\Program Files (x86)"));
        push_unique(PathBuf::from(r"C:\ProgramData"));
        for env_key in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
            if let Some(val) = std::env::var_os(env_key).filter(|v| !v.is_empty()) {
                let p = PathBuf::from(val);
                if p.is_absolute() {
                    push_unique(p);
                }
            }
        }
    }

    if let Some(home) = user_home_dir() {
        push_unique(home.join(".ssh"));
        push_unique(home.join(".gnupg"));
        push_unique(home.join(".aws"));
        push_unique(home.join(".config"));
    }

    roots
}

pub fn forbidden_paths_display(teza_data_root: &Path) -> Vec<String> {
    let clean_root = file_dialog::normalize_os_display_path(teza_data_root);
    vec![
        r"C:\Windows".to_string(),
        r"C:\Program Files".to_string(),
        r"C:\Program Files (x86)".to_string(),
        r"C:\ProgramData".to_string(),
        "~/.ssh".to_string(),
        "~/.gnupg".to_string(),
        "~/.aws".to_string(),
        "~/.config".to_string(),
        format!("{} (ngoại trừ ai_workspace)", clean_root.display()),
    ]
}

fn is_path_forbidden(resolved: &Path, workspace_dir: &Path, teza_data_root: &Path) -> bool {
    let ws_norm = resolve_path_safely(workspace_dir)
        .unwrap_or_else(|_| file_dialog::normalize_os_display_path(workspace_dir));
    let data_norm = resolve_path_safely(teza_data_root)
        .unwrap_or_else(|_| file_dialog::normalize_os_display_path(teza_data_root));

    // Teza's own private data root is strictly forbidden EXCEPT `<data_dir>/ai_workspace`.
    if path_starts_with_ci(resolved, &data_norm) && !path_starts_with_ci(resolved, &ws_norm) {
        return true;
    }

    for forbidden in collect_hard_forbidden_roots() {
        if path_starts_with_ci(resolved, &forbidden) {
            return true;
        }
    }
    false
}

fn effective_allowed_roots(cfg: &AiConfig, workspace_dir: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push_root = |p: PathBuf| {
        let resolved = resolve_path_safely(&p).unwrap_or(p);
        if !roots
            .iter()
            .any(|e| path_starts_with_ci(&resolved, e) && path_starts_with_ci(e, &resolved))
        {
            roots.push(resolved);
        }
    };

    // Always include `<data_dir>/ai_workspace` so exported chat attachments stay accessible.
    push_root(workspace_dir.to_path_buf());

    let configured = sanitize_zeroclaw_allowed_roots(&cfg.zeroclaw_allowed_roots);
    if configured.is_empty() {
        if let Some(home) = user_home_dir() {
            for sub in ["Desktop", "Documents", "Downloads"] {
                push_root(home.join(sub));
            }
        }
    } else {
        for raw in configured {
            let p = PathBuf::from(raw);
            if p.is_absolute() {
                push_root(p);
            }
        }
    }
    roots
}

fn validate_raw_path_chars(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return Err("Đường dẫn không được để trống".into());
    }
    if trimmed.contains('\0') || trimmed.chars().any(char::is_control) {
        return Err("Đường dẫn chứa ký tự điều khiển không hợp lệ".into());
    }
    #[cfg(target_os = "windows")]
    {
        // Reject NTFS Alternate Data Streams (e.g. `foo.txt:hidden`) while allowing `C:\...`.
        let after_drive = if trimmed.len() >= 2
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':'
        {
            &trimmed[2..]
        } else {
            trimmed
        };
        if after_drive.contains(':') {
            return Err("Đường dẫn NTFS stream không được phép".into());
        }
    }
    Ok(trimmed)
}

fn validate_win_path(
    raw: &str,
    system_level: SystemAccessLevel,
    cfg: &AiConfig,
    workspace_dir: &Path,
    teza_data_root: &Path,
    must_exist: bool,
) -> Result<PathBuf, String> {
    let clean = validate_raw_path_chars(raw)?;
    let input = PathBuf::from(clean);
    let full = if input.is_absolute() {
        input
    } else {
        workspace_dir.join(input)
    };
    let resolved = resolve_path_safely(&full)?;

    if is_path_forbidden(&resolved, workspace_dir, teza_data_root) {
        return Err(format!(
            "Đường dẫn bị chặn bởi chính sách bảo vệ hệ thống/Teza: {}",
            resolved.display()
        ));
    }

    if system_level == SystemAccessLevel::Supervised {
        let allowed = effective_allowed_roots(cfg, workspace_dir);
        if !allowed.iter().any(|root| path_starts_with_ci(&resolved, root)) {
            return Err(format!(
                "Đường dẫn nằm ngoài danh sách thư mục được phép (allowed_roots): {}",
                resolved.display()
            ));
        }
    }

    if must_exist && !resolved.exists() {
        return Err(format!("Không tìm thấy đường dẫn: {}", resolved.display()));
    }
    Ok(resolved)
}

fn validate_attachment_source_path(
    raw: &str,
    workspace_dir: &Path,
    teza_data_root: &Path,
) -> Result<(PathBuf, u64), String> {
    let clean = validate_raw_path_chars(raw)?;
    let input = PathBuf::from(clean);
    let full = if input.is_absolute() {
        input
    } else {
        workspace_dir.join(input)
    };
    let resolved = resolve_path_safely(&full)?;
    if is_path_forbidden(&resolved, workspace_dir, teza_data_root) {
        return Err(format!(
            "Không được phép gửi tệp từ thư mục được bảo vệ: {}",
            resolved.display()
        ));
    }
    let meta = std::fs::metadata(&resolved)
        .map_err(|_| format!("Không tìm thấy tệp đính kèm: {}", resolved.display()))?;
    if !meta.is_file() {
        return Err("Đường dẫn tệp đính kèm phải là một tệp hợp lệ".into());
    }
    let size = meta.len();
    if size == 0 {
        return Err("Tệp đính kèm rỗng (0 byte)".into());
    }
    if size > MAX_ATTACHMENT_BYTES {
        return Err("Tệp đính kèm vượt quá giới hạn 50 MiB".into());
    }
    messaging::validate_attachment_size(size).map_err(|e| e.to_string())?;
    Ok((resolved, size))
}

fn is_blocked_open_extension(path: &Path) -> bool {
    if file_dialog::is_dangerous_executable_ext(path) {
        return true;
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str(),
        "exe"
            | "bat"
            | "cmd"
            | "ps1"
            | "vbs"
            | "js"
            | "msi"
            | "scr"
            | "com"
            | "pif"
            | "hta"
            | "lnk"
    )
}

fn is_destructive_powershell_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let padded = format!(" {lower} ");
    const SUBSTR_PATTERNS: [&str; 12] = [
        "remove-item",
        "format-volume",
        "clear-disk",
        "stop-process",
        "reg delete",
        "reg.exe delete",
        "taskkill",
        "shutdown",
        "restart-computer",
        "bcdedit",
        "cipher /w",
        "remove-itemproperty",
    ];
    if SUBSTR_PATTERNS.iter().any(|pat| lower.contains(pat)) {
        return true;
    }
    // Token-bounded short aliases (`rm`, `del`, `rd`, `rmdir`, `ri`, `erase`).
    for token in padded.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_') {
        if matches!(token, "rm" | "del" | "rd" | "rmdir" | "erase" | "ri") {
            return true;
        }
    }
    false
}

// ============================================================================
// Pure-Rust Minimal HTTP/1.1 Parser, Security Validators & Response Writer
// ============================================================================

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    body: String,
}

impl HttpResponse {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            reason: status_reason(status),
            body: value.to_string(),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            reason: status_reason(status),
            body: String::new(),
        }
    }
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
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

fn is_valid_mcp_host(host: &str, bound_port: u16) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    normalized == format!("127.0.0.1:{bound_port}")
        || normalized == format!("localhost:{bound_port}")
}

fn has_forbidden_browser_headers(origin: Option<&str>, sec_fetch_site: Option<&str>) -> bool {
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

fn extract_bearer_token(auth_header: Option<&str>) -> Option<&str> {
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

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

async fn read_chunked_body(
    stream: &mut TcpStream,
    mut buf: Vec<u8>,
    deadline: Instant,
) -> Result<Vec<u8>, HttpResponse> {
    let mut decoded = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        // Ensure we have the chunk-size line ending with `\r\n`.
        let line_end = loop {
            if let Some(pos) = find_crlf(&buf) {
                break pos;
            }
            if buf.len() > 1024 {
                return Err(HttpResponse::json(
                    400,
                    &json!({ "error": "invalid_chunk_header" }),
                ));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(HttpResponse::json(408, &json!({ "error": "request_timeout" })));
            };
            match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
                Ok(Ok(0)) => {
                    return Err(HttpResponse::json(
                        400,
                        &json!({ "error": "incomplete_chunked_body" }),
                    ));
                }
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
                Ok(Err(_)) => {
                    return Err(HttpResponse::json(400, &json!({ "error": "read_error" })));
                }
                Err(_) => {
                    return Err(HttpResponse::json(408, &json!({ "error": "request_timeout" })));
                }
            }
        };

        let size_line = std::str::from_utf8(&buf[..line_end])
            .map_err(|_| HttpResponse::json(400, &json!({ "error": "invalid_chunk_size" })))?;
        let hex_part = size_line.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(hex_part, 16)
            .map_err(|_| HttpResponse::json(400, &json!({ "error": "invalid_chunk_size" })))?;
        buf.drain(..line_end + 2);

        if chunk_size == 0 {
            return Ok(decoded);
        }

        if decoded.len().saturating_add(chunk_size) > MAX_BODY_BYTES {
            return Err(HttpResponse::json(
                413,
                &json!({ "error": "payload_too_large", "message": "Request body exceeds 2 MiB limit" }),
            ));
        }

        let needed = chunk_size + 2; // data + trailing \r\n
        while buf.len() < needed {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(HttpResponse::json(408, &json!({ "error": "request_timeout" })));
            };
            let to_read = (needed - buf.len()).min(chunk.len());
            match tokio::time::timeout(remaining, stream.read(&mut chunk[..to_read])).await {
                Ok(Ok(0)) => {
                    return Err(HttpResponse::json(
                        400,
                        &json!({ "error": "incomplete_chunked_body" }),
                    ));
                }
                Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
                Ok(Err(_)) => {
                    return Err(HttpResponse::json(400, &json!({ "error": "read_error" })));
                }
                Err(_) => {
                    return Err(HttpResponse::json(408, &json!({ "error": "request_timeout" })));
                }
            }
        }

        if &buf[chunk_size..chunk_size + 2] != b"\r\n" {
            return Err(HttpResponse::json(
                400,
                &json!({ "error": "malformed_chunk_terminator" }),
            ));
        }
        decoded.extend_from_slice(&buf[..chunk_size]);
        buf.drain(..needed);
    }
}

async fn read_http_request(stream: &mut TcpStream) -> ReadRequestResult {
    let deadline = Instant::now() + REQUEST_READ_TIMEOUT;
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return ReadRequestResult::Error(HttpResponse::json(
                431,
                &json!({ "error": "headers_too_large" }),
            ));
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return ReadRequestResult::Error(HttpResponse::json(
                408,
                &json!({ "error": "request_timeout" }),
            ));
        };
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                if buf.is_empty() {
                    return ReadRequestResult::EmptyClose;
                }
                return ReadRequestResult::Error(HttpResponse::json(
                    400,
                    &json!({ "error": "incomplete_request" }),
                ));
            }
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_HEADER_BYTES + MAX_BODY_BYTES + 4 {
                    return ReadRequestResult::Error(HttpResponse::json(
                        413,
                        &json!({ "error": "payload_too_large" }),
                    ));
                }
            }
            Ok(Err(_)) => return ReadRequestResult::EmptyClose,
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::json(
                    408,
                    &json!({ "error": "request_timeout" }),
                ));
            }
        }
    };

    if header_end + 4 > MAX_HEADER_BYTES {
        return ReadRequestResult::Error(HttpResponse::json(
            431,
            &json!({ "error": "headers_too_large" }),
        ));
    }

    let head_str = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(_) => {
            return ReadRequestResult::Error(HttpResponse::json(
                400,
                &json!({ "error": "invalid_encoding" }),
            ));
        }
    };

    let mut lines = head_str.split("\r\n");
    let Some(request_line) = lines.next() else {
        return ReadRequestResult::Error(HttpResponse::json(400, &json!({ "error": "bad_request" })));
    };

    let mut rl_parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        rl_parts.next(),
        rl_parts.next(),
        rl_parts.next(),
        rl_parts.next(),
    ) else {
        return ReadRequestResult::Error(HttpResponse::json(
            400,
            &json!({ "error": "bad_request_line" }),
        ));
    };

    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return ReadRequestResult::Error(HttpResponse::json(
            505,
            &json!({ "error": "http_version_not_supported" }),
        ));
    }

    if method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
        || !target.starts_with('/')
        || target.bytes().any(|b| b < 0x21 || b == 0x7f)
    {
        return ReadRequestResult::Error(HttpResponse::json(
            400,
            &json!({ "error": "invalid_target" }),
        ));
    }

    let method = method.to_string();
    let path = target.split_once('?').map(|(p, _)| p).unwrap_or(target).to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    let mut header_count = 0usize;
    for line in lines {
        header_count += 1;
        if header_count > MAX_HEADER_COUNT {
            return ReadRequestResult::Error(HttpResponse::json(
                431,
                &json!({ "error": "too_many_headers" }),
            ));
        }
        let Some((raw_name, raw_val)) = line.split_once(':') else {
            return ReadRequestResult::Error(HttpResponse::json(
                400,
                &json!({ "error": "malformed_header" }),
            ));
        };
        if raw_name.is_empty()
            || !raw_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return ReadRequestResult::Error(HttpResponse::json(
                400,
                &json!({ "error": "invalid_header_name" }),
            ));
        }
        let name = raw_name.to_ascii_lowercase();
        let val = raw_val.trim_matches(|c| c == ' ' || c == '\t');
        if val.bytes().any(|b| b < 0x20 && b != b'\t' || b == 0x7f) {
            return ReadRequestResult::Error(HttpResponse::json(
                400,
                &json!({ "error": "invalid_header_value" }),
            ));
        }
        if SINGLE_VALUE_SECURITY_HEADERS.contains(&name.as_str()) && headers.contains_key(&name) {
            return ReadRequestResult::Error(HttpResponse::json(
                400,
                &json!({ "error": "duplicate_header" }),
            ));
        }
        headers.insert(name, val.to_string());
    }

    let has_te = headers.contains_key("transfer-encoding");
    let has_cl = headers.contains_key("content-length");
    if has_te && has_cl {
        return ReadRequestResult::Error(HttpResponse::json(
            400,
            &json!({ "error": "conflicting_body_length_headers" }),
        ));
    }

    let leftover = buf.split_off(header_end + 4);

    if let Some(te) = headers.get("transfer-encoding") {
        if !te.trim().eq_ignore_ascii_case("chunked") {
            return ReadRequestResult::Error(HttpResponse::json(
                501,
                &json!({ "error": "unsupported_transfer_encoding" }),
            ));
        }
        if let Some(expect_val) = headers.get("expect") {
            if expect_val.trim().eq_ignore_ascii_case("100-continue") && leftover.is_empty() {
                let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await;
                let _ = stream.flush().await;
            }
        }
        return match read_chunked_body(stream, leftover, deadline).await {
            Ok(body) => ReadRequestResult::Request(HttpRequest {
                method,
                path,
                headers,
                body,
            }),
            Err(resp) => ReadRequestResult::Error(resp),
        };
    }

    let content_length = match headers.get("content-length") {
        Some(raw_len) => match raw_len.parse::<usize>() {
            Ok(n) if n <= MAX_BODY_BYTES => n,
            Ok(_) => {
                return ReadRequestResult::Error(HttpResponse::json(
                    413,
                    &json!({ "error": "payload_too_large", "message": "Request body exceeds 2 MiB limit" }),
                ));
            }
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::json(
                    400,
                    &json!({ "error": "invalid_content_length" }),
                ));
            }
        },
        None => 0,
    };

    let mut body = leftover;
    if body.len() > content_length {
        body.truncate(content_length);
    }

    if let Some(expect_val) = headers.get("expect") {
        if expect_val.trim().eq_ignore_ascii_case("100-continue") && body.len() < content_length {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return ReadRequestResult::Error(HttpResponse::json(
                    408,
                    &json!({ "error": "request_timeout" }),
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
                    return ReadRequestResult::Error(HttpResponse::json(
                        408,
                        &json!({ "error": "request_timeout" }),
                    ));
                }
            }
        }
    }

    while body.len() < content_length {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return ReadRequestResult::Error(HttpResponse::json(
                408,
                &json!({ "error": "request_timeout" }),
            ));
        };
        let to_read = (content_length - body.len()).min(chunk.len());
        match tokio::time::timeout(remaining, stream.read(&mut chunk[..to_read])).await {
            Ok(Ok(0)) => {
                return ReadRequestResult::Error(HttpResponse::json(
                    400,
                    &json!({ "error": "incomplete_body" }),
                ));
            }
            Ok(Ok(n)) => body.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => return ReadRequestResult::EmptyClose,
            Err(_) => {
                return ReadRequestResult::Error(HttpResponse::json(
                    408,
                    &json!({ "error": "request_timeout" }),
                ));
            }
        }
    }

    ReadRequestResult::Request(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

async fn write_http_response(stream: &mut TcpStream, resp: &HttpResponse) -> std::io::Result<()> {
    let head = if resp.body.is_empty() {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\r\n",
            resp.status, resp.reason
        )
    } else {
        format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\r\n",
            resp.status,
            resp.reason,
            resp.body.len()
        )
    };

    let write_fut = async {
        stream.write_all(head.as_bytes()).await?;
        if !resp.body.is_empty() {
            stream.write_all(resp.body.as_bytes()).await?;
        }
        stream.flush().await?;
        let _ = stream.shutdown().await;
        Ok::<(), std::io::Error>(())
    };
    match tokio::time::timeout(RESPONSE_WRITE_TIMEOUT, write_fut).await {
        Ok(res) => res,
        Err(_) => Ok(()),
    }
}

// ============================================================================
// HTTP + MCP JSON-RPC 2.0 Connection Router
// ============================================================================

async fn handle_mcp_connection(mut stream: TcpStream, ctx: Arc<McpServerContext>) {
    let req = match read_http_request(&mut stream).await {
        ReadRequestResult::Request(r) => r,
        ReadRequestResult::Error(resp) => {
            let _ = write_http_response(&mut stream, &resp).await;
            return;
        }
        ReadRequestResult::EmptyClose => return,
    };

    let resp = route_mcp_http_request(req, &ctx).await;
    let _ = write_http_response(&mut stream, &resp).await;
}

async fn route_mcp_http_request(req: HttpRequest, ctx: &McpServerContext) -> HttpResponse {
    // 1. DNS rebinding protection: strict loopback Host with exact bound port.
    let host_ok = req
        .headers
        .get("host")
        .is_some_and(|h| is_valid_mcp_host(h, ctx.bound_port));
    if !host_ok {
        return HttpResponse::json(
            403,
            &json!({ "error": "forbidden_host", "message": "Invalid Host header" }),
        );
    }

    // 2. Browser CSRF protection: reject Origin and non-none Sec-Fetch-Site.
    if has_forbidden_browser_headers(
        req.headers.get("origin").map(String::as_str),
        req.headers.get("sec-fetch-site").map(String::as_str),
    ) {
        return HttpResponse::json(
            403,
            &json!({ "error": "forbidden_browser_request", "message": "Browser requests are blocked" }),
        );
    }

    // 3. Constant-time Bearer token check.
    let provided = extract_bearer_token(req.headers.get("authorization").map(String::as_str));
    let auth_ok = provided
        .is_some_and(|tok| constant_time_eq(tok.as_bytes(), ctx.bearer_token.as_bytes()));
    if !auth_ok {
        return HttpResponse::json(
            401,
            &json!({ "error": "unauthorized", "message": "Missing or invalid Bearer token" }),
        );
    }

    // 4. Path & Method checks: POST /mcp only.
    if req.path != "/mcp" {
        return HttpResponse::json(
            404,
            &json!({ "error": "not_found", "message": "Only POST /mcp is served" }),
        );
    }
    if req.method != "POST" {
        return HttpResponse::json(
            405,
            &json!({ "error": "method_not_allowed", "message": "Use POST /mcp" }),
        );
    }

    let payload: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            return HttpResponse::json(
                200,
                &jsonrpc_error(Value::Null, -32700, format!("Parse error: {e}")),
            );
        }
    };

    handle_jsonrpc_message(payload, ctx).await
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        }
    })
}

fn mcp_tool_result(data: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    if is_error {
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": true,
        })
    } else {
        json!({
            "content": [{ "type": "text", "text": text }],
        })
    }
}

async fn handle_jsonrpc_message(msg: Value, ctx: &McpServerContext) -> HttpResponse {
    let Some(obj) = msg.as_object() else {
        return HttpResponse::json(
            200,
            &jsonrpc_error(Value::Null, -32600, "Invalid JSON-RPC request object"),
        );
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return HttpResponse::json(
            200,
            &jsonrpc_error(
                obj.get("id").cloned().unwrap_or(Value::Null),
                -32600,
                "Missing JSON-RPC method",
            ),
        );
    };
    let id_opt = obj.get("id").filter(|v| !v.is_null()).cloned();
    let params = obj
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    // JSON-RPC notifications (no `id`) -> HTTP 202 Accepted.
    if id_opt.is_none() || method == "notifications/initialized" || method.starts_with("notifications/") {
        return HttpResponse::empty(202);
    }
    let id = id_opt.unwrap_or(Value::Null);

    match method {
        "initialize" => HttpResponse::json(
            200,
            &jsonrpc_result(
                id,
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": MCP_SERVER_NAME,
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),
        ),
        "ping" => HttpResponse::json(200, &jsonrpc_result(id, json!({}))),
        "tools/list" => {
            let cfg = ctx.deps.ai_config.lock().await.clone();
            let names = exposed_tool_names_for(cfg.teza_access_level, cfg.system_access_level);
            let tools: Vec<Value> = names.iter().map(|n| build_tool_schema(n)).collect();
            HttpResponse::json(200, &jsonrpc_result(id, json!({ "tools": tools })))
        }
        "tools/call" => {
            let raw_name = params
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let tool_name = raw_name.strip_prefix("teza__").unwrap_or(raw_name);
            if tool_name.is_empty() {
                return HttpResponse::json(
                    200,
                    &jsonrpc_error(id, -32602, "Missing tool name in tools/call"),
                );
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            let result = execute_mcp_tool_call(tool_name, args, ctx).await;
            HttpResponse::json(200, &jsonrpc_result(id, result))
        }
        _ => HttpResponse::json(
            200,
            &jsonrpc_error(id, -32601, format!("Method not found: {method}")),
        ),
    }
}

// ============================================================================
// Tool Dispatch, Policy Enforcement & Audit Recording
// ============================================================================

async fn execute_mcp_tool_call(tool_name: &str, args: Value, ctx: &McpServerContext) -> Value {
    let cfg = ctx.deps.ai_config.lock().await.clone();
    let session_id = ctx.current_session_id();
    let category = tool_category(tool_name);

    if !is_tool_exposed(tool_name, cfg.teza_access_level, cfg.system_access_level) {
        let msg = format!(
            "Công cụ '{tool_name}' chưa được cấp quyền (Teza={}, System={}).",
            cfg.teza_access_level.as_str(),
            cfg.system_access_level.as_str()
        );
        ctx.record_audit(
            &session_id,
            tool_name,
            category,
            "blocked",
            false,
            format!("Chặn công cụ chưa mở quyền: {tool_name}"),
            Some(msg.clone()),
        );
        return mcp_tool_result(&json!({ "ok": false, "error": msg }), true);
    }

    let exec_res: Result<(ApprovalOutcome, String, Value), (String, String, String)> = match tool_name
    {
        // ---- Teza Read & Draft (T1+) ----
        "get_account_info" => wrap_auto(tool_get_account_info(ctx).await, "Xem thông tin tài khoản"),
        "list_threads" => wrap_auto(tool_list_threads(&args, ctx).await, "Liệt kê hội thoại"),
        "get_thread_details" => {
            wrap_auto(tool_get_thread_details(&args, ctx).await, "Xem chi tiết hội thoại")
        }
        "get_messages" => wrap_auto(tool_get_messages(&args, ctx).await, "Đọc lịch sử tin nhắn"),
        "search_messages" => {
            wrap_auto(tool_search_messages(&args, ctx).await, "Tìm kiếm tin nhắn")
        }
        "list_friends" => wrap_auto(tool_list_friends(&args, ctx).await, "Xem danh sách bạn bè"),
        "list_folders" => wrap_auto(tool_list_folders(ctx).await, "Xem thư mục hội thoại"),
        "list_schedules" => {
            wrap_auto(tool_list_schedules(&args, ctx).await, "Xem danh sách tin hẹn giờ")
        }
        "export_attachment" => {
            wrap_auto(tool_export_attachment(&args, ctx).await, "Xuất tệp đính kèm")
        }
        "draft_message" => wrap_auto(tool_draft_message(&args, ctx).await, "Soạn bản nháp tin nhắn"),

        // ---- Teza Control (T2+) ----
        "send_message" => tool_send_message(&args, &cfg, &session_id, ctx).await,
        "send_attachment" => tool_send_attachment(&args, &cfg, &session_id, ctx).await,
        "react_message" => tool_react_message(&args, &cfg, &session_id, ctx).await,
        "recall_or_delete_message" => {
            tool_recall_or_delete_message(&args, &cfg, &session_id, ctx).await
        }
        "manage_thread_state" => tool_manage_thread_state(&args, &cfg, ctx).await,
        "pin_chat_message" => tool_pin_chat_message(&args, &cfg, &session_id, ctx).await,
        "create_schedule" => tool_create_schedule(&args, &cfg, &session_id, ctx).await,
        "manage_schedule" => tool_manage_schedule(&args, &cfg, &session_id, ctx).await,
        "create_poll" => tool_create_poll(&args, &cfg, &session_id, ctx).await,
        "create_reminder" => tool_create_reminder(&args, &cfg, &session_id, ctx).await,
        "manage_social" => tool_manage_social(&args, &cfg, &session_id, ctx).await,

        // ---- Windows & Office System Tools (S1+) ----
        "win_list_dir" => wrap_auto(tool_win_list_dir(&args, &cfg, ctx).await, "Liệt kê thư mục"),
        "win_read_file" => wrap_auto(tool_win_read_file(&args, &cfg, ctx).await, "Đọc tệp Windows"),
        "win_write_file" => tool_win_write_file(&args, &cfg, &session_id, ctx).await,
        "win_run_powershell" => tool_win_run_powershell(&args, &cfg, &session_id, ctx).await,
        "win_open_path" => tool_win_open_path(&args, &cfg, &session_id, ctx).await,

        _ => Err((
            "blocked".to_string(),
            format!("Công cụ không tồn tại: {tool_name}"),
            format!("Unknown tool: {tool_name}"),
        )),
    };

    match exec_res {
        Ok((decision, summary, value)) => {
            ctx.record_audit(
                &session_id,
                tool_name,
                category,
                decision.audit_str(),
                true,
                summary,
                None,
            );
            mcp_tool_result(&value, false)
        }
        Err((decision, summary, err_msg)) => {
            ctx.record_audit(
                &session_id,
                tool_name,
                category,
                &decision,
                false,
                summary,
                Some(err_msg.clone()),
            );
            mcp_tool_result(&json!({ "ok": false, "error": err_msg }), true)
        }
    }
}

fn wrap_auto(
    res: Result<(String, Value), String>,
    fallback_summary: &str,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    match res {
        Ok((summary, val)) => Ok((ApprovalOutcome::Auto, summary, val)),
        Err(err) => {
            let decision = if err.contains("bị chặn") || err.contains("allowed_roots") {
                "blocked"
            } else {
                "auto"
            };
            Err((decision.to_string(), fallback_summary.to_string(), err))
        }
    }
}

fn enforce_allowed_thread_scope(cfg: &AiConfig, thread_id: &str) -> Result<(), (String, String, String)> {
    if cfg.teza_access_level != TezaAccessLevel::Autonomous
        || cfg.zeroclaw_allowed_thread_ids.is_empty()
    {
        return Ok(());
    }
    let clean_tid = thread_id.trim();
    let clean_gid = clean_group_id(clean_tid);
    let allowed = cfg.zeroclaw_allowed_thread_ids.iter().any(|item| {
        let trimmed = item.trim();
        trimmed == clean_tid || (!clean_gid.is_empty() && clean_group_id(trimmed) == clean_gid)
    });
    if !allowed {
        let err = format!(
            "Hội thoại '{clean_tid}' không nằm trong danh sách cho phép tự động (zeroclaw_allowed_thread_ids)."
        );
        return Err(("blocked".to_string(), format!("Chặn ghi ngoài phạm vi: {clean_tid}"), err));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn maybe_require_approval(
    ctx: &McpServerContext,
    need_approval: bool,
    session_id: &str,
    tool_name: &str,
    grant_key: &str,
    category: &str,
    risk: &str,
    title: String,
    summary: String,
    details: Value,
) -> Result<ApprovalOutcome, (String, String, String)> {
    if !need_approval {
        return Ok(ApprovalOutcome::Auto);
    }
    let outcome = ctx
        .approvals
        .request_approval(
            &ctx.sink,
            session_id,
            tool_name,
            grant_key,
            category,
            risk,
            title,
            summary.clone(),
            details,
        )
        .await;
    if outcome.is_allowed() {
        let live_cfg = ctx.deps.ai_config.lock().await.clone();
        let canonical_tool = if tool_name == "kick_group_member" || tool_name == "leave_group" {
            "manage_social"
        } else {
            tool_name
        };
        if !is_tool_exposed(
            canonical_tool,
            live_cfg.teza_access_level,
            live_cfg.system_access_level,
        ) {
            return Err((
                "blocked".to_string(),
                summary,
                format!(
                    "Quyền thực thi công cụ '{tool_name}' đã bị thu hồi trước khi duyệt hoàn tất."
                ),
            ));
        }
        Ok(outcome)
    } else {
        let reason = match outcome {
            ApprovalOutcome::Deny => "Người dùng đã từ chối hành động này.".to_string(),
            ApprovalOutcome::Timeout => {
                "Hết thời gian chờ duyệt (180s), hành động đã bị hủy.".to_string()
            }
            ApprovalOutcome::Cancelled => "Hành động đã bị hủy bởi E-Stop hoặc đăng xuất.".to_string(),
            _ => "Hành động không được phép.".to_string(),
        };
        let audit_decision = match outcome {
            ApprovalOutcome::Timeout => "timeout",
            _ => "deny",
        };
        Err((audit_decision.to_string(), summary, reason))
    }
}

async fn lookup_thread_display_and_kind(
    store_arc: &Arc<Mutex<MessageStore>>,
    thread_id: &str,
) -> (String, ThreadKind) {
    let store = store_arc.lock().await;
    let title = store
        .list_threads(FolderFilter::All)
        .ok()
        .and_then(|list| list.into_iter().find(|t| t.id == thread_id))
        .map(|t| t.title)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| thread_id.to_string());
    let kind = store
        .get_thread_kind(thread_id)
        .ok()
        .flatten()
        .unwrap_or(ThreadKind::Direct);
    (title, kind)
}

async fn require_authenticated_session(
    ctx: &McpServerContext,
) -> Result<Session, String> {
    let sess = ctx.deps.session.lock().await;
    if !sess.is_authenticated() {
        return Err("Chưa đăng nhập tài khoản Teza (cần quét mã QR trước).".into());
    }
    Ok(sess.clone_shallow())
}

async fn handle_session_error(ctx: &McpServerContext, err: anyhow::Error, prefix: &str) -> String {
    if let Some(SessionError::Expired { code }) = err.downcast_ref::<SessionError>() {
        let reason = format!("Phiên đăng nhập đã hết hạn (mã {code}). Vui lòng quét lại mã QR.");
        ctx.sink.clear_expired_session(reason.clone()).await;
        return reason;
    }
    format!("{prefix}: {err}")
}

async fn persist_sent_message(
    ctx: &McpServerContext,
    msg: MessageDto,
) -> Result<MessageDto, String> {
    let created = {
        let mut store = ctx.deps.store.lock().await;
        store.upsert_message(&msg).map_err(|e| e.to_string())?;
        let preview = format_thread_preview(&msg.text, true, "", false);
        store
            .update_thread_preview_returning_created(&msg.thread_id, &preview, msg.ts, false, true)
            .unwrap_or(false)
    };
    ctx.sink.emit_json("core://message-new", &msg);
    if created {
        ctx.sink.emit_json("core://threads-updated", &Value::Null);
    }
    ctx.sink.notify_gdrive_sync();
    Ok(msg)
}

// ============================================================================
// Group A: Teza Read & Draft Tools (1–10)
// ============================================================================

async fn tool_get_account_info(ctx: &McpServerContext) -> Result<(String, Value), String> {
    let (authenticated, uid, display_name, avatar) = {
        let sess = ctx.deps.session.lock().await;
        (
            sess.is_authenticated(),
            sess.profile.uid.clone(),
            sess.profile.display_name.clone(),
            sess.profile.avatar.clone(),
        )
    };
    let active_thread_id = ctx.deps.active_thread_id.lock().await.clone();
    let (total_threads, total_unread) = {
        let store = ctx.deps.store.lock().await;
        let list = store.list_threads(FolderFilter::All).unwrap_or_default();
        let unread: u64 = list.iter().map(|t| u64::from(t.unread_count)).sum();
        (list.len(), unread)
    };
    let val = json!({
        "authenticated": authenticated,
        "uid": uid,
        "display_name": display_name,
        "avatar": avatar,
        "active_thread_id": active_thread_id,
        "total_threads": total_threads,
        "total_unread": total_unread,
    });
    Ok((
        format!("Tài khoản {} ({total_threads} hội thoại, {total_unread} chưa đọc)", if display_name.is_empty() { "chưa đăng nhập" } else { &display_name }),
        val,
    ))
}

async fn tool_list_threads(args: &Value, ctx: &McpServerContext) -> Result<(String, Value), String> {
    let kind_str = opt_str(args, "kind").unwrap_or("all").to_ascii_lowercase();
    let unread_only = opt_bool(args, "unread_only").unwrap_or(false);
    let query = opt_str(args, "query").unwrap_or("").trim().to_string();
    let limit = opt_usize(args, "limit").unwrap_or(50).clamp(1, 200);

    let folder = match kind_str.as_str() {
        "direct" => FolderFilter::Direct,
        "group" => FolderFilter::Group,
        "cloud" => FolderFilter::Cloud,
        _ => FolderFilter::All,
    };

    let mut threads = {
        let store = ctx.deps.store.lock().await;
        store.list_threads(folder).map_err(|e| e.to_string())?
    };

    if unread_only {
        threads.retain(|t| t.unread_count > 0);
    }
    if !query.is_empty() {
        let q_norm = strip_vi_accents(&query);
        threads.retain(|t| {
            strip_vi_accents(&t.title).contains(&q_norm)
                || strip_vi_accents(&t.last_preview).contains(&q_norm)
                || t.id.eq_ignore_ascii_case(&query)
        });
    }
    let total_matched = threads.len();
    threads.truncate(limit);

    let items: Vec<Value> = threads.iter().map(thread_summary_json).collect();
    Ok((
        format!("Liệt kê {}/{total_matched} hội thoại", items.len()),
        json!({
            "total_matched": total_matched,
            "threads": items,
        }),
    ))
}

fn thread_summary_json(t: &ThreadDto) -> Value {
    json!({
        "thread_id": t.id,
        "title": t.title,
        "kind": t.kind,
        "last_preview": t.last_preview,
        "last_ts": t.last_ts,
        "unread_count": t.unread_count,
        "is_pinned": t.is_pinned,
        "is_muted": t.is_muted,
        "is_archived": t.is_archived,
    })
}

async fn tool_get_thread_details(
    args: &Value,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let thread_id = req_str(args, "thread_id")?;
    let (thread_opt, kind, pinned_msgs, media_items, file_items, link_items, known_profiles) = {
        let store = ctx.deps.store.lock().await;
        let thread_opt = store
            .list_threads(FolderFilter::All)
            .ok()
            .and_then(|list| list.into_iter().find(|t| t.id == thread_id));
        let kind = thread_opt
            .as_ref()
            .map(|t| t.kind)
            .or_else(|| store.get_thread_kind(&thread_id).ok().flatten())
            .unwrap_or(ThreadKind::Direct);
        let pinned = store
            .list_pinned_messages(&thread_id, 30)
            .unwrap_or_default();
        let (media, files) = store
            .list_thread_media(&thread_id, 30)
            .unwrap_or_default();
        let links = store
            .list_thread_links(&thread_id, 30)
            .unwrap_or_default();
        let known = if kind == ThreadKind::Group {
            store.resolve_known_profiles().unwrap_or_default()
        } else {
            HashMap::new()
        };
        (thread_opt, kind, pinned, media, files, links, known)
    };

    let mut total_members = None;
    let mut members = Vec::new();
    let mut creator_id = String::new();
    let mut admin_ids = Vec::new();
    if kind == ThreadKind::Group {
        let sess_snapshot = {
            let sess = ctx.deps.session.lock().await;
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

    let pinned_json: Vec<Value> = pinned_msgs.iter().map(message_compact_json).collect();
    let files_json: Vec<Value> = file_items.iter().map(message_compact_json).collect();
    let media_json: Vec<Value> = media_items.iter().map(message_compact_json).collect();
    let links_json: Vec<Value> = link_items.iter().map(message_compact_json).collect();

    Ok((
        format!("Xem chi tiết hội thoại {thread_id}"),
        json!({
            "thread_id": thread_id,
            "thread": thread_opt.as_ref().map(thread_summary_json),
            "kind": kind,
            "total_members": total_members,
            "creator_id": creator_id,
            "admin_ids": admin_ids,
            "members": members,
            "pinned_messages": pinned_json,
            "file_items": files_json,
            "media_items": media_json,
            "link_items": links_json,
        }),
    ))
}

fn message_compact_json(m: &MessageDto) -> Value {
    let has_attachment = m
        .attachment_url
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || m.attachment_name
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty() && !s.starts_with("teza-poll:") && !s.starts_with("teza-reminder:"));
    json!({
        "msg_id": m.msg_id,
        "thread_id": m.thread_id,
        "sender_id": m.sender_id,
        "sender_name": m.sender_name,
        "text": m.text,
        "ts": m.ts,
        "is_outgoing": m.is_outgoing,
        "is_recalled": m.is_recalled,
        "is_pinned": m.is_pinned,
        "msg_type": m.msg_type,
        "quote": m.quote,
        "reactions": m.reactions,
        "attachment_name": m.attachment_name,
        "has_attachment": has_attachment,
    })
}

async fn tool_get_messages(args: &Value, ctx: &McpServerContext) -> Result<(String, Value), String> {
    let thread_id = req_str(args, "thread_id")?;
    let limit = opt_usize(args, "limit").unwrap_or(40).clamp(1, 150);
    let before_ts = opt_i64(args, "before_ts").map(normalize_ts_ms);

    let msgs = {
        let store = ctx.deps.store.lock().await;
        store
            .load_messages(&thread_id, before_ts, limit)
            .map_err(|e| e.to_string())?
    };
    let items: Vec<Value> = msgs.iter().map(message_compact_json).collect();
    Ok((
        format!("Đọc {} tin nhắn từ {thread_id}", items.len()),
        json!({
            "thread_id": thread_id,
            "count": items.len(),
            "messages": items,
        }),
    ))
}

async fn tool_search_messages(
    args: &Value,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let query = req_str(args, "query")?;
    let thread_filter = opt_str(args, "thread_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let limit = opt_usize(args, "limit").unwrap_or(30).clamp(1, 100);

    let (messages, matching_threads) = {
        let store = ctx.deps.store.lock().await;
        let fetch_limit = if thread_filter.is_some() { 200 } else { limit };
        let mut hits = store
            .search_messages(&query, fetch_limit)
            .map_err(|e| e.to_string())?;
        if let Some(ref tid) = thread_filter {
            hits.retain(|m| m.thread_id == *tid);
            hits.truncate(limit);
        }
        let q_norm = strip_vi_accents(&query);
        let threads: Vec<ThreadDto> = store
            .list_threads(FolderFilter::All)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| {
                thread_filter
                    .as_deref()
                    .is_none_or(|tid| t.id == tid)
                    && strip_vi_accents(&t.title).contains(&q_norm)
            })
            .take(20)
            .collect();
        (hits, threads)
    };

    let msg_json: Vec<Value> = messages.iter().map(message_compact_json).collect();
    let thread_json: Vec<Value> = matching_threads.iter().map(thread_summary_json).collect();
    Ok((
        format!(
            "Tìm '{}': {} tin nhắn, {} hội thoại",
            truncate_chars(&query, 40),
            msg_json.len(),
            thread_json.len()
        ),
        json!({
            "query": query,
            "messages": msg_json,
            "matching_threads": thread_json,
        }),
    ))
}

async fn tool_list_friends(args: &Value, ctx: &McpServerContext) -> Result<(String, Value), String> {
    let include_requests = opt_bool(args, "include_requests").unwrap_or(false);
    let friends: Vec<Value> = {
        let store = ctx.deps.store.lock().await;
        store
            .list_threads(FolderFilter::Direct)
            .map_err(|e| e.to_string())?
            .iter()
            .map(thread_summary_json)
            .collect()
    };

    let mut requests_val = Value::Null;
    let mut requests_note: Option<String> = None;
    if include_requests {
        let sess_snapshot = {
            let sess = ctx.deps.session.lock().await;
            sess.clone_shallow()
        };
        if sess_snapshot.is_authenticated() {
            match threads::list_friend_requests(&sess_snapshot).await {
                Ok(reqs) => {
                    requests_val = serde_json::to_value(reqs).unwrap_or(Value::Null);
                }
                Err(e) => {
                    requests_note = Some(handle_session_error(ctx, e, "Không tải được lời mời kết bạn").await);
                }
            }
        } else {
            requests_note = Some("Chưa đăng nhập để tải lời mời kết bạn trực tuyến".into());
        }
    }

    Ok((
        format!("Danh sách {} bạn bè", friends.len()),
        json!({
            "count": friends.len(),
            "friends": friends,
            "friend_requests": requests_val,
            "requests_note": requests_note,
        }),
    ))
}

async fn tool_list_folders(ctx: &McpServerContext) -> Result<(String, Value), String> {
    let folders = {
        let store = ctx.deps.store.lock().await;
        store.list_chat_folders().map_err(|e| e.to_string())?
    };
    Ok((
        format!("Danh sách {} thư mục chat", folders.len()),
        json!({
            "count": folders.len(),
            "folders": folders,
        }),
    ))
}

async fn tool_list_schedules(
    args: &Value,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let status = opt_str(args, "status")
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"));
    let limit = opt_usize(args, "limit").unwrap_or(50).clamp(1, 200);
    let items = {
        let store = ctx.deps.store.lock().await;
        store
            .list_scheduled_messages(status, limit)
            .map_err(|e| e.to_string())?
    };
    Ok((
        format!("Danh sách {} tin hẹn giờ", items.len()),
        json!({
            "count": items.len(),
            "schedules": items,
        }),
    ))
}

fn pick_unique_workspace_path(workspace_dir: &Path, filename: &str) -> PathBuf {
    let candidate = workspace_dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let stem = candidate
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment");
    let ext = candidate.extension().and_then(|s| s.to_str());
    for idx in 1..=9999 {
        let next_name = match ext {
            Some(e) if !e.is_empty() => format!("{stem}_{idx}.{e}"),
            _ => format!("{stem}_{idx}"),
        };
        let next = workspace_dir.join(next_name);
        if !next.exists() {
            return next;
        }
    }
    workspace_dir.join(format!("{stem}_{}", now_ms()))
}

async fn tool_export_attachment(
    args: &Value,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let msg_id = req_str(args, "msg_id")?;
    let thread_id_opt = opt_str(args, "thread_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let msg = {
        let store = ctx.deps.store.lock().await;
        if let Some(ref tid) = thread_id_opt {
            store
                .get_message_by_ids(tid, &msg_id, None)
                .map_err(|e| e.to_string())?
        } else {
            let mut found = None;
            if let Ok(threads) = store.list_threads(FolderFilter::All) {
                for t in threads {
                    if let Ok(Some(m)) = store.get_message_by_ids(&t.id, &msg_id, None) {
                        found = Some(m);
                        break;
                    }
                }
            }
            found
        }
    }
    .ok_or_else(|| format!("Không tìm thấy tin nhắn {msg_id}"))?;

    std::fs::create_dir_all(&ctx.workspace_dir)
        .map_err(|e| format!("Không tạo được thư mục ai_workspace: {e}"))?;

    let raw_name = msg
        .attachment_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("attachment");
    let url_str = msg.attachment_url.as_deref().unwrap_or("").trim();
    let ext_hint = file_dialog::extract_extension_from_name(raw_name)
        .or_else(|| file_dialog::extract_extension_from_url(url_str));

    // 1. Try MediaVault cached object linked by `(thread_id, msg_id)` or `attachment_url`.
    let vault = ctx.deps.media_vault.read().await.clone();
    let mut cached_obj = {
        let store = ctx.deps.store.lock().await;
        store
            .lookup_media_by_msg(&msg.thread_id, &msg.msg_id, "primary")
            .ok()
            .flatten()
            .filter(|o| o.local_state == MEDIA_LOCAL_STATE_LOCAL)
    };
    if cached_obj.is_none() && !url_str.is_empty() {
        cached_obj = vault
            .lookup_local_by_url(&ctx.deps.store, url_str)
            .await
            .ok()
            .flatten();
    }

    if let Some(obj) = cached_obj {
        let final_ext = ext_hint
            .or_else(|| file_dialog::extension_from_mime(&obj.mime).map(str::to_string));
        let safe_name =
            file_dialog::sanitize_suggested_filename_preserving_ext(raw_name, final_ext.as_deref());
        let dst = pick_unique_workspace_path(&ctx.workspace_dir, &safe_name);
        let size_bytes = vault
            .export_to_path(&obj, &dst)
            .await
            .map_err(|e| format!("Không xuất được tệp từ MediaVault: {e}"))?;
        let display_path = file_dialog::normalize_os_display_path(&dst)
            .to_string_lossy()
            .into_owned();
        let filename = dst
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&safe_name)
            .to_string();
        return Ok((
            format!("Xuất tệp {filename} ({size_bytes} B)"),
            json!({
                "ok": true,
                "path": display_path,
                "filename": filename,
                "size_bytes": size_bytes,
                "thread_id": msg.thread_id,
                "msg_id": msg.msg_id,
            }),
        ));
    }

    // 2. Try previously saved local path recorded in store.
    let saved_path_opt = {
        let store = ctx.deps.store.lock().await;
        store
            .get_message_saved_path(&msg.thread_id, &msg.msg_id)
            .ok()
            .flatten()
    };
    if let Some(saved_str) = saved_path_opt {
        let src = PathBuf::from(saved_str.trim());
        if src.is_file() {
            if let Ok(resolved) = resolve_path_safely(&src) {
                if !is_path_forbidden(&resolved, &ctx.workspace_dir, &ctx.paths.root) {
                    let src_ext = resolved
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase());
                    let safe_name = file_dialog::sanitize_suggested_filename_preserving_ext(
                        raw_name,
                        ext_hint.as_deref().or(src_ext.as_deref()),
                    );
                    let dst = pick_unique_workspace_path(&ctx.workspace_dir, &safe_name);
                    let size_bytes = std::fs::copy(&resolved, &dst)
                        .map_err(|e| format!("Không sao chép được tệp vào ai_workspace: {e}"))?;
                    let display_path = file_dialog::normalize_os_display_path(&dst)
                        .to_string_lossy()
                        .into_owned();
                    let filename = dst
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&safe_name)
                        .to_string();
                    return Ok((
                        format!("Xuất tệp {filename} ({size_bytes} B)"),
                        json!({
                            "ok": true,
                            "path": display_path,
                            "filename": filename,
                            "size_bytes": size_bytes,
                            "thread_id": msg.thread_id,
                            "msg_id": msg.msg_id,
                        }),
                    ));
                }
            }
        }
    }

    // 3. Try downloading remote attachment URL into MediaVault if authenticated.
    if url_str.starts_with("http://") || url_str.starts_with("https://") {
        let sess_snapshot = {
            let sess = ctx.deps.session.lock().await;
            sess.clone_shallow()
        };
        if sess_snapshot.is_authenticated() {
            let kind_hint = if msg.msg_type.contains("photo") || msg.msg_type.contains("image") {
                "image"
            } else if msg.msg_type.contains("video") {
                "video"
            } else if msg.msg_type.contains("voice") || msg.msg_type.contains("audio") {
                "audio"
            } else {
                "file"
            };
            let obj = vault
                .ensure_cached_url(
                    &sess_snapshot,
                    &ctx.deps.store,
                    url_str,
                    kind_hint,
                    Some((msg.thread_id.as_str(), msg.msg_id.as_str())),
                    |_, _| {},
                )
                .await
                .map_err(|e| format!("Không tải được tệp đính kèm từ máy chủ: {e}"))?;
            let final_ext = ext_hint
                .or_else(|| file_dialog::extension_from_mime(&obj.mime).map(str::to_string));
            let safe_name = file_dialog::sanitize_suggested_filename_preserving_ext(
                raw_name,
                final_ext.as_deref(),
            );
            let dst = pick_unique_workspace_path(&ctx.workspace_dir, &safe_name);
            let size_bytes = vault
                .export_to_path(&obj, &dst)
                .await
                .map_err(|e| format!("Không xuất được tệp vào ai_workspace: {e}"))?;
            let display_path = file_dialog::normalize_os_display_path(&dst)
                .to_string_lossy()
                .into_owned();
            let filename = dst
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&safe_name)
                .to_string();
            return Ok((
                format!("Xuất tệp {filename} ({size_bytes} B)"),
                json!({
                    "ok": true,
                    "path": display_path,
                    "filename": filename,
                    "size_bytes": size_bytes,
                    "thread_id": msg.thread_id,
                    "msg_id": msg.msg_id,
                }),
            ));
        }
    }

    Err(format!(
        "Tin nhắn {msg_id} không có tệp đính kèm cục bộ hoặc không thể tải xuống."
    ))
}

async fn tool_draft_message(
    args: &Value,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let thread_id = req_str(args, "thread_id")?;
    let text = req_str(args, "text")?;
    let open_thread = opt_bool(args, "open_thread").unwrap_or(true);

    ctx.sink.emit_json(
        "core://ai-compose-draft",
        &json!({
            "thread_id": thread_id,
            "text": text,
            "open_thread": open_thread,
        }),
    );

    Ok((
        format!("Điền bản nháp vào {thread_id}"),
        json!({
            "ok": true,
            "thread_id": thread_id,
            "open_thread": open_thread,
            "text_len": text.chars().count(),
        }),
    ))
}

// ============================================================================
// Group B: Teza Control Tools (11–21)
// ============================================================================

async fn tool_send_message(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Gửi tin nhắn".into(), e))?;
    let text = req_str(args, "text")
        .map_err(|e| ("auto".into(), "Gửi tin nhắn".into(), e))?;
    let reply_to_msg_id = opt_str(args, "reply_to_msg_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), format!("Gửi tin tới {thread_id}"), e))?;
    let (title, kind) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;

    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let summary = format!("Gửi tin tới {title}: {}", truncate_chars(&text, 100));
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "send_message",
        "send_message",
        "teza",
        "medium",
        format!("Gửi tin nhắn tới {title}"),
        summary.clone(),
        json!({
            "recipient": title,
            "recipient_name": title,
            "thread_title": title,
            "thread_id": thread_id,
            "text": text,
            "reply_to_msg_id": reply_to_msg_id,
        }),
    )
    .await?;

    let msg = if let Some(ref qid) = reply_to_msg_id {
        let quoted = {
            let store = ctx.deps.store.lock().await;
            store
                .get_message_by_ids(&thread_id, qid, None)
                .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
                .ok_or_else(|| {
                    (
                        decision.audit_str().into(),
                        summary.clone(),
                        format!("Không tìm thấy tin nhắn gốc '{qid}' để trả lời"),
                    )
                })?
        };
        match messaging::send_quote(&sess_snapshot, &thread_id, kind, &text, &quoted).await {
            Ok(m) => m,
            Err(e) => {
                let err = handle_session_error(ctx, e, "Gửi tin trả lời thất bại").await;
                return Err((decision.audit_str().into(), summary, err));
            }
        }
    } else {
        match messaging::send_text(&sess_snapshot, &thread_id, kind, &text).await {
            Ok(m) => m,
            Err(e) => {
                let err = handle_session_error(ctx, e, "Gửi tin nhắn thất bại").await;
                return Err((decision.audit_str().into(), summary, err));
            }
        }
    };

    let saved = persist_sent_message(ctx, msg)
        .await
        .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;
    Ok((
        decision,
        summary,
        json!({
            "ok": true,
            "msg_id": saved.msg_id,
            "thread_id": saved.thread_id,
            "ts": saved.ts,
        }),
    ))
}

async fn tool_send_attachment(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Gửi tệp đính kèm".into(), e))?;
    let file_path_raw = req_str(args, "file_path")
        .map_err(|e| ("auto".into(), "Gửi tệp đính kèm".into(), e))?;
    let caption = opt_str(args, "caption").unwrap_or("").trim().to_string();

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let (resolved, size_bytes) =
        validate_attachment_source_path(&file_path_raw, &ctx.workspace_dir, &ctx.paths.root)
            .map_err(|e| ("blocked".into(), format!("Chặn gửi tệp: {file_path_raw}"), e))?;
    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), format!("Gửi tệp tới {thread_id}"), e))?;
    let (title, kind) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;

    let file_name = resolved
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment")
        .to_string();
    let display_path = file_dialog::normalize_os_display_path(&resolved)
        .to_string_lossy()
        .into_owned();
    let summary = format!("Gửi tệp '{file_name}' tới {title}");

    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "send_attachment",
        "send_attachment",
        "teza",
        "medium",
        format!("Gửi tệp đính kèm tới {title}"),
        summary.clone(),
        json!({
            "recipient": title,
            "recipient_name": title,
            "thread_title": title,
            "thread_id": thread_id,
            "attachment": display_path,
            "file_path": display_path,
            "file_name": file_name,
            "size_bytes": size_bytes,
            "text": caption,
        }),
    )
    .await?;

    let msg = match messaging::send_attachment_file(
        &sess_snapshot,
        &thread_id,
        kind,
        &resolved,
        &caption,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            let err = handle_session_error(ctx, e, "Gửi tệp đính kèm thất bại").await;
            return Err((decision.audit_str().into(), summary, err));
        }
    };

    let saved = persist_sent_message(ctx, msg)
        .await
        .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;

    // Best-effort ingest into MediaVault matching `message_send_attachment_path`.
    let vault = ctx.deps.media_vault.read().await.clone();
    let media_kind = if messaging::is_image_extension(&file_name) {
        "image"
    } else if saved.msg_type.contains("video") {
        "video"
    } else {
        "file"
    };
    let mime = messaging::mime_from_extension(&file_name);
    let _ = vault
        .ingest_local_file(
            &ctx.deps.store,
            &resolved,
            media_kind,
            mime,
            saved.attachment_url.as_deref(),
            Some((saved.thread_id.as_str(), saved.msg_id.as_str())),
            saved.attachment_name.as_deref().or(Some(file_name.as_str())),
        )
        .await;
    ctx.sink.notify_gdrive_sync();

    Ok((
        decision,
        summary,
        json!({
            "ok": true,
            "msg_id": saved.msg_id,
            "thread_id": saved.thread_id,
            "file_name": file_name,
            "size_bytes": size_bytes,
            "ts": saved.ts,
        }),
    ))
}

async fn tool_react_message(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Thả cảm xúc".into(), e))?;
    let msg_id = req_str(args, "msg_id")
        .map_err(|e| ("auto".into(), "Thả cảm xúc".into(), e))?;
    let emoji_raw = req_str(args, "emoji")
        .map_err(|e| ("auto".into(), "Thả cảm xúc".into(), e))?;
    let canonical = canonicalize_reaction_emoji(&emoji_raw)
        .unwrap_or_else(|| emoji_raw.trim().to_string());
    if canonical.is_empty() {
        return Err(("auto".into(), "Thả cảm xúc".into(), "Biểu tượng cảm xúc không hợp lệ".into()));
    }

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), "Thả cảm xúc".into(), e))?;
    let (title, kind, msg) = {
        let store = ctx.deps.store.lock().await;
        let msg = store
            .get_message_by_ids(&thread_id, &msg_id, None)
            .map_err(|e| ("auto".into(), "Thả cảm xúc".into(), e.to_string()))?
            .ok_or_else(|| ("auto".into(), "Thả cảm xúc".into(), "Không tìm thấy tin nhắn".into()))?;
        let kind = store
            .get_thread_kind(&msg.thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let title = store
            .list_threads(FolderFilter::All)
            .ok()
            .and_then(|list| list.into_iter().find(|t| t.id == msg.thread_id))
            .map(|t| t.title)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| msg.thread_id.clone());
        (title, kind, msg)
    };

    let remove = msg.reactions.iter().any(|r| {
        r.me && (r.emoji == canonical
            || canonicalize_reaction_emoji(&r.emoji).as_deref() == Some(canonical.as_str()))
    });

    let summary = format!("Thả cảm xúc {canonical} trong {title}");
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "react_message",
        "react_message",
        "teza",
        "medium",
        format!("Thả cảm xúc {canonical}"),
        summary.clone(),
        json!({
            "recipient": title,
            "thread_id": msg.thread_id,
            "msg_id": msg.msg_id,
            "emoji": canonical,
            "remove": remove,
        }),
    )
    .await?;

    if let Err(e) =
        messaging::send_reaction(&sess_snapshot, &msg.thread_id, kind, &msg, &canonical, remove)
            .await
    {
        let err = handle_session_error(ctx, e, "Thả cảm xúc thất bại").await;
        return Err((decision.audit_str().into(), summary, err));
    }

    let updated = {
        let mut store = ctx.deps.store.lock().await;
        store
            .apply_reaction_event(
                &msg.thread_id,
                &msg.msg_id,
                msg.cli_msg_id.as_deref(),
                None,
                Some(&canonical),
                true,
                remove,
            )
            .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
            .ok_or_else(|| {
                (
                    decision.audit_str().into(),
                    summary.clone(),
                    "Không cập nhật được cảm xúc".into(),
                )
            })?
    };
    ctx.sink.emit_json("core://message-updated", &updated);
    Ok((
        decision,
        summary,
        json!({
            "ok": true,
            "msg_id": updated.msg_id,
            "reactions": updated.reactions,
        }),
    ))
}

async fn tool_recall_or_delete_message(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Thu hồi/xóa tin nhắn".into(), e))?;
    let msg_id = req_str(args, "msg_id")
        .map_err(|e| ("auto".into(), "Thu hồi/xóa tin nhắn".into(), e))?;
    let action = req_str(args, "action")
        .map_err(|e| ("auto".into(), "Thu hồi/xóa tin nhắn".into(), e))?
        .to_ascii_lowercase();
    if action != "recall" && action != "delete_local" {
        return Err((
            "auto".into(),
            "Thu hồi/xóa tin nhắn".into(),
            "action phải là 'recall' hoặc 'delete_local'".into(),
        ));
    }

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let (title, kind, msg) = {
        let store = ctx.deps.store.lock().await;
        let msg = store
            .get_message_by_ids(&thread_id, &msg_id, None)
            .map_err(|e| ("auto".into(), "Thu hồi/xóa tin nhắn".into(), e.to_string()))?
            .ok_or_else(|| {
                ("auto".into(), "Thu hồi/xóa tin nhắn".into(), "Không tìm thấy tin nhắn".into())
            })?;
        let kind = store
            .get_thread_kind(&msg.thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct);
        let title = store
            .list_threads(FolderFilter::All)
            .ok()
            .and_then(|list| list.into_iter().find(|t| t.id == msg.thread_id))
            .map(|t| t.title)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| msg.thread_id.clone());
        (title, kind, msg)
    };

    let sess_snapshot = if action == "recall" {
        Some(
            require_authenticated_session(ctx)
                .await
                .map_err(|e| ("auto".into(), "Thu hồi tin nhắn".into(), e))?,
        )
    } else {
        None
    };

    let summary = format!(
        "{} tin nhắn {} trong {}",
        if action == "recall" { "Thu hồi" } else { "Xóa cục bộ" },
        msg.msg_id,
        title
    );
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "recall_or_delete_message",
        "recall_or_delete_message",
        "teza",
        "high",
        if action == "recall" {
            format!("Thu hồi tin nhắn trong {title}")
        } else {
            format!("Xóa cục bộ tin nhắn trong {title}")
        },
        summary.clone(),
        json!({
            "action": action,
            "recipient": title,
            "thread_id": msg.thread_id,
            "msg_id": msg.msg_id,
            "text": truncate_chars(&msg.text, 200),
        }),
    )
    .await?;

    if action == "recall" {
        let sess = sess_snapshot.expect("checked above");
        if let Err(e) = messaging::recall_message(&sess, &msg.thread_id, kind, &msg).await {
            let err = handle_session_error(ctx, e, "Thu hồi tin nhắn thất bại").await;
            return Err((decision.audit_str().into(), summary, err));
        }
        let (updated, orphans) = {
            let mut store = ctx.deps.store.lock().await;
            let orphans = store
                .cleanup_orphaned_media_for_msg(&msg.thread_id, &msg.msg_id)
                .unwrap_or_default();
            let updated = store
                .mark_recalled(&msg.thread_id, &msg.msg_id)
                .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
                .ok_or_else(|| {
                    (
                        decision.audit_str().into(),
                        summary.clone(),
                        "Không cập nhật được trạng thái thu hồi".into(),
                    )
                })?;
            (updated, orphans)
        };
        if !orphans.is_empty() {
            let vault = ctx.deps.media_vault.read().await.clone();
            vault.unlink_rel_paths(&orphans).await;
        }
        ctx.sink.emit_json("core://message-updated", &updated);
        ctx.sink.notify_gdrive_sync();
        Ok((
            decision,
            summary,
            json!({ "ok": true, "action": "recall", "msg_id": updated.msg_id }),
        ))
    } else {
        let orphans = {
            let mut store = ctx.deps.store.lock().await;
            let orphans = store
                .cleanup_orphaned_media_for_msg(&msg.thread_id, &msg.msg_id)
                .unwrap_or_default();
            store
                .delete_message(&msg.thread_id, &msg.msg_id)
                .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?;
            orphans
        };
        if !orphans.is_empty() {
            let vault = ctx.deps.media_vault.read().await.clone();
            vault.unlink_rel_paths(&orphans).await;
        }
        ctx.sink.emit_json(
            "core://message-deleted",
            &json!({ "thread_id": msg.thread_id, "msg_id": msg.msg_id }),
        );
        ctx.sink.notify_gdrive_sync();
        Ok((
            decision,
            summary,
            json!({ "ok": true, "action": "delete_local", "msg_id": msg.msg_id }),
        ))
    }
}

async fn tool_manage_thread_state(
    args: &Value,
    cfg: &AiConfig,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Quản lý trạng thái hội thoại".into(), e))?;
    let action = req_str(args, "action")
        .map_err(|e| ("auto".into(), "Quản lý trạng thái hội thoại".into(), e))?
        .to_ascii_lowercase();
    let mute_duration_secs = opt_i64(args, "mute_duration_secs");

    if action != "open_thread" {
        enforce_allowed_thread_scope(cfg, &thread_id)?;
    }

    if action == "open_thread" {
        ctx.sink
            .emit_json("core://ai-open-thread", &json!({ "thread_id": thread_id }));
        return Ok((
            ApprovalOutcome::Auto,
            format!("Mở hội thoại {thread_id}"),
            json!({ "ok": true, "thread_id": thread_id, "action": "open_thread" }),
        ));
    }

    let kind = {
        let store = ctx.deps.store.lock().await;
        match action.as_str() {
            "mark_read" => store.mark_thread_read(&thread_id),
            "mute" => {
                let until = mute_duration_secs
                    .filter(|d| *d > 0)
                    .map(|d| now_ms().saturating_add(d.saturating_mul(1000)))
                    .unwrap_or(0);
                store.set_thread_muted_until(&thread_id, true, until)
            }
            "unmute" => store.set_thread_muted_until(&thread_id, false, 0),
            "pin" => store.set_thread_pinned(&thread_id, true),
            "unpin" => store.set_thread_pinned(&thread_id, false),
            "archive" => store.set_thread_archived(&thread_id, true),
            "unarchive" => store.set_thread_archived(&thread_id, false),
            _ => {
                return Err((
                    "auto".into(),
                    "Quản lý trạng thái hội thoại".into(),
                    format!("Hành động không hợp lệ: {action}"),
                ));
            }
        }
        .map_err(|e| ("auto".into(), format!("Trạng thái {action}"), e.to_string()))?;
        store
            .get_thread_kind(&thread_id)
            .ok()
            .flatten()
            .unwrap_or(ThreadKind::Direct)
    };

    // Best-effort remote sync when authenticated (mirrors `lib.rs` thread commands).
    if thread_id != "cloud-me" {
        let sess_snapshot = {
            let sess = ctx.deps.session.lock().await;
            sess.clone_shallow()
        };
        if sess_snapshot.is_authenticated() {
            match action.as_str() {
                "mark_read" => {
                    let _ = threads::sync_unread_mark(&sess_snapshot, &thread_id, kind, false).await;
                }
                "mute" => {
                    let _ = threads::sync_mute_thread(
                        &sess_snapshot,
                        &thread_id,
                        kind,
                        true,
                        mute_duration_secs,
                    )
                    .await;
                }
                "unmute" => {
                    let _ = threads::sync_mute_thread(&sess_snapshot, &thread_id, kind, false, None)
                        .await;
                }
                "pin" => {
                    let _ = threads::sync_pin_thread(&sess_snapshot, &thread_id, kind, true).await;
                }
                "unpin" => {
                    let _ = threads::sync_pin_thread(&sess_snapshot, &thread_id, kind, false).await;
                }
                _ => {}
            }
        }
    }

    ctx.sink.emit_json("core://threads-updated", &Value::Null);
    ctx.sink.emit_json("core://threads-refresh", &Value::Null);

    Ok((
        ApprovalOutcome::Auto,
        format!("Cập nhật hội thoại {thread_id}: {action}"),
        json!({ "ok": true, "thread_id": thread_id, "action": action }),
    ))
}

async fn tool_pin_chat_message(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Ghim tin nhắn".into(), e))?;
    let msg_id = req_str(args, "msg_id")
        .map_err(|e| ("auto".into(), "Ghim tin nhắn".into(), e))?;
    let unpin = opt_bool(args, "unpin").unwrap_or(false);
    let pinned = !unpin;

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let (title, _) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
    let summary = format!(
        "{} tin nhắn {msg_id} trong {title}",
        if pinned { "Ghim" } else { "Bỏ ghim" }
    );
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "pin_chat_message",
        "pin_chat_message",
        "teza",
        "medium",
        if pinned {
            format!("Ghim tin nhắn trong {title}")
        } else {
            format!("Bỏ ghim tin nhắn trong {title}")
        },
        summary.clone(),
        json!({
            "recipient": title,
            "thread_id": thread_id,
            "msg_id": msg_id,
            "pinned": pinned,
        }),
    )
    .await?;

    let updated = {
        let store = ctx.deps.store.lock().await;
        store
            .set_message_pinned(&thread_id, &msg_id, pinned)
            .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
            .ok_or_else(|| {
                (
                    decision.audit_str().into(),
                    summary.clone(),
                    "Không tìm thấy tin nhắn để ghim".into(),
                )
            })?
    };
    ctx.sink.emit_json("core://message-updated", &updated);
    ctx.sink.notify_gdrive_sync();
    Ok((
        decision,
        summary,
        json!({ "ok": true, "msg_id": updated.msg_id, "is_pinned": updated.is_pinned }),
    ))
}

async fn tool_create_schedule(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Tạo tin hẹn giờ".into(), e))?;
    let text = opt_str(args, "text").unwrap_or("").trim().to_string();
    let att_raw = opt_str(args, "attachment_path").unwrap_or("").trim().to_string();
    let send_at_raw = opt_i64(args, "send_at_ts")
        .ok_or_else(|| ("auto".into(), "Tạo tin hẹn giờ".into(), "Thiếu tham số send_at_ts".into()))?;
    let recurrence = opt_str(args, "recurrence").unwrap_or("once").trim().to_ascii_lowercase();
    let misfire_policy = opt_str(args, "misfire_policy")
        .unwrap_or("ask")
        .trim()
        .to_ascii_lowercase();

    if text.is_empty() && att_raw.is_empty() {
        return Err((
            "auto".into(),
            "Tạo tin hẹn giờ".into(),
            "Cần nhập text hoặc attachment_path".into(),
        ));
    }

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let send_at_ts = normalize_ts_ms(send_at_raw);
    let now = now_ms();
    if send_at_ts <= now {
        return Err((
            "auto".into(),
            "Tạo tin hẹn giờ".into(),
            "send_at_ts phải nằm trong tương lai (> thời điểm hiện tại)".into(),
        ));
    }
    if send_at_ts > now.saturating_add(MAX_SCHEDULE_FUTURE_MS) {
        return Err((
            "auto".into(),
            "Tạo tin hẹn giờ".into(),
            "send_at_ts vượt quá giới hạn tối đa 5 năm".into(),
        ));
    }

    let attachment_path = if !att_raw.is_empty() {
        let (resolved, _) =
            validate_attachment_source_path(&att_raw, &ctx.workspace_dir, &ctx.paths.root)
                .map_err(|e| ("blocked".into(), "Chặn tệp hẹn giờ".into(), e))?;
        file_dialog::normalize_os_display_path(&resolved)
            .to_string_lossy()
            .into_owned()
    } else {
        String::new()
    };

    let (title, kind) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
    let kind_str = match kind {
        ThreadKind::Direct => "direct",
        ThreadKind::Group => "group",
        ThreadKind::Cloud => "cloud",
    }
    .to_string();

    let summary = format!("Hẹn giờ gửi tới {title} lúc {send_at_ts}");
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "create_schedule",
        "create_schedule",
        "teza",
        "medium",
        format!("Tạo tin hẹn giờ tới {title}"),
        summary.clone(),
        json!({
            "recipient": title,
            "recipient_name": title,
            "thread_title": title,
            "thread_id": thread_id,
            "text": text,
            "attachment": attachment_path,
            "attachment_path": attachment_path,
            "send_at_ts": send_at_ts,
            "schedule_time": format!("{send_at_ts}"),
            "recurrence": recurrence,
            "misfire_policy": misfire_policy,
        }),
    )
    .await?;

    let input = NewScheduledMessageInput {
        thread_id: thread_id.clone(),
        thread_title: title,
        thread_kind: kind_str,
        text,
        attachment_path,
        send_at_ts,
        recurrence,
        misfire_policy,
        misfire_grace_secs: 600,
    };

    let created = {
        let store = ctx.deps.store.lock().await;
        store
            .create_scheduled_message(&input)
            .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
    };
    ctx.deps.scheduler.notify_changed();
    ctx.sink.emit_json("core://schedule-updated", &created);

    Ok((
        decision,
        summary,
        json!({ "ok": true, "schedule": created }),
    ))
}

async fn tool_manage_schedule(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let schedule_id = req_str(args, "schedule_id")
        .map_err(|e| ("auto".into(), "Quản lý lịch hẹn".into(), e))?;
    let action = req_str(args, "action")
        .map_err(|e| ("auto".into(), "Quản lý lịch hẹn".into(), e))?
        .to_ascii_lowercase();
    if !matches!(action.as_str(), "cancel" | "delete" | "send_now") {
        return Err((
            "auto".into(),
            "Quản lý lịch hẹn".into(),
            "action phải là 'cancel', 'delete', hoặc 'send_now'".into(),
        ));
    }

    let existing = {
        let store = ctx.deps.store.lock().await;
        store
            .get_scheduled_message(&schedule_id)
            .map_err(|e| ("auto".into(), "Quản lý lịch hẹn".into(), e.to_string()))?
            .ok_or_else(|| {
                ("auto".into(), "Quản lý lịch hẹn".into(), format!("Không tìm thấy lịch hẹn '{schedule_id}'"))
            })?
    };
    enforce_allowed_thread_scope(cfg, &existing.thread_id)?;

    let summary = format!("Lịch hẹn {schedule_id}: {action}");
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "manage_schedule",
        "manage_schedule",
        "teza",
        "medium",
        format!("Quản lý lịch hẹn ({action})"),
        summary.clone(),
        json!({
            "schedule_id": schedule_id,
            "action": action,
            "thread_id": existing.thread_id,
            "recipient": existing.thread_title,
            "text": truncate_chars(&existing.text, 200),
        }),
    )
    .await?;

    let result = match action.as_str() {
        "cancel" => {
            let (ok, dto) = {
                let store = ctx.deps.store.lock().await;
                let ok = store
                    .cancel_scheduled_message(&schedule_id)
                    .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?;
                let dto = if ok {
                    store.get_scheduled_message(&schedule_id).ok().flatten()
                } else {
                    None
                };
                (ok, dto)
            };
            if ok {
                ctx.deps.scheduler.notify_changed();
                if let Some(ref d) = dto {
                    ctx.sink.emit_json("core://schedule-updated", d);
                } else {
                    ctx.sink.emit_json("core://schedule-updated", &Value::Null);
                }
            }
            json!({ "ok": ok, "action": "cancel", "schedule": dto })
        }
        "delete" => {
            let ok = {
                let store = ctx.deps.store.lock().await;
                store
                    .delete_scheduled_message(&schedule_id)
                    .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
            };
            if ok {
                ctx.deps.scheduler.notify_changed();
                ctx.sink.emit_json("core://schedule-updated", &Value::Null);
            }
            json!({ "ok": ok, "action": "delete", "schedule_id": schedule_id })
        }
        "send_now" => {
            let dto = {
                let store = ctx.deps.store.lock().await;
                store
                    .reschedule_now(&schedule_id, now_ms())
                    .map_err(|e| (decision.audit_str().into(), summary.clone(), e.to_string()))?
            };
            if let Some(ref d) = dto {
                ctx.deps.scheduler.notify_changed();
                ctx.sink.emit_json("core://schedule-updated", d);
            }
            json!({ "ok": dto.is_some(), "action": "send_now", "schedule": dto })
        }
        _ => unreachable!(),
    };

    Ok((decision, summary, result))
}

async fn tool_create_poll(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Tạo bình chọn".into(), e))?;
    let question = req_str(args, "question")
        .map_err(|e| ("auto".into(), "Tạo bình chọn".into(), e))?;
    let raw_options = args
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| ("auto".into(), "Tạo bình chọn".into(), "Thiếu danh sách options".into()))?;

    let mut options: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for item in raw_options {
        if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            let key = s.to_lowercase();
            if seen.insert(key) {
                options.push(s.to_string());
            }
        }
    }
    if options.len() < 2 {
        return Err((
            "auto".into(),
            "Tạo bình chọn".into(),
            "Bình chọn cần ít nhất 2 lựa chọn khác nhau".into(),
        ));
    }

    let allow_multi = opt_bool(args, "allow_multi_choices")
        .or_else(|| opt_bool(args, "allow_multi"))
        .unwrap_or(false);
    let allow_add = opt_bool(args, "allow_add_new_option")
        .or_else(|| opt_bool(args, "allow_add_option"))
        .unwrap_or(false);
    let is_anonymous = opt_bool(args, "is_anonymous").unwrap_or(false);
    let expired_time_ms = opt_i64(args, "expired_time_ms").unwrap_or(0);

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let (title, kind) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
    if kind != ThreadKind::Group {
        return Err((
            "auto".into(),
            "Tạo bình chọn".into(),
            "Bình chọn chỉ hỗ trợ trong cuộc trò chuyện nhóm".into(),
        ));
    }
    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), "Tạo bình chọn".into(), e))?;

    let summary = format!("Tạo bình chọn trong {title}: {}", truncate_chars(&question, 80));
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "create_poll",
        "create_poll",
        "teza",
        "medium",
        format!("Tạo bình chọn trong {title}"),
        summary.clone(),
        json!({
            "recipient": title,
            "thread_id": thread_id,
            "question": question,
            "options": options,
            "allow_multi_choices": allow_multi,
            "allow_add_new_option": allow_add,
        }),
    )
    .await?;

    let msg = match threads::create_poll(
        &sess_snapshot,
        &thread_id,
        &question,
        &options,
        expired_time_ms,
        allow_multi,
        allow_add,
        is_anonymous,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            let err = handle_session_error(ctx, e, "Tạo bình chọn thất bại").await;
            return Err((decision.audit_str().into(), summary, err));
        }
    };

    let saved = persist_sent_message(ctx, msg)
        .await
        .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;
    Ok((
        decision,
        summary,
        json!({ "ok": true, "msg_id": saved.msg_id, "thread_id": saved.thread_id }),
    ))
}

async fn tool_create_reminder(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let thread_id = req_str(args, "thread_id")
        .map_err(|e| ("auto".into(), "Tạo nhắc hẹn".into(), e))?;
    let rem_title = req_str(args, "title")
        .map_err(|e| ("auto".into(), "Tạo nhắc hẹn".into(), e))?;
    let start_raw = opt_i64(args, "start_time_ms")
        .ok_or_else(|| ("auto".into(), "Tạo nhắc hẹn".into(), "Thiếu start_time_ms".into()))?;
    let repeat_mode = match args.get("repeat_mode") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) as i32,
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "daily" | "1" => 1,
            "weekly" | "2" => 2,
            "monthly" | "3" => 3,
            _ => 0,
        },
        _ => 0,
    };

    let start_time_ms = threads::validate_reminder_start_time(
        normalize_ts_ms(start_raw),
        now_ms(),
        threads::REMINDER_MIN_LEAD_MS,
    )
    .map_err(|e| ("auto".into(), "Tạo nhắc hẹn".into(), e.to_string()))?;

    enforce_allowed_thread_scope(cfg, &thread_id)?;

    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), "Tạo nhắc hẹn".into(), e))?;
    let (title, kind) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;

    let summary = format!("Tạo nhắc hẹn trong {title}: {}", truncate_chars(&rem_title, 80));
    let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "create_reminder",
        "create_reminder",
        "teza",
        "medium",
        format!("Tạo nhắc hẹn trong {title}"),
        summary.clone(),
        json!({
            "recipient": title,
            "thread_id": thread_id,
            "title": rem_title,
            "start_time_ms": start_time_ms,
            "repeat_mode": repeat_mode,
        }),
    )
    .await?;

    let msg = match threads::create_reminder(
        &sess_snapshot,
        &thread_id,
        kind,
        &rem_title,
        start_time_ms,
        repeat_mode,
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            let err = handle_session_error(ctx, e, "Tạo nhắc hẹn thất bại").await;
            return Err((decision.audit_str().into(), summary, err));
        }
    };

    let saved = persist_sent_message(ctx, msg)
        .await
        .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;
    Ok((
        decision,
        summary,
        json!({ "ok": true, "msg_id": saved.msg_id, "thread_id": saved.thread_id, "start_time_ms": start_time_ms }),
    ))
}

async fn tool_manage_social(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let action = req_str(args, "action")
        .map_err(|e| ("auto".into(), "Quản lý kết bạn/nhóm".into(), e))?
        .to_ascii_lowercase();

    let sess_snapshot = require_authenticated_session(ctx)
        .await
        .map_err(|e| ("auto".into(), format!("Social {action}"), e))?;

    match action.as_str() {
        "find_user_by_phone" => {
            let phone = req_str(args, "phone")
                .map_err(|e| ("auto".into(), "Tra SĐT".into(), e))?;
            let country = opt_str(args, "country_code")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("84");
            match threads::find_user_by_phone_with_country(&sess_snapshot, &phone, country).await {
                Ok(profile) => Ok((
                    ApprovalOutcome::Auto,
                    format!("Tra SĐT thành công: {}", profile.display_name),
                    json!({ "ok": true, "profile": profile }),
                )),
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Không tìm thấy người dùng").await;
                    Err(("auto".into(), "Tra SĐT".into(), err))
                }
            }
        }
        "send_friend_request" => {
            let uid = req_str(args, "user_id")
                .map_err(|e| ("auto".into(), "Gửi lời mời kết bạn".into(), e))?;
            let clean_uid = uid.split('_').next().unwrap_or(&uid).trim().to_string();
            let message = opt_str(args, "message")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| truncate_chars(s, 150));

            let summary = format!("Gửi lời mời kết bạn tới {clean_uid}");
            let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
            let decision = maybe_require_approval(
                ctx,
                need_approval,
                session_id,
                "manage_social",
                "manage_social:send_friend_request",
                "teza",
                "medium",
                "Gửi lời mời kết bạn".into(),
                summary.clone(),
                json!({ "action": action, "user_id": clean_uid, "message": message }),
            )
            .await?;

            match threads::send_friend_request(&sess_snapshot, &clean_uid, message.as_deref()).await
            {
                Ok(res) => {
                    if res.ok {
                        let kind = if res.auto_accepted {
                            "friend_added"
                        } else {
                            "friend_request_sent"
                        };
                        ctx.sink.emit_json(
                            "core://social-event",
                            &json!({ "kind": kind, "thread_id": Value::Null, "user_id": res.user_id, "self_removed": false }),
                        );
                    }
                    Ok((decision, summary, json!({ "ok": res.ok, "result": res })))
                }
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Gửi lời mời kết bạn thất bại").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        "accept_friend_request" => {
            let uid = req_str(args, "user_id")
                .map_err(|e| ("auto".into(), "Chấp nhận kết bạn".into(), e))?;
            let clean_uid = uid.split('_').next().unwrap_or(&uid).trim().to_string();
            let summary = format!("Chấp nhận lời mời kết bạn của {clean_uid}");
            let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
            let decision = maybe_require_approval(
                ctx,
                need_approval,
                session_id,
                "manage_social",
                "manage_social:accept_friend_request",
                "teza",
                "medium",
                "Chấp nhận lời mời kết bạn".into(),
                summary.clone(),
                json!({ "action": action, "user_id": clean_uid }),
            )
            .await?;

            match threads::accept_friend_request(&sess_snapshot, &clean_uid).await {
                Ok(res) => {
                    if res.ok {
                        ctx.sink.emit_json(
                            "core://social-event",
                            &json!({ "kind": "friend_added", "thread_id": Value::Null, "user_id": res.user_id, "self_removed": false }),
                        );
                    }
                    Ok((decision, summary, json!({ "ok": res.ok, "result": res })))
                }
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Chấp nhận kết bạn thất bại").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        "reject_friend_request" => {
            let uid = req_str(args, "user_id")
                .map_err(|e| ("auto".into(), "Từ chối kết bạn".into(), e))?;
            let clean_uid = uid.split('_').next().unwrap_or(&uid).trim().to_string();
            let summary = format!("Từ chối lời mời kết bạn của {clean_uid}");
            let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
            let decision = maybe_require_approval(
                ctx,
                need_approval,
                session_id,
                "manage_social",
                "manage_social:reject_friend_request",
                "teza",
                "medium",
                "Từ chối lời mời kết bạn".into(),
                summary.clone(),
                json!({ "action": action, "user_id": clean_uid }),
            )
            .await?;

            match threads::reject_friend_request(&sess_snapshot, &clean_uid).await {
                Ok(res) => {
                    if res.ok {
                        ctx.sink.emit_json(
                            "core://social-event",
                            &json!({ "kind": "friend_request_rejected", "thread_id": Value::Null, "user_id": res.user_id, "self_removed": false }),
                        );
                    }
                    Ok((decision, summary, json!({ "ok": res.ok, "result": res })))
                }
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Từ chối kết bạn thất bại").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        "add_group_members" => {
            let thread_id = req_str(args, "thread_id")
                .map_err(|e| ("auto".into(), "Thêm thành viên nhóm".into(), e))?;
            enforce_allowed_thread_scope(cfg, &thread_id)?;
            let member_ids = extract_member_ids(args)?;
            let (title, _) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
            let summary = format!("Thêm {} thành viên vào nhóm {title}", member_ids.len());
            let need_approval = cfg.teza_access_level == TezaAccessLevel::Supervised;
            let decision = maybe_require_approval(
                ctx,
                need_approval,
                session_id,
                "manage_social",
                "manage_social:add_group_members",
                "teza",
                "medium",
                format!("Thêm thành viên vào nhóm {title}"),
                summary.clone(),
                json!({
                    "action": action,
                    "recipient": title,
                    "thread_id": thread_id,
                    "user_ids": member_ids,
                }),
            )
            .await?;

            match threads::add_group_members(&sess_snapshot, &thread_id, &member_ids).await {
                Ok(res) => Ok((decision, summary, json!({ "ok": true, "result": res }))),
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Thêm thành viên nhóm thất bại").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        "kick_group_member" => {
            let thread_id = req_str(args, "thread_id")
                .map_err(|e| ("auto".into(), "Mời ra khỏi nhóm".into(), e))?;
            enforce_allowed_thread_scope(cfg, &thread_id)?;
            let member_ids = extract_member_ids(args)?;
            let clean_gid = clean_group_id(&thread_id).trim().to_string();
            if clean_gid.is_empty() {
                return Err(("auto".into(), "Mời ra khỏi nhóm".into(), "Mã nhóm không hợp lệ".into()));
            }
            let (title, _) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
            let summary = format!("Mời {} thành viên ra khỏi nhóm {title}", member_ids.len());

            // Destructive: ALWAYS require per-action high-risk approval even in T3!
            let decision = maybe_require_approval(
                ctx,
                true,
                session_id,
                "kick_group_member",
                "manage_social:kick_group_member",
                "teza",
                "high",
                format!("Mời thành viên ra khỏi nhóm {title}"),
                summary.clone(),
                json!({
                    "action": action,
                    "recipient": title,
                    "thread_id": thread_id,
                    "user_ids": member_ids,
                }),
            )
            .await?;

            match threads::kick_group_members(&sess_snapshot, &clean_gid, &member_ids).await {
                Ok(res) => {
                    if !res.removed.is_empty() {
                        ctx.sink.emit_json(
                            "core://social-event",
                            &json!({
                                "kind": "group_member_removed",
                                "thread_id": res.group_id,
                                "user_id": res.removed.first(),
                                "self_removed": false,
                            }),
                        );
                    }
                    Ok((decision, summary, json!({ "ok": true, "result": res })))
                }
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Không thể mời thành viên ra khỏi nhóm").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        "leave_group" => {
            let thread_id = req_str(args, "thread_id")
                .map_err(|e| ("auto".into(), "Rời nhóm".into(), e))?;
            enforce_allowed_thread_scope(cfg, &thread_id)?;
            let clean_gid = clean_group_id(&thread_id).trim().to_string();
            if clean_gid.is_empty() {
                return Err(("auto".into(), "Rời nhóm".into(), "Mã nhóm không hợp lệ".into()));
            }
            let silent = opt_bool(args, "silent").unwrap_or(false);
            let (title, _) = lookup_thread_display_and_kind(&ctx.deps.store, &thread_id).await;
            let summary = format!("Rời nhóm {title}");

            // Destructive: ALWAYS require per-action high-risk approval even in T3!
            let decision = maybe_require_approval(
                ctx,
                true,
                session_id,
                "leave_group",
                "manage_social:leave_group",
                "teza",
                "high",
                format!("Rời nhóm {title}"),
                summary.clone(),
                json!({
                    "action": action,
                    "recipient": title,
                    "thread_id": thread_id,
                    "silent": silent,
                }),
            )
            .await?;

            match threads::leave_group(&sess_snapshot, &clean_gid, silent).await {
                Ok(res) => {
                    if res.left {
                        let orphans = {
                            let mut store = ctx.deps.store.lock().await;
                            let mut list = store.delete_thread(&res.group_id).unwrap_or_default();
                            if thread_id != res.group_id {
                                if let Ok(extra) = store.delete_thread(&thread_id) {
                                    list.extend(extra);
                                }
                            }
                            list
                        };
                        if !orphans.is_empty() {
                            let vault = ctx.deps.media_vault.read().await.clone();
                            vault.unlink_rel_paths(&orphans).await;
                        }
                        ctx.sink.emit_json(
                            "core://social-event",
                            &json!({
                                "kind": "group_left",
                                "thread_id": res.group_id,
                                "user_id": Value::Null,
                                "self_removed": true,
                            }),
                        );
                        ctx.sink.emit_json("core://threads-updated", &Value::Null);
                        ctx.sink.notify_gdrive_sync();
                    }
                    Ok((decision, summary, json!({ "ok": res.left, "result": res })))
                }
                Err(e) => {
                    let err = handle_session_error(ctx, e, "Rời nhóm thất bại").await;
                    Err((decision.audit_str().into(), summary, err))
                }
            }
        }
        _ => Err((
            "auto".into(),
            "Quản lý kết bạn/nhóm".into(),
            format!("Hành động manage_social không hợp lệ: {action}"),
        )),
    }
}

fn extract_member_ids(args: &Value) -> Result<Vec<String>, (String, String, String)> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(arr) = args.get("user_ids").and_then(Value::as_array) {
        for v in arr {
            if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                let clean = s.split('_').next().unwrap_or(s).trim().to_string();
                if !clean.is_empty() && !ids.contains(&clean) {
                    ids.push(clean);
                }
            }
        }
    }
    if let Some(single) = opt_str(args, "user_id").map(str::trim).filter(|s| !s.is_empty()) {
        let clean = single.split('_').next().unwrap_or(single).trim().to_string();
        if !clean.is_empty() && !ids.contains(&clean) {
            ids.push(clean);
        }
    }
    if ids.is_empty() {
        return Err((
            "auto".into(),
            "Quản lý thành viên nhóm".into(),
            "Cần cung cấp user_id hoặc user_ids".into(),
        ));
    }
    Ok(ids)
}

// ============================================================================
// Group C: Windows & Office System Tools (22–26)
// ============================================================================

async fn tool_win_list_dir(
    args: &Value,
    cfg: &AiConfig,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let raw_path = req_str(args, "path")?;
    let max_entries = opt_usize(args, "max_entries").unwrap_or(100).clamp(1, 500);

    let resolved = validate_win_path(
        &raw_path,
        cfg.system_access_level,
        cfg,
        &ctx.workspace_dir,
        &ctx.paths.root,
        true,
    )?;
    if !resolved.is_dir() {
        return Err(format!("Không phải thư mục: {}", resolved.display()));
    }

    let ws = ctx.workspace_dir.clone();
    let root = ctx.paths.root.clone();
    let dir_clone = resolved.clone();
    let (entries, truncated) = tokio::task::spawn_blocking(move || -> Result<(Vec<Value>, bool), String> {
        let rd = std::fs::read_dir(&dir_clone)
            .map_err(|e| format!("Không đọc được thư mục: {e}"))?;
        let mut out = Vec::new();
        let mut truncated = false;
        for entry_res in rd {
            let Ok(entry) = entry_res else {
                continue;
            };
            let entry_path = entry.path();
            if let Ok(entry_res_path) = resolve_path_safely(&entry_path) {
                if is_path_forbidden(&entry_res_path, &ws, &root) {
                    continue;
                }
            }
            if out.len() >= max_entries {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let size_bytes = meta.as_ref().map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0);
            let modified_ts = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(json!({
                "name": name,
                "path": file_dialog::normalize_os_display_path(&entry_path).to_string_lossy(),
                "is_dir": is_dir,
                "size_bytes": size_bytes,
                "modified_ts": modified_ts,
            }));
        }
        Ok((out, truncated))
    })
    .await
    .map_err(|e| format!("Lỗi tác vụ đọc thư mục: {e}"))??;

    let display = file_dialog::normalize_os_display_path(&resolved)
        .to_string_lossy()
        .into_owned();
    Ok((
        format!("Liệt kê {} mục trong {display}", entries.len()),
        json!({
            "path": display,
            "count": entries.len(),
            "truncated": truncated,
            "entries": entries,
        }),
    ))
}

async fn tool_win_read_file(
    args: &Value,
    cfg: &AiConfig,
    ctx: &McpServerContext,
) -> Result<(String, Value), String> {
    let raw_path = req_str(args, "path")?;
    let max_bytes = opt_usize(args, "max_bytes")
        .unwrap_or(MAX_WIN_READ_BYTES_DEFAULT)
        .clamp(1, MAX_WIN_READ_BYTES_CAP);

    let resolved = validate_win_path(
        &raw_path,
        cfg.system_access_level,
        cfg,
        &ctx.workspace_dir,
        &ctx.paths.root,
        true,
    )?;
    if !resolved.is_file() {
        return Err(format!("Không phải tệp hợp lệ: {}", resolved.display()));
    }

    let path_clone = resolved.clone();
    let (content, total_bytes, truncated) =
        tokio::task::spawn_blocking(move || -> Result<(String, u64, bool), String> {
            use std::io::Read;
            let meta = std::fs::metadata(&path_clone)
                .map_err(|e| format!("Không đọc được thông tin tệp: {e}"))?;
            let total = meta.len();
            let mut file = std::fs::File::open(&path_clone)
                .map_err(|e| format!("Không mở được tệp: {e}"))?;
            let mut buf = Vec::with_capacity((total as usize).min(max_bytes + 1));
            file.by_ref()
                .take((max_bytes + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| format!("Lỗi đọc tệp: {e}"))?;
            let truncated = buf.len() > max_bytes;
            if truncated {
                buf.truncate(max_bytes);
            }
            let text = match String::from_utf8(buf) {
                Ok(s) => s,
                Err(e) => {
                    let valid_up_to = e.utf8_error().valid_up_to();
                    if truncated && valid_up_to > 0 && valid_up_to + 4 >= e.as_bytes().len() {
                        String::from_utf8_lossy(&e.as_bytes()[..valid_up_to]).into_owned()
                    } else {
                        return Err(format!(
                            "Tệp không phải văn bản UTF-8 ({total} bytes). Hãy dùng win_run_powershell/Python để đọc tệp nhị phân (.xlsx/.docx/.pdf)."
                        ));
                    }
                }
            };
            Ok((text, total, truncated))
        })
        .await
        .map_err(|e| format!("Lỗi tác vụ đọc tệp: {e}"))??;

    let display = file_dialog::normalize_os_display_path(&resolved)
        .to_string_lossy()
        .into_owned();
    Ok((
        format!("Đọc tệp {display} ({} bytes)", content.len()),
        json!({
            "path": display,
            "size_bytes": total_bytes,
            "truncated": truncated,
            "content": content,
        }),
    ))
}

async fn tool_win_write_file(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let raw_path = req_str(args, "path")
        .map_err(|e| ("auto".into(), "Ghi tệp Windows".into(), e))?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ("auto".into(), "Ghi tệp Windows".into(), "Thiếu nội dung content".into()))?
        .to_string();
    let append = opt_bool(args, "append").unwrap_or(false);

    let resolved = validate_win_path(
        &raw_path,
        cfg.system_access_level,
        cfg,
        &ctx.workspace_dir,
        &ctx.paths.root,
        false,
    )
    .map_err(|e| ("blocked".into(), format!("Chặn ghi tệp: {raw_path}"), e))?;

    let display_path = file_dialog::normalize_os_display_path(&resolved)
        .to_string_lossy()
        .into_owned();
    let mode = if append { "append" } else { "overwrite" };
    let summary = format!("Ghi tệp ({mode}): {display_path} ({} bytes)", content.len());
    let need_approval = cfg.system_access_level == SystemAccessLevel::Supervised;

    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "win_write_file",
        "win_write_file",
        "system",
        "medium",
        format!("Ghi tệp Windows ({mode})"),
        summary.clone(),
        json!({
            "path": display_path,
            "append": append,
            "mode": mode,
            "bytes": content.len(),
            "content_preview": truncate_chars(&content, 1200),
        }),
    )
    .await?;

    let target = resolved.clone();
    let bytes_len = content.len();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Không tạo được thư mục cha: {e}"))?;
        }
        if append {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target)
                .map_err(|e| format!("Không mở được tệp để ghi nối: {e}"))?;
            file.write_all(content.as_bytes())
                .map_err(|e| format!("Lỗi ghi tệp: {e}"))?;
        } else {
            std::fs::write(&target, content.as_bytes())
                .map_err(|e| format!("Lỗi ghi tệp: {e}"))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| (decision.audit_str().into(), summary.clone(), format!("Lỗi tác vụ ghi tệp: {e}")))?
    .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;

    Ok((
        decision,
        summary,
        json!({
            "ok": true,
            "path": display_path,
            "bytes_written": bytes_len,
            "mode": mode,
        }),
    ))
}

async fn read_capped_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    cap: usize,
) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let take = (cap - out.len()).min(n);
                    out.extend_from_slice(&buf[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    (out, truncated)
}

async fn tool_win_run_powershell(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let command = req_str(args, "command")
        .map_err(|e| ("auto".into(), "Chạy lệnh PowerShell".into(), e))?;
    if command.len() > MAX_PS_COMMAND_BYTES {
        return Err((
            "blocked".into(),
            "Chạy lệnh PowerShell".into(),
            "Lệnh PowerShell vượt quá giới hạn 32 KiB".into(),
        ));
    }
    let timeout_secs = opt_u64(args, "timeout_secs").unwrap_or(60).clamp(1, 120);

    let cwd_path = if let Some(raw_cwd) = opt_str(args, "cwd").map(str::trim).filter(|s| !s.is_empty()) {
        let resolved = validate_win_path(
            raw_cwd,
            cfg.system_access_level,
            cfg,
            &ctx.workspace_dir,
            &ctx.paths.root,
            true,
        )
        .map_err(|e| ("blocked".into(), format!("Chặn cwd PowerShell: {raw_cwd}"), e))?;
        if !resolved.is_dir() {
            return Err((
                "auto".into(),
                "Chạy lệnh PowerShell".into(),
                format!("Thư mục làm việc cwd không phải thư mục: {}", resolved.display()),
            ));
        }
        resolved
    } else {
        std::fs::create_dir_all(&ctx.workspace_dir).map_err(|e| {
            (
                "auto".into(),
                "Chạy lệnh PowerShell".into(),
                format!("Không tạo được thư mục ai_workspace: {e}"),
            )
        })?;
        resolve_path_safely(&ctx.workspace_dir).unwrap_or_else(|_| ctx.workspace_dir.clone())
    };

    let cwd_display = file_dialog::normalize_os_display_path(&cwd_path)
        .to_string_lossy()
        .into_owned();
    let destructive = is_destructive_powershell_command(&command);
    let risk = if destructive { "high" } else { "medium" };
    let summary = format!("PowerShell: {}", truncate_chars(&command, 100));
    let need_approval = cfg.system_access_level == SystemAccessLevel::Supervised;

    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "win_run_powershell",
        if destructive {
            "win_run_powershell:destructive"
        } else {
            "win_run_powershell"
        },
        "system",
        risk,
        if destructive {
            "Chạy lệnh PowerShell rủi ro cao".into()
        } else {
            "Chạy lệnh PowerShell".into()
        },
        summary.clone(),
        json!({
            "command": command,
            "cwd": cwd_display,
            "timeout_secs": timeout_secs,
        }),
    )
    .await?;

    let wrapped_script = format!(
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; $OutputEncoding=[System.Text.Encoding]::UTF8;\n{command}"
    );
    let utf16_bytes: Vec<u8> = wrapped_script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let encoded_cmd = B64_STANDARD.encode(&utf16_bytes);

    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encoded_cmd,
    ])
    .current_dir(&cwd_path)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| {
        (
            decision.audit_str().into(),
            summary.clone(),
            format!("Không khởi chạy được powershell.exe: {e}"),
        )
    })?;

    let pid = child.id().unwrap_or(0);
    let _pid_guard = if pid != 0 {
        Some(TrackedPidGuard::new(ctx.ps_tracker.clone(), pid))
    } else {
        None
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        match stdout_pipe {
            Some(p) => read_capped_stream(p, MAX_PS_OUTPUT_BYTES).await,
            None => (Vec::new(), false),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr_pipe {
            Some(p) => read_capped_stream(p, MAX_PS_OUTPUT_BYTES).await,
            None => (Vec::new(), false),
        }
    });

    let mut estop_rx = ctx.ps_tracker.subscribe_estop();
    let sleep = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(sleep);

    let (exit_code, timed_out, stopped_by_estop) = tokio::select! {
        status_res = child.wait() => {
            let code = status_res.ok().and_then(|s| s.code()).unwrap_or(-1);
            (Some(code), false, false)
        }
        _ = &mut sleep => {
            if pid != 0 {
                let _ = tokio::task::spawn_blocking(move || kill_process_tree_sync(pid)).await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true, false)
        }
        changed = estop_rx.changed() => {
            let _ = changed;
            if pid != 0 {
                let _ = tokio::task::spawn_blocking(move || kill_process_tree_sync(pid)).await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, false, true)
        }
    };

    let (stdout_bytes, stdout_truncated) =
        tokio::time::timeout(Duration::from_secs(2), stdout_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
    let (stderr_bytes, stderr_truncated) =
        tokio::time::timeout(Duration::from_secs(2), stderr_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if stopped_by_estop {
        return Err((
            decision.audit_str().into(),
            summary,
            "Tiến trình PowerShell đã bị dừng khẩn cấp bởi E-Stop.".into(),
        ));
    }

    let ok = !timed_out && exit_code == Some(0);
    Ok((
        decision,
        summary,
        json!({
            "ok": ok,
            "exit_code": exit_code,
            "timed_out": timed_out,
            "cwd": cwd_display,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
        }),
    ))
}

async fn tool_win_open_path(
    args: &Value,
    cfg: &AiConfig,
    session_id: &str,
    ctx: &McpServerContext,
) -> Result<(ApprovalOutcome, String, Value), (String, String, String)> {
    let raw_path = req_str(args, "path")
        .map_err(|e| ("auto".into(), "Mở tệp/thư mục".into(), e))?;

    let resolved = validate_win_path(
        &raw_path,
        cfg.system_access_level,
        cfg,
        &ctx.workspace_dir,
        &ctx.paths.root,
        true,
    )
    .map_err(|e| ("blocked".into(), format!("Chặn mở đường dẫn: {raw_path}"), e))?;

    if resolved.is_file() && is_blocked_open_extension(&resolved) {
        return Err((
            "blocked".into(),
            format!("Chặn mở tệp thực thi: {}", resolved.display()),
            "Vì lý do an toàn, win_open_path chặn mở trực tiếp tệp thực thi/script (.exe, .bat, .cmd, .ps1, .vbs, .js, .msi, .lnk...).".into(),
        ));
    }

    let display_path = file_dialog::normalize_os_display_path(&resolved)
        .to_string_lossy()
        .into_owned();
    let summary = format!("Mở đường dẫn: {display_path}");
    let need_approval = cfg.system_access_level == SystemAccessLevel::Supervised;

    let decision = maybe_require_approval(
        ctx,
        need_approval,
        session_id,
        "win_open_path",
        "win_open_path",
        "system",
        "medium",
        "Mở tệp / thư mục Windows".into(),
        summary.clone(),
        json!({ "path": display_path }),
    )
    .await?;

    let target = resolved.clone();
    tokio::task::spawn_blocking(move || {
        if target.is_dir() {
            file_dialog::open_directory(&target)
        } else {
            file_dialog::open_local_file(&target)
        }
    })
    .await
    .map_err(|e| (decision.audit_str().into(), summary.clone(), format!("Lỗi tác vụ mở đường dẫn: {e}")))?
    .map_err(|e| (decision.audit_str().into(), summary.clone(), e))?;

    Ok((
        decision,
        summary,
        json!({ "ok": true, "path": display_path }),
    ))
}

// ============================================================================
// Argument & Time Helpers
// ============================================================================

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("Thiếu tham số bắt buộc '{key}'"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

fn opt_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

// ============================================================================
// Unit & Loopback HTTP Integration Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use zalo_core::chat::{QuoteRef, ThreadDto};
    use zalo_core::media_vault::MediaVaultConfig;
    use zalo_core::relay::RelayConfig;

    fn make_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "teza_mcp_test_{label}_{}_{}",
            std::process::id(),
            MCP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    async fn spawn_test_bridge(
        label: &str,
        cfg: AiConfig,
    ) -> (
        PathBuf,
        Arc<TezaMcpBridge>,
        SocketAddr,
        Arc<Mutex<MessageStore>>,
        Arc<Mutex<AiConfig>>,
        Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    ) {
        let root = make_temp_dir(label);
        let paths = StoragePaths { root: root.clone() };
        let store = MessageStore::open(&paths.history_db(None)).expect("open store");
        let vault = MediaVault::open(&paths, None, MediaVaultConfig::default()).expect("open vault");
        let session = Session::new_with_relay(RelayConfig::default()).expect("new session");

        let store_arc = Arc::new(Mutex::new(store));
        let ai_config_arc = Arc::new(Mutex::new(cfg));
        let deps = McpBridgeDeps {
            session: Arc::new(Mutex::new(session)),
            store: store_arc.clone(),
            media_vault: Arc::new(RwLock::new(Arc::new(vault))),
            ai_config: ai_config_arc.clone(),
            active_thread_id: Arc::new(Mutex::new(Some("t-1".to_string()))),
            scheduler: SchedulerHandle::new(),
        };

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = McpEventSink::capture(events.clone());
        let bridge = Arc::new(TezaMcpBridge::new(paths));
        let addr = bridge
            .start_with_sink(sink, deps)
            .await
            .expect("start mcp bridge");

        (root, bridge, addr, store_arc, ai_config_arc, events)
    }

    async fn send_raw_http(addr: SocketAddr, raw_req: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect loopback");
        stream.write_all(raw_req.as_bytes()).await.expect("write req");
        stream.flush().await.expect("flush req");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read resp");
        let text = String::from_utf8_lossy(&buf).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    async fn post_mcp(addr: SocketAddr, token: &str, payload: &Value) -> (u16, Value) {
        let body = payload.to_string();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let (status, resp_body) = send_raw_http(addr, &req).await;
        let json_val = if resp_body.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&resp_body).unwrap_or(Value::Null)
        };
        (status, json_val)
    }

    #[tokio::test]
    async fn mcp_http_auth_host_and_origin_security() {
        let (root, bridge, addr, _, _, _) =
            spawn_test_bridge("security", AiConfig::default()).await;
        let token = bridge.bearer_token_for_test().to_string();
        let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string();

        // 1. Missing Bearer -> 401
        let req_missing = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: {}\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let (st, _) = send_raw_http(addr, &req_missing).await;
        assert_eq!(st, 401);

        // 2. Wrong Bearer -> 401
        let req_wrong = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer wrongtoken\r\nContent-Length: {}\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let (st, _) = send_raw_http(addr, &req_wrong).await;
        assert_eq!(st, 401);

        // 3. Browser Origin header -> 403
        let req_origin = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://evil.example\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let (st, _) = send_raw_http(addr, &req_origin).await;
        assert_eq!(st, 403);

        // 4. Non-loopback Host -> 403
        let req_host = format!(
            "POST /mcp HTTP/1.1\r\nHost: evil.example:{}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
            addr.port(),
            body.len()
        );
        let (st, _) = send_raw_http(addr, &req_host).await;
        assert_eq!(st, 403);

        // 5. Valid initialize & notifications/initialized
        let (st, init_val) = post_mcp(
            addr,
            &token,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;
        assert_eq!(st, 200);
        assert_eq!(init_val["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(init_val["result"]["serverInfo"]["name"], "teza");

        let (st_notif, _) = post_mcp(
            addr,
            &token,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(st_notif, 202);

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_tools_list_reflects_live_2axis_matrix() {
        let (root, bridge, addr, _, ai_cfg_arc, _) =
            spawn_test_bridge("matrix", AiConfig::default()).await;
        let token = bridge.bearer_token_for_test().to_string();

        let cases = [
            (TezaAccessLevel::Isolated, SystemAccessLevel::Safe, 0usize),
            (TezaAccessLevel::ReadOnly, SystemAccessLevel::Safe, 10),
            (TezaAccessLevel::Supervised, SystemAccessLevel::Safe, 21),
            (TezaAccessLevel::Autonomous, SystemAccessLevel::Safe, 21),
            (TezaAccessLevel::Isolated, SystemAccessLevel::Supervised, 5),
            (TezaAccessLevel::Supervised, SystemAccessLevel::Supervised, 26),
            (TezaAccessLevel::Autonomous, SystemAccessLevel::Full, 26),
        ];

        for (teza, sys, expected) in cases {
            {
                let mut cfg = ai_cfg_arc.lock().await;
                cfg.teza_access_level = teza;
                cfg.system_access_level = sys;
            }
            let (st, resp) = post_mcp(
                addr,
                &token,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            )
            .await;
            assert_eq!(st, 200);
            let tools = resp["result"]["tools"].as_array().expect("tools array");
            assert_eq!(
                tools.len(),
                expected,
                "unexpected tool count for ({teza:?}, {sys:?})"
            );

            let cfg_snap = ai_cfg_arc.lock().await.clone();
            let ep = bridge.endpoint_config_for(&cfg_snap).await;
            if expected == 0 {
                assert!(ep.is_none());
            } else {
                assert_eq!(ep.expect("endpoint config").exposed_tool_names.len(), expected);
            }
        }

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_read_messages_search_and_draft_message_roundtrip() {
        let cfg = AiConfig {
            teza_access_level: TezaAccessLevel::ReadOnly,
            ..AiConfig::default()
        };
        let (root, bridge, addr, store_arc, _, events) = spawn_test_bridge("read_draft", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();

        {
            let mut store = store_arc.lock().await;
            store
                .upsert_thread(&ThreadDto {
                    id: "t-1".into(),
                    title: "Nguyễn Văn An".into(),
                    avatar: String::new(),
                    kind: ThreadKind::Direct,
                    last_preview: "Báo cáo doanh số quý 4".into(),
                    last_ts: 1_700_000_000_000,
                    unread_count: 2,
                    is_pinned: false,
                    is_muted: false,
                    last_read_in_ts: 0,
                    peer_read_ts: 0,
                    muted_until_ts: 0,
                    is_archived: false,
                })
                .expect("upsert thread");
            store
                .upsert_message(&MessageDto {
                    msg_id: "m-101".into(),
                    cli_msg_id: None,
                    thread_id: "t-1".into(),
                    sender_id: "u-an".into(),
                    sender_name: "Nguyễn Văn An".into(),
                    text: "Đã gửi báo cáo doanh số quý 4 rồi nhé".into(),
                    msg_type: "chat.text".into(),
                    ts: 1_700_000_000_000,
                    is_outgoing: false,
                    is_recalled: false,
                    is_pinned: false,
                    quote: Some(QuoteRef {
                        msg_id: "m-100".into(),
                        sender_name: "Tôi".into(),
                        snippet: "Nhớ gửi báo cáo".into(),
                    }),
                    reactions: Vec::new(),
                    attachment_url: None,
                    attachment_name: None,
                })
                .expect("upsert msg");
        }

        // get_messages
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "get_messages",
                    "arguments": { "thread_id": "t-1", "limit": 10 }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        assert!(resp["result"].get("isError").is_none());
        let body_text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body_text).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["messages"][0]["msg_id"], "m-101");
        assert!(parsed["messages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("báo cáo doanh số"));

        // search_messages (accent-insensitive Vietnamese!)
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": {
                    "name": "search_messages",
                    "arguments": { "query": "bao cao doanh so" }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        let search_text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let search_val: Value = serde_json::from_str(search_text).unwrap();
        assert_eq!(search_val["messages"].as_array().unwrap().len(), 1);
        assert_eq!(search_val["messages"][0]["msg_id"], "m-101");

        // draft_message -> emits core://ai-compose-draft
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 12,
                "method": "tools/call",
                "params": {
                    "name": "draft_message",
                    "arguments": {
                        "thread_id": "t-1",
                        "text": "Cảm ơn An, mình đã nhận được!",
                        "open_thread": true
                    }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        assert!(resp["result"].get("isError").is_none());
        let captured = events.lock().unwrap().clone();
        let draft_ev = captured
            .iter()
            .find(|(name, _)| name == "core://ai-compose-draft")
            .expect("draft event emitted");
        assert_eq!(draft_ev.1["thread_id"], "t-1");
        assert_eq!(draft_ev.1["text"], "Cảm ơn An, mình đã nhận được!");
        assert_eq!(draft_ev.1["open_thread"], true);

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_interactive_approval_allow_and_deny_win_write_file() {
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Supervised,
            ..AiConfig::default()
        };
        let (root, bridge, addr, _, _, events) = spawn_test_bridge("approval", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();
        bridge.set_active_session_id("sess-alpha");

        let ws = managed_ai_workspace_dir(&root);
        let ok_file = ws.join("approved_note.txt");
        let deny_file = ws.join("denied_note.txt");

        // Case 1: allow_once -> writes file cleanly
        let token_c = token.clone();
        let ok_path_str = ok_file.to_string_lossy().into_owned();
        let call_task = tokio::spawn(async move {
            post_mcp(
                addr,
                &token_c,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": "tools/call",
                    "params": {
                        "name": "win_write_file",
                        "arguments": {
                            "path": ok_path_str,
                            "content": "Xin chào từ Teza MCP!"
                        }
                    }
                }),
            )
            .await
        });

        let approval_id = loop {
            let found = {
                let guard = events.lock().unwrap();
                guard.iter().find_map(|(ev, payload)| {
                    if ev == "core://ai-tool-approval" {
                        payload["approval_id"].as_str().map(str::to_string)
                    } else {
                        None
                    }
                })
            };
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        };

        assert!(bridge.resolve_approval(&approval_id, "allow_once").await);
        let (st, resp) = call_task.await.expect("join allow task");
        assert_eq!(st, 200);
        assert!(resp["result"].get("isError").is_none());
        assert_eq!(
            std::fs::read_to_string(&ok_file).expect("read written file"),
            "Xin chào từ Teza MCP!"
        );

        // Case 2: deny -> does NOT write file and returns isError=true
        events.lock().unwrap().clear();
        let token_d = token.clone();
        let deny_path_str = deny_file.to_string_lossy().into_owned();
        let deny_task = tokio::spawn(async move {
            post_mcp(
                addr,
                &token_d,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": "tools/call",
                    "params": {
                        "name": "win_write_file",
                        "arguments": {
                            "path": deny_path_str,
                            "content": "should not be written"
                        }
                    }
                }),
            )
            .await
        });

        let deny_approval_id = loop {
            let found = {
                let guard = events.lock().unwrap();
                guard.iter().find_map(|(ev, payload)| {
                    if ev == "core://ai-tool-approval" {
                        payload["approval_id"].as_str().map(str::to_string)
                    } else {
                        None
                    }
                })
            };
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        };

        assert!(bridge.resolve_approval(&deny_approval_id, "deny").await);
        let (st, resp) = deny_task.await.expect("join deny task");
        assert_eq!(st, 200);
        assert_eq!(resp["result"]["isError"], true);
        assert!(!deny_file.exists());

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_guard_blocks_system_ssh_and_teza_db_even_in_s2_but_allows_ai_workspace() {
        let root = make_temp_dir("path_guard");
        let ws = managed_ai_workspace_dir(&root);
        std::fs::create_dir_all(&ws).expect("create ws");
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Full,
            ..AiConfig::default()
        };

        // 1. Teza private DB inside data root -> BLOCKED even in S2!
        let db_path = root.join("history.db").to_string_lossy().into_owned();
        assert!(
            validate_win_path(&db_path, SystemAccessLevel::Full, &cfg, &ws, &root, false).is_err()
        );

        // 2. Teza ai_workspace -> ALLOWED!
        let ws_file = ws.join("report.csv").to_string_lossy().into_owned();
        assert!(
            validate_win_path(&ws_file, SystemAccessLevel::Full, &cfg, &ws, &root, false).is_ok()
        );

        // 3. C:\Windows\System32 -> BLOCKED even in S2!
        #[cfg(target_os = "windows")]
        assert!(validate_win_path(
            r"C:\Windows\System32\drivers\etc\hosts",
            SystemAccessLevel::Full,
            &cfg,
            &ws,
            &root,
            false
        )
        .is_err());

        // 4. ~/.ssh -> BLOCKED even in S2!
        if let Some(home) = user_home_dir() {
            let ssh_key = home.join(".ssh").join("id_rsa").to_string_lossy().into_owned();
            assert!(
                validate_win_path(&ssh_key, SystemAccessLevel::Full, &cfg, &ws, &root, false)
                    .is_err()
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn mcp_win_run_powershell_utf8_vietnamese_stdout() {
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Full,
            ..AiConfig::default()
        };
        let (root, bridge, addr, _, _, _) = spawn_test_bridge("ps_utf8", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();

        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 30,
                "method": "tools/call",
                "params": {
                    "name": "win_run_powershell",
                    "arguments": {
                        "command": "Write-Output 'Xin chào Teza — Tiếng Việt UTF-8!'"
                    }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        assert!(resp["result"].get("isError").is_none());
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["exit_code"], 0);
        assert!(
            parsed["stdout"]
                .as_str()
                .unwrap()
                .contains("Xin chào Teza — Tiếng Việt UTF-8!"),
            "unexpected powershell stdout: {:?}",
            parsed["stdout"]
        );

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_emergency_stop_cancels_pending_approvals_and_clears_session_grants() {
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Supervised,
            ..AiConfig::default()
        };
        let (root, bridge, addr, _, _, events) = spawn_test_bridge("estop", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();
        bridge.set_active_session_id("sess-estop");

        let ws = managed_ai_workspace_dir(&root);
        let p1 = ws.join("grant_test.txt").to_string_lossy().into_owned();
        let p2 = ws.join("estop_pending.txt").to_string_lossy().into_owned();

        // First call: approve with `allow_session` to populate session grant.
        let tok1 = token.clone();
        let p1_c = p1.clone();
        let t1 = tokio::spawn(async move {
            post_mcp(
                addr,
                &tok1,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 40,
                    "method": "tools/call",
                    "params": {
                        "name": "win_write_file",
                        "arguments": { "path": p1_c, "content": "one" }
                    }
                }),
            )
            .await
        });
        let apr1 = loop {
            let found = events.lock().unwrap().iter().find_map(|(ev, p)| {
                (ev == "core://ai-tool-approval")
                    .then(|| p["approval_id"].as_str().map(str::to_string))
                    .flatten()
            });
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        };
        assert!(bridge.resolve_approval(&apr1, "allow_session").await);
        let (st1, _) = t1.await.unwrap();
        assert_eq!(st1, 200);

        // Now switch to another session so a new pending approval blocks, then fire E-Stop.
        bridge.set_active_session_id("sess-estop-2");
        events.lock().unwrap().clear();
        let tok2 = token.clone();
        let p2_c = p2.clone();
        let t2 = tokio::spawn(async move {
            post_mcp(
                addr,
                &tok2,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 41,
                    "method": "tools/call",
                    "params": {
                        "name": "win_write_file",
                        "arguments": { "path": p2_c, "content": "should cancel" }
                    }
                }),
            )
            .await
        });
        loop {
            if bridge
                .status(&AiConfig {
                    system_access_level: SystemAccessLevel::Supervised,
                    ..AiConfig::default()
                })
                .await
                .pending_approvals_count
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }

        let estop_res = bridge.emergency_stop().await;
        assert_eq!(estop_res.cancelled_approvals, 1);
        let (st2, resp2) = t2.await.unwrap();
        assert_eq!(st2, 200);
        assert_eq!(resp2["result"]["isError"], true);
        assert!(!PathBuf::from(&p2).exists());

        // Verify previous `sess-estop` session grant was ALSO cleared by E-Stop!
        assert!(bridge
            .approvals
            .session_grants
            .lock()
            .unwrap()
            .is_empty());

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_t3_allowed_thread_ids_blocks_unlisted_writes_and_schedules_roundtrip() {
        let cfg = AiConfig {
            teza_access_level: TezaAccessLevel::Autonomous,
            zeroclaw_allowed_thread_ids: vec!["t-allowed".to_string()],
            ..AiConfig::default()
        };
        let (root, bridge, addr, store_arc, _, events) = spawn_test_bridge("t3_scope", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();

        {
            let store = store_arc.lock().await;
            for tid in ["t-allowed", "t-blocked"] {
                store
                    .upsert_thread(&ThreadDto {
                        id: tid.into(),
                        title: format!("Chat {tid}"),
                        avatar: String::new(),
                        kind: ThreadKind::Direct,
                        last_preview: String::new(),
                        last_ts: 1_700_000_000_000,
                        unread_count: 1,
                        is_pinned: false,
                        is_muted: false,
                        last_read_in_ts: 0,
                        peer_read_ts: 0,
                        muted_until_ts: 0,
                        is_archived: false,
                    })
                    .expect("upsert thread");
            }
        }

        let future_ts = now_ms() + 3_600_000;

        // Blocked thread -> rejected with isError=true & audit decision == "blocked"
        let (st_blk, resp_blk) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 50,
                "method": "tools/call",
                "params": {
                    "name": "create_schedule",
                    "arguments": {
                        "thread_id": "t-blocked",
                        "text": "Should be blocked",
                        "send_at_ts": future_ts
                    }
                }
            }),
        )
        .await;
        assert_eq!(st_blk, 200);
        assert_eq!(resp_blk["result"]["isError"], true);

        // Allowed thread -> creates schedule cleanly in T3 without approval prompt!
        let (st_ok, resp_ok) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 51,
                "method": "tools/call",
                "params": {
                    "name": "create_schedule",
                    "arguments": {
                        "thread_id": "t-allowed",
                        "text": "Hẹn gửi báo cáo",
                        "send_at_ts": future_ts
                    }
                }
            }),
        )
        .await;
        assert_eq!(st_ok, 200);
        assert!(resp_ok["result"].get("isError").is_none());
        let created_json: Value =
            serde_json::from_str(resp_ok["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        let sched_id = created_json["schedule"]["id"]
            .as_str()
            .expect("schedule id")
            .to_string();

        // manage_schedule cancel on allowed thread succeeds
        let (st_can, resp_can) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 52,
                "method": "tools/call",
                "params": {
                    "name": "manage_schedule",
                    "arguments": {
                        "schedule_id": sched_id,
                        "action": "cancel"
                    }
                }
            }),
        )
        .await;
        assert_eq!(st_can, 200);
        assert!(resp_can["result"].get("isError").is_none());

        // manage_thread_state open_thread emits core://ai-open-thread
        let (st_open, _) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 53,
                "method": "tools/call",
                "params": {
                    "name": "manage_thread_state",
                    "arguments": {
                        "thread_id": "t-allowed",
                        "action": "open_thread"
                    }
                }
            }),
        )
        .await;
        assert_eq!(st_open, 200);
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|(ev, p)| ev == "core://ai-open-thread" && p["thread_id"] == "t-allowed"));

        // Audit logs recorded both blocked and auto entries
        let logs = bridge.list_audit_logs(Some(20));
        assert!(logs.iter().any(|l| l.decision == "blocked" && l.tool_name == "create_schedule"));
        assert!(logs.iter().any(|l| l.decision == "auto" && l.ok && l.tool_name == "create_schedule"));

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_win_list_read_and_open_path_blocks_executables_and_outside_roots() {
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Supervised,
            ..AiConfig::default()
        };
        let (root, bridge, addr, _, _, _) = spawn_test_bridge("win_tools", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();

        let ws = managed_ai_workspace_dir(&root);
        std::fs::create_dir_all(&ws).expect("create ws");
        let txt_path = ws.join("sample.txt");
        let exe_path = ws.join("evil.bat");
        std::fs::write(&txt_path, "Dữ liệu Excel UTF-8").expect("write txt");
        std::fs::write(&exe_path, "@echo hi").expect("write bat");

        // win_list_dir in ai_workspace -> auto-approved & returns both entries
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 60,
                "method": "tools/call",
                "params": {
                    "name": "win_list_dir",
                    "arguments": { "path": ws.to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        assert!(resp["result"].get("isError").is_none());
        let list_val: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(list_val["count"], 2);

        // win_read_file -> auto-approved & reads UTF-8 content
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 61,
                "method": "tools/call",
                "params": {
                    "name": "win_read_file",
                    "arguments": { "path": txt_path.to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        let read_val: Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(read_val["content"], "Dữ liệu Excel UTF-8");

        // win_open_path on .bat -> blocked immediately before approval!
        let (st, resp) = post_mcp(
            addr,
            &token,
            &json!({
                "jsonrpc": "2.0",
                "id": 62,
                "method": "tools/call",
                "params": {
                    "name": "win_open_path",
                    "arguments": { "path": exe_path.to_string_lossy() }
                }
            }),
        )
        .await;
        assert_eq!(st, 200);
        assert_eq!(resp["result"]["isError"], true);

        // High-risk approval downgrades allow_session -> allow_once (never stored in session_grants)
        assert!(is_destructive_powershell_command("Remove-Item C:\\temp\\a.txt"));
        assert!(!is_destructive_powershell_command("Get-ChildItem -Path ."));

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_high_risk_approval_never_grants_allow_session() {
        let mgr = McpApprovalManager::new(30);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = McpEventSink::capture(events.clone());

        let (tx, rx) = oneshot::channel::<ApprovalOutcome>();
        mgr.pending.lock().unwrap().insert(
            "apr-high-1".to_string(),
            PendingApproval {
                tx,
                session_id: "s1".to_string(),
                risk: "high".to_string(),
                grant_key: "manage_social:leave_group".to_string(),
            },
        );
        assert!(mgr.resolve_approval(Some(&sink), "apr-high-1", "allow_session"));
        let outcome = rx.await.expect("receive outcome");
        assert_eq!(outcome, ApprovalOutcome::AllowOnce);
        assert!(mgr.session_grants.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn mcp_permission_downgrade_and_per_session_cancel_clear_approvals_and_grants() {
        let cfg = AiConfig {
            system_access_level: SystemAccessLevel::Supervised,
            ..AiConfig::default()
        };
        let (root, bridge, addr, _, ai_cfg_arc, events) =
            spawn_test_bridge("perm_downgrade_session_cancel", cfg).await;
        let token = bridge.bearer_token_for_test().to_string();
        let ws = managed_ai_workspace_dir(&root);

        // 1. Permission downgrade (S1 -> S0) triggers emergency_stop: cancels pending approval,
        //    emits core://ai-tool-approval-resolved ("cancelled"), and prevents revoked execution.
        bridge.set_active_session_id("sess-downgrade");
        let revoked_file = ws.join("revoked_after_downgrade.txt");
        let tok1 = token.clone();
        let revoked_str = revoked_file.to_string_lossy().into_owned();
        let downgrade_task = tokio::spawn(async move {
            post_mcp(
                addr,
                &tok1,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 70,
                    "method": "tools/call",
                    "params": {
                        "name": "win_write_file",
                        "arguments": { "path": revoked_str, "content": "must not write" }
                    }
                }),
            )
            .await
        });
        let apr_downgrade = loop {
            let found = events.lock().unwrap().iter().find_map(|(ev, p)| {
                (ev == "core://ai-tool-approval")
                    .then(|| p["approval_id"].as_str().map(str::to_string))
                    .flatten()
            });
            if let Some(id) = found {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        };

        // Simulate `ai_update_config` permission downgrade (`S1 -> S0` while enabled stays true)
        {
            let mut live = ai_cfg_arc.lock().await;
            live.system_access_level = SystemAccessLevel::Safe;
        }
        let estop = bridge.emergency_stop().await;
        assert_eq!(estop.cancelled_approvals, 1);
        // Late resolve attempt after downgrade must fail cleanly
        assert!(!bridge.resolve_approval(&apr_downgrade, "allow_once").await);
        let (st_dg, resp_dg) = downgrade_task.await.unwrap();
        assert_eq!(st_dg, 200);
        assert_eq!(resp_dg["result"]["isError"], true);
        assert!(!revoked_file.exists());
        assert!(events.lock().unwrap().iter().any(|(ev, p)| {
            ev == "core://ai-tool-approval-resolved"
                && p["approval_id"] == apr_downgrade
                && p["decision"] == "cancelled"
        }));

        // 2. Per-session cancel_session_approvals(sid, clear_grants):
        //    Restore S1, seed session grants for `sess-a` and `sess-b`, plus pending approvals for both.
        {
            let mut live = ai_cfg_arc.lock().await;
            live.system_access_level = SystemAccessLevel::Supervised;
        }
        {
            let mut grants = bridge.approvals.session_grants.lock().unwrap();
            grants.insert(("sess-a".to_string(), "win_write_file".to_string()));
            grants.insert(("sess-b".to_string(), "win_write_file".to_string()));
        }
        let (tx_a, rx_a) = oneshot::channel::<ApprovalOutcome>();
        let (tx_b, rx_b) = oneshot::channel::<ApprovalOutcome>();
        {
            let mut map = bridge.approvals.pending.lock().unwrap();
            map.insert(
                "apr-a".to_string(),
                PendingApproval {
                    tx: tx_a,
                    session_id: "sess-a".to_string(),
                    risk: "medium".to_string(),
                    grant_key: "win_open_path".to_string(),
                },
            );
            map.insert(
                "apr-b".to_string(),
                PendingApproval {
                    tx: tx_b,
                    session_id: "sess-b".to_string(),
                    risk: "medium".to_string(),
                    grant_key: "win_open_path".to_string(),
                },
            );
        }
        events.lock().unwrap().clear();

        // `ai_chat_cancel("sess-a")` -> cancel_session_approvals("sess-a", false):
        // cancels `apr-a` only, emits resolved("cancelled"), preserves `sess-a` & `sess-b` grants and `apr-b`.
        assert_eq!(bridge.cancel_session_approvals("sess-a", false).await, 1);
        assert_eq!(rx_a.await.unwrap(), ApprovalOutcome::Cancelled);
        assert!(bridge
            .approvals
            .session_grants
            .lock()
            .unwrap()
            .contains(&("sess-a".to_string(), "win_write_file".to_string())));
        assert!(bridge.approvals.pending.lock().unwrap().contains_key("apr-b"));
        assert!(events.lock().unwrap().iter().any(|(ev, p)| {
            ev == "core://ai-tool-approval-resolved"
                && p["approval_id"] == "apr-a"
                && p["session_id"] == "sess-a"
                && p["decision"] == "cancelled"
        }));

        // `ai_delete_session("sess-a")` / `ai_clear_messages("sess-a")` -> clear_grants = true:
        // removes `sess-a` grants while keeping `sess-b` grants & `apr-b` untouched.
        assert_eq!(bridge.cancel_session_approvals("sess-a", true).await, 0);
        {
            let grants = bridge.approvals.session_grants.lock().unwrap();
            assert!(!grants.contains(&("sess-a".to_string(), "win_write_file".to_string())));
            assert!(grants.contains(&("sess-b".to_string(), "win_write_file".to_string())));
        }
        assert!(bridge.resolve_approval("apr-b", "allow_once").await);
        assert_eq!(rx_b.await.unwrap(), ApprovalOutcome::AllowOnce);

        bridge.stop().await;
        let _ = std::fs::remove_dir_all(&root);
    }
}
