import { useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  AlertTriangle,
  Check,
  CheckCircle2,
  Code2,
  Copy,
  KeyRound,
  Loader2,
  Lock,
  Plus,
  Power,
  RefreshCw,
  Save,
  Shield,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import type { ThreadDto } from "../types";
import { copyTextReliable } from "../utils/clipboard";
import {
  DEFAULT_LOCAL_API_MIN_INTERVAL_MS,
  DEFAULT_LOCAL_API_PORT,
  MAX_LOCAL_API_INTERVAL_MS,
  MIN_LOCAL_API_INTERVAL_MS,
  buildLocalApiCurlExample,
  buildLocalApiPowerShellExample,
  buildLocalApiPythonExample,
  formatAuditOutcome,
  formatLocalDateTime,
  formatThreadKindLabel,
  getAuditStatusTone,
  parseAllowedThreadIdsInput,
  sanitizeLocalApiPort,
  validateLocalApiMinIntervalMs,
  validateLocalApiPort,
  type ApiAuditLogDto,
  type ApiClientDto,
  type ApiClientScope,
  type BadgeTone,
  type CreateApiClientInput,
  type CreatedApiClientDto,
  type LocalApiConfigDto,
  type LocalApiStatusDto,
  type UpdateApiClientInput,
} from "../utils/automationHelpers";

export interface LocalApiPanelProps {
  config: LocalApiConfigDto | null;
  status: LocalApiStatusDto | null;
  clients: ApiClientDto[];
  auditLogs: ApiAuditLogDto[];
  threads: ThreadDto[];
  loading?: boolean;
  onSaveConfig: (config: LocalApiConfigDto) => Promise<LocalApiStatusDto | void>;
  onCreateClient: (input: CreateApiClientInput) => Promise<CreatedApiClientDto>;
  onUpdateClient: (id: string, input: UpdateApiClientInput) => Promise<ApiClientDto | void>;
  onDeleteClient: (id: string) => Promise<void>;
  onRefreshAudit: () => Promise<void> | void;
  onClearAudit: () => Promise<void>;
}

type SnippetTab = "curl" | "powershell" | "python";

const SCOPE_OPTIONS: { id: ApiClientScope; label: string; desc: string }[] = [
  { id: "send", label: "send", desc: "Gửi tin nhắn trực tiếp" },
  { id: "schedule", label: "schedule", desc: "Tạo tin nhắn hẹn giờ" },
  { id: "read_threads", label: "read_threads", desc: "Đọc danh sách hội thoại & lịch hẹn" },
];

function toneStyle(tone: BadgeTone): React.CSSProperties {
  switch (tone) {
    case "success":
      return { background: "rgba(34, 197, 94, 0.14)", color: "#4ade80" };
    case "warn":
      return { background: "rgba(245, 158, 11, 0.16)", color: "#fbbf24" };
    case "danger":
      return { background: "var(--danger-bg)", color: "var(--danger-text)" };
    case "info":
      return { background: "var(--accent-soft)", color: "var(--accent)" };
    case "neutral":
    default:
      return { background: "var(--chip-bg)", color: "var(--text-secondary)" };
  }
}

export function LocalApiPanel({
  config,
  status,
  clients,
  auditLogs,
  threads,
  loading = false,
  onSaveConfig,
  onCreateClient,
  onUpdateClient,
  onDeleteClient,
  onRefreshAudit,
  onClearAudit,
}: LocalApiPanelProps) {
  // Server config form state
  const [enabled, setEnabled] = useState(false);
  const [portInput, setPortInput] = useState(String(DEFAULT_LOCAL_API_PORT));
  const [minIntervalInput, setMinIntervalInput] = useState(String(DEFAULT_LOCAL_API_MIN_INTERVAL_MS));
  const [savingConfig, setSavingConfig] = useState(false);

  // Create API client form state
  const [appName, setAppName] = useState("");
  const [selectedScopes, setSelectedScopes] = useState<Set<ApiClientScope>>(
    () => new Set<ApiClientScope>(["send", "read_threads"]),
  );
  const [allowedIdsRaw, setAllowedIdsRaw] = useState("");
  const [pickerThreadId, setPickerThreadId] = useState("");
  const [rateLimitInput, setRateLimitInput] = useState("20");
  const [creatingClient, setCreatingClient] = useState(false);

  // One-time raw token reveal banner
  const [justCreated, setJustCreated] = useState<CreatedApiClientDto | null>(null);
  const [copiedToken, setCopiedToken] = useState(false);

  // Client list actions
  const [busyClientId, setBusyClientId] = useState<string | null>(null);
  const [confirmDeleteClientId, setConfirmDeleteClientId] = useState<string | null>(null);

  // Quick snippets
  const [snippetTab, setSnippetTab] = useState<SnippetTab>("curl");
  const [snippetThreadId, setSnippetThreadId] = useState("");
  const [copiedSnippet, setCopiedSnippet] = useState(false);

  // Audit logs
  const [refreshingAudit, setRefreshingAudit] = useState(false);
  const [clearingAudit, setClearingAudit] = useState(false);
  const [confirmClearAudit, setConfirmClearAudit] = useState(false);

  // General feedback
  const [feedback, setFeedback] = useState<{ tone: "ok" | "error"; text: string } | null>(null);

  const tokenTimerRef = useRef<number | null>(null);
  const snippetTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (tokenTimerRef.current !== null) window.clearTimeout(tokenTimerRef.current);
      if (snippetTimerRef.current !== null) window.clearTimeout(snippetTimerRef.current);
    };
  }, []);

  useEffect(() => {
    if (!config) return;
    setEnabled(config.enabled);
    setPortInput(String(config.port || DEFAULT_LOCAL_API_PORT));
    setMinIntervalInput(String(config.min_send_interval_ms ?? DEFAULT_LOCAL_API_MIN_INTERVAL_MS));
  }, [config]);

  useEffect(() => {
    if (!pickerThreadId && threads.length > 0) {
      setPickerThreadId(threads[0].id);
    }
    if (!snippetThreadId && threads.length > 0) {
      setSnippetThreadId(threads[0].id);
    }
  }, [threads, pickerThreadId, snippetThreadId]);

  const parsedAllowedIds = useMemo(
    () => parseAllowedThreadIdsInput(allowedIdsRaw),
    [allowedIdsRaw],
  );

  const threadTitleMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const th of threads) {
      map.set(th.id, th.title || th.id);
    }
    return map;
  }, [threads]);

  const activePort = useMemo(() => {
    const parsed = Number(portInput.trim());
    if (status?.port) return sanitizeLocalApiPort(status.port);
    return sanitizeLocalApiPort(parsed);
  }, [portInput, status?.port]);

  const snippetCode = useMemo(() => {
    const sampleThread = snippetThreadId.trim() || "123456789";
    const placeholder = "teza_YOUR_TOKEN";
    if (snippetTab === "powershell") {
      return buildLocalApiPowerShellExample(activePort, placeholder, sampleThread);
    }
    if (snippetTab === "python") {
      return buildLocalApiPythonExample(activePort, placeholder, sampleThread);
    }
    return buildLocalApiCurlExample(activePort, placeholder, sampleThread);
  }, [snippetTab, activePort, snippetThreadId]);

  const handleSaveConfig = async (e: React.FormEvent) => {
    e.preventDefault();
    if (savingConfig) return;
    setFeedback(null);

    const parsedPort = Number(portInput.trim());
    const portCheck = validateLocalApiPort(parsedPort);
    if (!portCheck.valid) {
      setFeedback({ tone: "error", text: portCheck.error || "Cổng không hợp lệ." });
      return;
    }

    const parsedInterval = Number(minIntervalInput.trim());
    const intervalCheck = validateLocalApiMinIntervalMs(parsedInterval);
    if (!intervalCheck.valid) {
      setFeedback({
        tone: "error",
        text: intervalCheck.error || "Khoảng cách gửi tối thiểu không hợp lệ.",
      });
      return;
    }

    setSavingConfig(true);
    try {
      await onSaveConfig({
        enabled,
        port: parsedPort,
        min_send_interval_ms: Math.round(parsedInterval),
      });
      setFeedback({
        tone: "ok",
        text: enabled
          ? `Đã lưu và khởi chạy Local Automation API trên 127.0.0.1:${parsedPort}.`
          : "Đã tắt Local Automation API (Kill Switch đã kích hoạt).",
      });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSavingConfig(false);
    }
  };

  const toggleScope = (scope: ApiClientScope) => {
    setSelectedScopes((prev) => {
      const next = new Set(prev);
      if (next.has(scope)) next.delete(scope);
      else next.add(scope);
      return next;
    });
  };

  const handleAppendThreadToWhitelist = () => {
    const id = pickerThreadId.trim();
    if (!id) return;
    const existing = parseAllowedThreadIdsInput(allowedIdsRaw);
    if (existing.includes(id)) return;
    setAllowedIdsRaw(existing.length > 0 ? `${existing.join("\n")}\n${id}` : id);
  };

  const handleCreateClient = async (e: React.FormEvent) => {
    e.preventDefault();
    if (creatingClient) return;
    setFeedback(null);

    const cleanName = appName.trim();
    if (!cleanName) {
      setFeedback({ tone: "error", text: "Vui lòng nhập tên ứng dụng cần cấp Token." });
      return;
    }
    const scopes = Array.from(selectedScopes);
    if (scopes.length === 0) {
      setFeedback({ tone: "error", text: "Vui lòng chọn ít nhất một quyền (scope)." });
      return;
    }
    const parsedRate = Number(rateLimitInput.trim());
    if (!Number.isInteger(parsedRate) || parsedRate < 1 || parsedRate > 120) {
      setFeedback({
        tone: "error",
        text: "Giới hạn tần suất (req/phút) phải là số nguyên từ 1 đến 120.",
      });
      return;
    }

    setCreatingClient(true);
    try {
      const created = await onCreateClient({
        name: cleanName,
        scopes,
        allowed_thread_ids: parsedAllowedIds,
        rate_limit_per_min: parsedRate,
      });
      setJustCreated(created);
      setCopiedToken(false);
      setAppName("");
      setAllowedIdsRaw("");
      setFeedback({
        tone: "ok",
        text: `Đã tạo ứng dụng "${created.client.name}". Hãy sao chép Token ngay bây giờ!`,
      });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setCreatingClient(false);
    }
  };

  const handleCopyCreatedToken = () => {
    if (!justCreated) return;
    void copyTextReliable(justCreated.raw_token)
      .then(() => {
        setCopiedToken(true);
        if (tokenTimerRef.current !== null) window.clearTimeout(tokenTimerRef.current);
        tokenTimerRef.current = window.setTimeout(() => setCopiedToken(false), 2000);
      })
      .catch(() => {
        // Ignore clipboard failure
      });
  };

  const handleCopySnippet = () => {
    void copyTextReliable(snippetCode)
      .then(() => {
        setCopiedSnippet(true);
        if (snippetTimerRef.current !== null) window.clearTimeout(snippetTimerRef.current);
        snippetTimerRef.current = window.setTimeout(() => setCopiedSnippet(false), 1800);
      })
      .catch(() => {
        // Ignore clipboard failure
      });
  };

  const handleToggleClientEnabled = async (client: ApiClientDto) => {
    if (busyClientId) return;
    setBusyClientId(client.id);
    setFeedback(null);
    try {
      await onUpdateClient(client.id, { enabled: !client.enabled });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusyClientId(null);
    }
  };

  const handleDeleteClient = async (id: string) => {
    if (busyClientId) return;
    setBusyClientId(id);
    setFeedback(null);
    try {
      await onDeleteClient(id);
      setConfirmDeleteClientId(null);
      if (justCreated?.client.id === id) {
        setJustCreated(null);
      }
      setFeedback({ tone: "ok", text: "Đã thu hồi và xóa ứng dụng API." });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusyClientId(null);
    }
  };

  const handleRefreshAudit = async () => {
    if (refreshingAudit) return;
    setRefreshingAudit(true);
    try {
      await onRefreshAudit();
    } finally {
      setRefreshingAudit(false);
    }
  };

  const handleClearAudit = async () => {
    if (clearingAudit) return;
    setClearingAudit(true);
    setFeedback(null);
    try {
      await onClearAudit();
      setConfirmClearAudit(false);
      setFeedback({ tone: "ok", text: "Đã xóa nhật ký gọi Local API." });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setClearingAudit(false);
    }
  };

  if (loading && !config && !status) {
    return (
      <div
        className="flex items-center justify-center gap-2 rounded-xl p-6 text-xs"
        style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
      >
        <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
        <span>Đang tải cấu hình Local Automation API...</span>
      </div>
    );
  }

  const isRunning = Boolean(status?.running);
  const bindDisplay = status?.bind_addr || `127.0.0.1:${activePort}`;

  return (
    <div className="space-y-5">
      {/* Section 1: Security Banner & Server Kill Switch */}
      <section className="space-y-2.5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-[11.5px] font-semibold"
            style={{
              background: "var(--accent-soft)",
              color: "var(--accent)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <Shield className="h-3.5 w-3.5" />
            <span>Chỉ lắng nghe 127.0.0.1 — tắt mặc định</span>
          </div>

          <div
            className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11.5px] font-semibold"
            style={toneStyle(isRunning ? "success" : "neutral")}
          >
            <span
              className={`h-2 w-2 rounded-full ${
                isRunning ? "animate-pulse bg-emerald-400" : "bg-zinc-400"
              }`}
            />
            <span>{isRunning ? `Đang chạy · ${bindDisplay}` : "Đã dừng"}</span>
          </div>
        </div>

        <form
          onSubmit={(e) => void handleSaveConfig(e)}
          className="space-y-3.5 rounded-xl p-4"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <label className="flex cursor-pointer items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-2.5">
              <span
                className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-xl"
                style={{
                  background: enabled ? "rgba(34, 197, 94, 0.14)" : "var(--card-bg)",
                  color: enabled ? "#4ade80" : "var(--text-secondary)",
                }}
              >
                <Power className="h-4 w-4" />
              </span>
              <div className="min-w-0">
                <div className="text-[14px] font-semibold">
                  Bật Local Automation API (Kill Switch)
                </div>
                <p className="mt-0.5 text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  Cho phép script nội bộ trên chính máy tính này (PowerShell, Python, cURL, tự động
                  hóa) gửi hoặc hẹn giờ tin nhắn qua HTTP cục bộ. Tắt công tắc sẽ ngắt ngay cổng lắng
                  nghe.
                </p>
              </div>
            </div>
            <input
              type="checkbox"
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
              className="mt-1.5 h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)]"
            />
          </label>

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Cổng cục bộ (1024–65535)
              </label>
              <input
                type="number"
                min={1024}
                max={65535}
                value={portInput}
                onChange={(e) => setPortInput(e.target.value)}
                className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>

            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Giãn cách gửi tối thiểu ({MIN_LOCAL_API_INTERVAL_MS}–{MAX_LOCAL_API_INTERVAL_MS} ms)
              </label>
              <input
                type="number"
                min={MIN_LOCAL_API_INTERVAL_MS}
                max={MAX_LOCAL_API_INTERVAL_MS}
                step={100}
                value={minIntervalInput}
                onChange={(e) => setMinIntervalInput(e.target.value)}
                className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>
          </div>

          {status?.last_error && (
            <div
              className="flex items-start gap-2 rounded-xl px-3 py-2 text-xs"
              style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
            >
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span className="wrap-anywhere">{status.last_error}</span>
            </div>
          )}

          <div
            className="flex flex-wrap items-center justify-between gap-2 pt-2 text-[11px]"
            style={{
              borderTop: "1px solid var(--border-subtle)",
              color: "var(--text-secondary)",
            }}
          >
            <span className="inline-flex items-center gap-1.5">
              <ShieldCheck className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
              <span>Chặn DNS Rebinding · Chặn Origin CSRF · Bắt buộc Bearer Token</span>
            </span>

            <button
              type="submit"
              disabled={savingConfig}
              className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
            >
              {savingConfig ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Save className="h-3.5 w-3.5" />
              )}
              <span>Lưu & Áp dụng</span>
            </button>
          </div>
        </form>
      </section>

      {/* Global Feedback Banner */}
      {feedback && (
        <div
          className="flex items-start gap-2 rounded-xl px-3.5 py-2.5 text-xs font-medium"
          style={{
            background:
              feedback.tone === "ok" ? "rgba(34, 197, 94, 0.12)" : "var(--danger-bg)",
            color: feedback.tone === "ok" ? "#4ade80" : "var(--danger-text)",
          }}
        >
          {feedback.tone === "ok" ? (
            <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />
          ) : (
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          )}
          <span className="wrap-anywhere">{feedback.text}</span>
        </div>
      )}

      {/* Section 2: Apps & Tokens */}
      <section className="space-y-3">
        <div className="flex items-center justify-between gap-2">
          <h4
            className="text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--text-secondary)" }}
          >
            Ứng dụng & Token ({clients.length})
          </h4>
        </div>

        {/* One-time Created Token Highlight Box */}
        {justCreated && (
          <div
            className="space-y-2.5 rounded-xl p-4"
            style={{
              background: "rgba(245, 158, 11, 0.12)",
              border: "1px solid rgba(245, 158, 11, 0.42)",
            }}
          >
            <div className="flex items-start justify-between gap-2">
              <div className="flex items-start gap-2">
                <KeyRound className="mt-0.5 h-4 w-4 shrink-0 text-amber-400" />
                <div>
                  <div className="text-xs font-bold text-amber-300">
                    Token cho &quot;{justCreated.client.name}&quot; — CHỈ HIỂN THỊ MỘT LẦN DUY NHẤT
                  </div>
                  <p className="mt-0.5 text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                    Teza chỉ lưu mã băm SHA-256 của token. Hãy sao chép và cất giữ an toàn ngay
                    trước khi ẩn thông báo này.
                  </p>
                </div>
              </div>
              <button
                type="button"
                onClick={() => setJustCreated(null)}
                className="shrink-0 rounded-lg px-2 py-1 text-[11px] font-medium hover:opacity-80"
                style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
              >
                Đã lưu, ẩn đi
              </button>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <code
                className="selectable-text wrap-anywhere flex-1 rounded-lg px-3 py-2 font-mono text-xs font-semibold"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {justCreated.raw_token}
              </code>
              <button
                type="button"
                onClick={handleCopyCreatedToken}
                className="tf-btn inline-flex shrink-0 items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-semibold"
              >
                {copiedToken ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
                <span>{copiedToken ? "Đã chép Token" : "Sao chép Token"}</span>
              </button>
            </div>
          </div>
        )}

        {/* Create Client Form */}
        <form
          onSubmit={(e) => void handleCreateClient(e)}
          className="space-y-3.5 rounded-xl p-4"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <div className="text-xs font-semibold">Cấp Token mới cho ứng dụng cục bộ</div>

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="sm:col-span-2">
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Tên ứng dụng
              </label>
              <input
                value={appName}
                onChange={(e) => setAppName(e.target.value)}
                placeholder="VD: Script báo cáo sáng, n8n cục bộ..."
                className="w-full rounded-xl px-3 py-2 text-xs outline-none"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>

            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Giới hạn (req/phút)
              </label>
              <input
                type="number"
                min={1}
                max={120}
                value={rateLimitInput}
                onChange={(e) => setRateLimitInput(e.target.value)}
                className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>
          </div>

          {/* Scopes */}
          <div>
            <div className="mb-1.5 text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              Quyền truy cập (Scopes)
            </div>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
              {SCOPE_OPTIONS.map((sc) => {
                const checked = selectedScopes.has(sc.id);
                return (
                  <label
                    key={sc.id}
                    className="flex cursor-pointer items-start gap-2 rounded-xl px-3 py-2 text-xs transition"
                    style={{
                      background: checked ? "var(--accent-soft)" : "var(--card-bg)",
                      border: `1px solid ${checked ? "var(--accent)" : "var(--border-subtle)"}`,
                    }}
                  >
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={() => toggleScope(sc.id)}
                      className="mt-0.5 h-3.5 w-3.5 shrink-0 accent-[var(--accent)]"
                    />
                    <div className="min-w-0">
                      <div className="font-mono font-semibold">{sc.label}</div>
                      <div className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {sc.desc}
                      </div>
                    </div>
                  </label>
                );
              })}
            </div>
          </div>

          {/* Whitelist allowed_thread_ids */}
          <div className="space-y-1.5">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                Danh sách Thread ID được phép (tùy chọn — mỗi dòng hoặc ngăn bằng dấu phẩy)
              </label>
              {parsedAllowedIds.length > 0 && (
                <span
                  className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  {parsedAllowedIds.length} ID đã chọn
                </span>
              )}
            </div>

            {threads.length > 0 && (
              <div className="flex flex-wrap items-center gap-2">
                <select
                  value={pickerThreadId}
                  onChange={(e) => setPickerThreadId(e.target.value)}
                  className="min-w-0 flex-1 rounded-xl px-3 py-1.5 text-xs outline-none"
                  style={{
                    background: "var(--bg-stage)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                >
                  {threads.slice(0, 120).map((th) => (
                    <option key={th.id} value={th.id}>
                      [{formatThreadKindLabel(th.kind)}] {th.title || th.id} ({th.id})
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  onClick={handleAppendThreadToWhitelist}
                  className="hover-menu inline-flex shrink-0 items-center gap-1 rounded-xl px-3 py-1.5 text-xs font-semibold"
                  style={{
                    background: "var(--card-bg)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--accent)",
                  }}
                >
                  <Plus className="h-3.5 w-3.5" />
                  <span>Thêm vào danh sách</span>
                </button>
              </div>
            )}

            <textarea
              rows={2}
              value={allowedIdsRaw}
              onChange={(e) => setAllowedIdsRaw(e.target.value)}
              placeholder="Để trống để cho phép mọi hội thoại, hoặc nhập ID cụ thể (VD: 123456789, 987654321)"
              className="w-full resize-y rounded-xl px-3 py-2 font-mono text-xs outline-none"
              style={{
                background: "var(--bg-stage)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            />
          </div>

          <div className="flex justify-end">
            <button
              type="submit"
              disabled={creatingClient || !appName.trim()}
              className="tf-btn inline-flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
            >
              {creatingClient ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <KeyRound className="h-3.5 w-3.5" />
              )}
              <span>Tạo ứng dụng & cấp Token</span>
            </button>
          </div>
        </form>

        {/* Existing Clients List */}
        {clients.length === 0 ? (
          <div
            className="rounded-xl p-4 text-center text-xs"
            style={{
              background: "var(--bg-surface)",
              border: "1px dashed var(--border-subtle)",
              color: "var(--text-secondary)",
            }}
          >
            Chưa có ứng dụng nào được cấp Token. Hãy tạo một ứng dụng ở trên để bắt đầu tích hợp.
          </div>
        ) : (
          <div className="space-y-2">
            {clients.map((client) => {
              const isBusy = busyClientId === client.id;
              const isConfirmingDel = confirmDeleteClientId === client.id;
              return (
                <div
                  key={client.id}
                  className="space-y-2 rounded-xl p-3.5"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                    opacity: client.enabled ? 1 : 0.72,
                  }}
                >
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <div className="flex min-w-0 flex-wrap items-center gap-2">
                      <span className="truncate text-[13.5px] font-semibold">{client.name}</span>
                      <code
                        className="rounded-md px-2 py-0.5 font-mono text-[11px]"
                        style={{ background: "var(--card-bg)", color: "var(--text-secondary)" }}
                      >
                        {client.token_prefix}…
                      </code>
                      <span
                        className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                        style={toneStyle(client.enabled ? "success" : "neutral")}
                      >
                        {client.enabled ? "Đang bật" : "Đã khóa"}
                      </span>
                    </div>

                    <div className="flex items-center gap-2">
                      <label className="inline-flex cursor-pointer items-center gap-1.5 text-xs">
                        <input
                          type="checkbox"
                          disabled={Boolean(busyClientId)}
                          checked={client.enabled}
                          onChange={() => void handleToggleClientEnabled(client)}
                          className="h-3.5 w-3.5 cursor-pointer accent-[var(--accent)]"
                        />
                        <span style={{ color: "var(--text-secondary)" }}>Kích hoạt</span>
                      </label>

                      {isConfirmingDel ? (
                        <div className="flex items-center gap-1">
                          <button
                            type="button"
                            disabled={isBusy}
                            onClick={() => void handleDeleteClient(client.id)}
                            className="rounded-lg bg-rose-600 px-2.5 py-1 text-[11px] font-semibold text-white hover:bg-rose-500 disabled:opacity-50"
                          >
                            {isBusy ? "..." : "Xác nhận"}
                          </button>
                          <button
                            type="button"
                            onClick={() => setConfirmDeleteClientId(null)}
                            className="rounded-lg px-2 py-1 text-[11px]"
                            style={{ background: "var(--card-bg)", color: "var(--text-secondary)" }}
                          >
                            Hủy
                          </button>
                        </div>
                      ) : (
                        <button
                          type="button"
                          disabled={Boolean(busyClientId)}
                          onClick={() => setConfirmDeleteClientId(client.id)}
                          className="hover-menu inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11px] font-medium disabled:opacity-50"
                          style={{
                            background: "var(--danger-bg)",
                            color: "var(--danger-text)",
                          }}
                          title="Thu hồi & xóa Token"
                        >
                          <Trash2 className="h-3 w-3" />
                          <span>Thu hồi</span>
                        </button>
                      )}
                    </div>
                  </div>

                  {/* Badges row */}
                  <div className="flex flex-wrap items-center gap-1.5 text-[11px]">
                    {client.scopes.map((sc) => (
                      <span
                        key={sc}
                        className="rounded-md px-2 py-0.5 font-mono font-medium"
                        style={{
                          background: "var(--accent-soft)",
                          color: "var(--accent)",
                        }}
                      >
                        {sc}
                      </span>
                    ))}

                    {client.allowed_thread_ids.length > 0 ? (
                      <span
                        className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 font-medium"
                        style={toneStyle("info")}
                        title={client.allowed_thread_ids
                          .map((id) => `${threadTitleMap.get(id) ?? id} (${id})`)
                          .join(", ")}
                      >
                        <Lock className="h-3 w-3" />
                        <span>Giới hạn {client.allowed_thread_ids.length} hội thoại</span>
                      </span>
                    ) : (
                      <span
                        className="rounded-md px-2 py-0.5 font-medium"
                        style={toneStyle("warn")}
                      >
                        Mọi hội thoại
                      </span>
                    )}

                    <span
                      className="rounded-md px-2 py-0.5 font-mono"
                      style={{ background: "var(--card-bg)", color: "var(--text-secondary)" }}
                    >
                      {client.rate_limit_per_min} req/phút
                    </span>

                    <span className="ml-auto text-[11px]" style={{ color: "var(--text-secondary)" }}>
                      Dùng gần nhất:{" "}
                      {client.last_used_at > 0
                        ? formatLocalDateTime(client.last_used_at)
                        : "Chưa sử dụng"}
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>

      {/* Section 3: Quick Integration Examples */}
      <section className="space-y-2.5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h4
            className="inline-flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--text-secondary)" }}
          >
            <Code2 className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
            <span>Ví dụ tích hợp nhanh</span>
          </h4>

          {threads.length > 0 && (
            <select
              value={snippetThreadId}
              onChange={(e) => setSnippetThreadId(e.target.value)}
              className="max-w-[230px] rounded-lg px-2.5 py-1 text-[11.5px] outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
              title="Chọn hội thoại mẫu cho đoạn mã ví dụ"
            >
              {threads.slice(0, 60).map((th) => (
                <option key={th.id} value={th.id}>
                  Mẫu: {th.title || th.id} ({th.id})
                </option>
              ))}
            </select>
          )}
        </div>

        <div
          className="overflow-hidden rounded-xl"
          style={{
            background: "var(--bg-stage)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <div
            className="flex items-center justify-between gap-2 px-3 py-2"
            style={{
              background: "var(--bg-surface)",
              borderBottom: "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex items-center gap-1">
              {(
                [
                  { id: "curl" as const, label: "cURL" },
                  { id: "powershell" as const, label: "PowerShell" },
                  { id: "python" as const, label: "Python" },
                ]
              ).map((t) => {
                const active = snippetTab === t.id;
                return (
                  <button
                    key={t.id}
                    type="button"
                    onClick={() => setSnippetTab(t.id)}
                    className="rounded-lg px-2.5 py-1 text-xs font-medium transition"
                    style={{
                      background: active ? "var(--accent-soft)" : "transparent",
                      color: active ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    {t.label}
                  </button>
                );
              })}
            </div>

            <button
              type="button"
              onClick={handleCopySnippet}
              className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-xs font-medium transition"
              style={{
                border: "1px solid var(--border-subtle)",
                color: copiedSnippet ? "#4ade80" : "var(--accent)",
              }}
            >
              {copiedSnippet ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
              <span>{copiedSnippet ? "Đã chép" : "Sao chép"}</span>
            </button>
          </div>

          <pre className="selectable-text overflow-x-auto p-3.5 font-mono text-[12px] leading-relaxed">
            <code>{snippetCode}</code>
          </pre>
        </div>
      </section>

      {/* Section 4: API Audit Log */}
      <section className="space-y-2.5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h4
            className="inline-flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--text-secondary)" }}
          >
            <Activity className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
            <span>Nhật ký API (Audit Log · {auditLogs.length})</span>
          </h4>

          <div className="flex items-center gap-1.5">
            <button
              type="button"
              disabled={refreshingAudit}
              onClick={() => void handleRefreshAudit()}
              className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-xs font-medium disabled:opacity-50"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${refreshingAudit ? "animate-spin" : ""}`} />
              <span>Làm mới</span>
            </button>

            {auditLogs.length > 0 &&
              (confirmClearAudit ? (
                <div className="flex items-center gap-1">
                  <button
                    type="button"
                    disabled={clearingAudit}
                    onClick={() => void handleClearAudit()}
                    className="rounded-lg bg-rose-600 px-2.5 py-1 text-xs font-semibold text-white hover:bg-rose-500 disabled:opacity-50"
                  >
                    {clearingAudit ? "Đang xóa..." : "Xác nhận xóa"}
                  </button>
                  <button
                    type="button"
                    onClick={() => setConfirmClearAudit(false)}
                    className="rounded-lg px-2 py-1 text-xs"
                    style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                  >
                    Hủy
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  onClick={() => setConfirmClearAudit(true)}
                  className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-xs font-medium"
                  style={{
                    background: "var(--danger-bg)",
                    color: "var(--danger-text)",
                  }}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                  <span>Xóa nhật ký</span>
                </button>
              ))}
          </div>
        </div>

        <div
          className="overflow-hidden rounded-xl"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          {auditLogs.length === 0 ? (
            <div
              className="py-6 text-center text-xs"
              style={{ color: "var(--text-secondary)" }}
            >
              Chưa có yêu cầu API nào được ghi nhận.
            </div>
          ) : (
            <div className="max-h-72 overflow-auto">
              <table className="w-full border-collapse text-left text-xs">
                <thead>
                  <tr
                    className="text-[11px] uppercase tracking-wider"
                    style={{
                      background: "var(--card-bg)",
                      borderBottom: "1px solid var(--border-subtle)",
                      color: "var(--text-secondary)",
                    }}
                  >
                    <th className="px-3 py-2 font-semibold">Thời gian</th>
                    <th className="px-3 py-2 font-semibold">Ứng dụng</th>
                    <th className="px-3 py-2 font-semibold">Endpoint</th>
                    <th className="px-3 py-2 font-semibold">Đích (ID)</th>
                    <th className="px-3 py-2 font-semibold">HTTP</th>
                    <th className="px-3 py-2 font-semibold">Kết quả</th>
                  </tr>
                </thead>
                <tbody>
                  {auditLogs.map((log) => (
                    <tr
                      key={log.id}
                      style={{ borderBottom: "1px solid var(--border-subtle)" }}
                    >
                      <td
                        className="whitespace-nowrap px-3 py-2 font-mono text-[11px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {formatLocalDateTime(log.created_at, true)}
                      </td>
                      <td className="max-w-[140px] truncate px-3 py-2 font-medium">
                        {log.client_name || log.client_id || "—"}
                      </td>
                      <td className="whitespace-nowrap px-3 py-2 font-mono text-[11px]">
                        <span className="font-semibold" style={{ color: "var(--accent)" }}>
                          {log.method}
                        </span>{" "}
                        <span>{log.path}</span>
                      </td>
                      <td
                        className="max-w-[120px] truncate px-3 py-2 font-mono text-[11px]"
                        style={{ color: "var(--text-secondary)" }}
                        title={log.target_thread_id || undefined}
                      >
                        {log.target_thread_id || "—"}
                      </td>
                      <td className="whitespace-nowrap px-3 py-2">
                        <span
                          className="rounded-md px-1.5 py-0.5 font-mono text-[11px] font-semibold"
                          style={toneStyle(getAuditStatusTone(log.status_code))}
                        >
                          {log.status_code}
                        </span>
                      </td>
                      <td
                        className="max-w-[200px] px-3 py-2 text-[11.5px]"
                        title={log.detail || undefined}
                      >
                        <div className="truncate font-medium">
                          {formatAuditOutcome(log.status_code, log.outcome)}
                        </div>
                        {log.detail && (
                          <div
                            className="truncate text-[10.5px]"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            {log.detail}
                          </div>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

export default LocalApiPanel;
