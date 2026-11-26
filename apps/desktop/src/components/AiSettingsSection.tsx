import { useEffect, useState } from "react";
import {
  AlertTriangle,
  Bot,
  CheckCircle2,
  Eye,
  EyeOff,
  KeyRound,
  Loader2,
  RotateCcw,
  Save,
  ShieldAlert,
  Sparkles,
  Terminal,
} from "lucide-react";
import {
  AI_GEMINI_MODELS,
  AI_SUMMARY_LANGUAGES,
  DEFAULT_AI_SYSTEM_PROMPT,
  clampSummaryMessageCount,
  type AiBackendKind,
  type AiCliProbeDto,
  type AiConfigDto,
  type AiConfigUpdateDto,
} from "../utils/automationHelpers";

export interface AiSettingsSectionProps {
  config: AiConfigDto | null;
  loading?: boolean;
  translateHasGeminiKey?: boolean;
  translateGeminiPreview?: string;
  onSave: (update: AiConfigUpdateDto) => Promise<AiConfigDto | void>;
  onProbeCli: (cliPath: string) => Promise<AiCliProbeDto>;
  onOpenAiChat?: () => void;
  onOpenTranslateSettings?: () => void;
}

const KNOWN_MODEL_IDS = new Set<string>(AI_GEMINI_MODELS.map((m) => m.id));

export function AiSettingsSection({
  config,
  loading = false,
  translateHasGeminiKey = false,
  translateGeminiPreview = "",
  onSave,
  onProbeCli,
  onOpenAiChat,
  onOpenTranslateSettings,
}: AiSettingsSectionProps) {
  const [enabled, setEnabled] = useState(false);
  const [backend, setBackend] = useState<AiBackendKind>("gemini_api");
  const [geminiModel, setGeminiModel] = useState("gemini-2.5-flash");
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);
  const [clearApiKey, setClearApiKey] = useState(false);
  const [cliPath, setCliPath] = useState("gemini");
  const [cliModel, setCliModel] = useState("");
  const [systemPrompt, setSystemPrompt] = useState(DEFAULT_AI_SYSTEM_PROMPT);
  const [summaryLanguage, setSummaryLanguage] = useState("vi");
  const [maxSummaryMessages, setMaxSummaryMessages] = useState(40);

  const [saving, setSaving] = useState(false);
  const [probingCli, setProbingCli] = useState(false);
  const [cliProbe, setCliProbe] = useState<AiCliProbeDto | null>(null);
  const [feedback, setFeedback] = useState<{ tone: "ok" | "error"; text: string } | null>(null);

  useEffect(() => {
    if (!config) return;
    setEnabled(config.enabled);
    setBackend(config.backend === "gemini_cli" ? "gemini_cli" : "gemini_api");
    setGeminiModel(config.gemini_model || "gemini-2.5-flash");
    setCliPath(config.cli_path || "gemini");
    setCliModel(config.cli_model || "");
    setSystemPrompt(config.system_prompt || DEFAULT_AI_SYSTEM_PROMPT);
    setSummaryLanguage(config.summary_language || "vi");
    setMaxSummaryMessages(clampSummaryMessageCount(config.max_summary_messages));
    setApiKeyInput("");
    setClearApiKey(false);
  }, [config]);

  const hasDedicatedKey = Boolean(config?.has_api_key) && !clearApiKey;
  const hasTranslateKey = Boolean(config?.has_translate_fallback_key || translateHasGeminiKey);
  const fallbackPreview = config?.translate_fallback_preview || translateGeminiPreview || "";
  const isCustomModel = !KNOWN_MODEL_IDS.has(geminiModel.trim());

  const handleProbeCli = async () => {
    if (probingCli) return;
    setProbingCli(true);
    setFeedback(null);
    try {
      const target = cliPath.trim() || "gemini";
      const result = await onProbeCli(target);
      setCliProbe(result);
    } catch (err) {
      setCliProbe({
        available: false,
        resolved_path: null,
        version: null,
        error: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setProbingCli(false);
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (saving) return;

    const cleanModel = geminiModel.trim() || "gemini-2.5-flash";
    if (!/^[A-Za-z0-9._-]{1,64}$/.test(cleanModel)) {
      setFeedback({
        tone: "error",
        text: "Tên model Gemini chỉ được chứa chữ cái, số và các ký tự . _ - (tối đa 64 ký tự).",
      });
      return;
    }

    const cleanCliModel = cliModel.trim();
    if (cleanCliModel && !/^[A-Za-z0-9._-]{1,64}$/.test(cleanCliModel)) {
      setFeedback({
        tone: "error",
        text: "Tên model CLI không hợp lệ (chỉ cho phép chữ cái, số, ., _, -).",
      });
      return;
    }

    setSaving(true);
    setFeedback(null);
    try {
      const payload: AiConfigUpdateDto = {
        enabled,
        backend,
        gemini_model: cleanModel,
        cli_path: cliPath.trim() || "gemini",
        cli_model: cleanCliModel,
        system_prompt: systemPrompt.trim() || DEFAULT_AI_SYSTEM_PROMPT,
        summary_language: summaryLanguage || "vi",
        max_summary_messages: clampSummaryMessageCount(maxSummaryMessages),
      };
      if (apiKeyInput.trim()) {
        payload.api_key = apiKeyInput.trim();
        payload.clear_api_key = false;
      } else if (clearApiKey) {
        payload.clear_api_key = true;
      }

      await onSave(payload);
      setApiKeyInput("");
      setClearApiKey(false);
      setFeedback({
        tone: "ok",
        text: enabled
          ? "Đã lưu cài đặt Trợ lý AI thành công."
          : "Đã lưu cài đặt (Trợ lý AI hiện đang tắt).",
      });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setSaving(false);
    }
  };

  if (loading && !config) {
    return (
      <div
        className="flex items-center justify-center gap-2 rounded-xl p-6 text-xs"
        style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
      >
        <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
        <span>Đang tải cấu hình Trợ lý AI...</span>
      </div>
    );
  }

  return (
    <form onSubmit={(e) => void handleSubmit(e)} className="space-y-5">
      {/* Master Opt-in Card */}
      <section className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <h4
            className="text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--text-secondary)" }}
          >
            Trợ lý AI tùy chọn
          </h4>
          {enabled && onOpenAiChat && (
            <button
              type="button"
              onClick={onOpenAiChat}
              className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-semibold"
              style={{
                border: "1px solid var(--border-subtle)",
                color: "var(--accent)",
              }}
            >
              <Sparkles className="h-3.5 w-3.5" />
              <span>Mở Trợ lý AI</span>
            </button>
          )}
        </div>

        <div
          className="rounded-xl p-3.5"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <label className="flex cursor-pointer items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-2.5">
              <span
                className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-xl"
                style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
              >
                <Sparkles className="h-4 w-4" />
              </span>
              <div className="min-w-0">
                <div className="text-[14px] font-semibold">Bật Trợ lý AI tùy chọn</div>
                <p className="mt-0.5 text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  Tắt mặc định để đảm bảo quyền riêng tư. Khi bật, bạn có thể trò chuyện trực tiếp
                  với Trợ lý AI trong Teza và tóm tắt nhanh tin nhắn chưa đọc.
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
        </div>
      </section>

      {/* Backend Selector Cards */}
      <section className="space-y-2.5">
        <h4
          className="text-xs font-semibold uppercase tracking-wider"
          style={{ color: "var(--text-secondary)" }}
        >
          Nguồn xử lý AI (Backend)
        </h4>

        <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
          {/* Card 1: Gemini API */}
          <button
            type="button"
            onClick={() => setBackend("gemini_api")}
            className="flex flex-col items-start gap-1.5 rounded-xl p-3.5 text-left transition"
            style={{
              background: backend === "gemini_api" ? "var(--accent-soft)" : "var(--bg-surface)",
              border: `1px solid ${
                backend === "gemini_api" ? "var(--accent)" : "var(--border-subtle)"
              }`,
            }}
          >
            <div className="flex w-full items-center justify-between gap-2">
              <span className="inline-flex items-center gap-1.5 text-[13.5px] font-semibold">
                <Bot className="h-4 w-4" style={{ color: "var(--accent)" }} />
                <span>Gemini API trực tiếp</span>
              </span>
              <span
                className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                style={{
                  background: "var(--chip-bg)",
                  color: "var(--accent)",
                  border: "1px solid var(--chip-border)",
                }}
              >
                Khuyên dùng
              </span>
            </div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              Tái sử dụng Gemini API key từ mục Dịch hoặc nhập khóa riêng. Stream phản hồi nhanh, ổn
              định, không cần cài đặt thêm.
            </p>
          </button>

          {/* Card 2: Gemini CLI */}
          <button
            type="button"
            onClick={() => setBackend("gemini_cli")}
            className="flex flex-col items-start gap-1.5 rounded-xl p-3.5 text-left transition"
            style={{
              background: backend === "gemini_cli" ? "var(--accent-soft)" : "var(--bg-surface)",
              border: `1px solid ${
                backend === "gemini_cli" ? "var(--accent)" : "var(--border-subtle)"
              }`,
            }}
          >
            <div className="flex w-full items-center justify-between gap-2">
              <span className="inline-flex items-center gap-1.5 text-[13.5px] font-semibold">
                <Terminal className="h-4 w-4" style={{ color: "var(--accent)" }} />
                <span>Gemini CLI trên máy</span>
              </span>
              <span
                className="rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                style={{
                  background: "var(--chip-bg)",
                  color: "var(--text-secondary)",
                  border: "1px solid var(--chip-border)",
                }}
              >
                Nâng cao
              </span>
            </div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              Gọi trực tiếp lệnh <code className="font-mono">gemini</code> bạn đã cài sẵn trên máy
              theo cơ chế cô lập an toàn, khóa tool hệ thống.
            </p>
          </button>
        </div>

        {/* Backend-Specific Configuration */}
        {backend === "gemini_api" ? (
          <div
            className="space-y-3.5 rounded-xl p-4"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            {/* Model selector */}
            <div className="space-y-2">
              <label
                className="block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Model Gemini
              </label>
              <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
                {AI_GEMINI_MODELS.map((m) => {
                  const active = geminiModel.trim() === m.id;
                  return (
                    <button
                      key={m.id}
                      type="button"
                      onClick={() => setGeminiModel(m.id)}
                      className="rounded-xl px-3 py-2 text-left transition"
                      style={{
                        background: active ? "var(--bg-active)" : "var(--card-bg)",
                        color: active ? "var(--text-on-active)" : "var(--text-primary)",
                        border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                      }}
                    >
                      <div className="text-xs font-semibold">{m.label}</div>
                      <div
                        className="mt-0.5 text-[11px]"
                        style={{
                          color: active ? "var(--text-secondary-on-active)" : "var(--text-secondary)",
                        }}
                      >
                        {m.hint}
                      </div>
                    </button>
                  );
                })}
              </div>

              <div className="flex items-center gap-2 pt-0.5">
                <span className="shrink-0 text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                  Mã model:
                </span>
                <input
                  value={geminiModel}
                  onChange={(e) => setGeminiModel(e.target.value)}
                  placeholder="gemini-2.5-flash"
                  className="flex-1 rounded-lg px-2.5 py-1.5 font-mono text-xs outline-none"
                  style={{
                    background: "var(--bg-stage)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
                {isCustomModel && (
                  <span
                    className="shrink-0 rounded-md px-2 py-0.5 text-[10.5px] font-medium"
                    style={{ background: "var(--chip-bg)", color: "var(--accent)" }}
                  >
                    Tùy chỉnh
                  </span>
                )}
              </div>
            </div>

            {/* API key configuration */}
            <div
              className="space-y-2 pt-2"
              style={{ borderTop: "1px solid var(--border-subtle)" }}
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <label
                  className="inline-flex items-center gap-1.5 text-xs font-medium"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <KeyRound className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
                  <span>Gemini API Key (tùy chọn ghi đè)</span>
                </label>

                {hasDedicatedKey ? (
                  <span className="inline-flex items-center gap-1 text-[11.5px] font-medium text-emerald-400">
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    <span>Khóa riêng: {config?.api_key_preview || "Đã lưu"}</span>
                  </span>
                ) : hasTranslateKey ? (
                  <span className="inline-flex items-center gap-1 text-[11.5px] font-medium text-emerald-400">
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    <span>
                      Dùng chung từ mục Dịch{fallbackPreview ? ` (${fallbackPreview})` : ""}
                    </span>
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1 text-[11.5px] text-amber-400">
                    <AlertTriangle className="h-3.5 w-3.5" />
                    <span>Chưa có API key</span>
                  </span>
                )}
              </div>

              <div className="flex items-center gap-2">
                <div className="relative flex-1">
                  <input
                    type={showApiKey ? "text" : "password"}
                    value={apiKeyInput}
                    onChange={(e) => {
                      setApiKeyInput(e.target.value);
                      if (e.target.value.trim()) setClearApiKey(false);
                    }}
                    placeholder={
                      hasDedicatedKey
                        ? "Nhập khóa mới để thay thế (để trống để giữ khóa hiện tại)"
                        : hasTranslateKey
                          ? "Để trống để dùng chung khóa Gemini từ mục Dịch, hoặc nhập khóa riêng"
                          : "AIza..."
                    }
                    className="w-full rounded-xl py-2 pr-9 pl-3 font-mono text-xs outline-none"
                    style={{
                      background: "var(--bg-stage)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  />
                  <button
                    type="button"
                    onClick={() => setShowApiKey((v) => !v)}
                    className="hover-menu absolute top-1/2 right-2 -translate-y-1/2 rounded-md p-1"
                    style={{ color: "var(--text-secondary)" }}
                    title={showApiKey ? "Ẩn API key" : "Hiện API key"}
                  >
                    {showApiKey ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                  </button>
                </div>
              </div>

              <div className="flex flex-wrap items-center justify-between gap-2 text-[11.5px]">
                <span style={{ color: "var(--text-secondary)" }}>
                  API key được mã hóa bảo vệ bằng DPAPI cục bộ, không bao giờ lộ ra log.
                </span>
                <div className="flex items-center gap-3">
                  {config?.has_api_key && (
                    <label className="inline-flex cursor-pointer items-center gap-1.5 text-amber-400">
                      <input
                        type="checkbox"
                        checked={clearApiKey}
                        onChange={(e) => {
                          setClearApiKey(e.target.checked);
                          if (e.target.checked) setApiKeyInput("");
                        }}
                        className="h-3.5 w-3.5 accent-[var(--accent)]"
                      />
                      <span>Xóa khóa riêng</span>
                    </label>
                  )}
                  {onOpenTranslateSettings && (
                    <button
                      type="button"
                      onClick={onOpenTranslateSettings}
                      className="font-medium underline hover:opacity-85"
                      style={{ color: "var(--accent)" }}
                    >
                      Mở cài đặt Dịch
                    </button>
                  )}
                </div>
              </div>
            </div>
          </div>
        ) : (
          <div
            className="space-y-3.5 rounded-xl p-4"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            {/* Security warning about disabled tools & prompt injection */}
            <div
              className="flex items-start gap-2.5 rounded-xl p-3 text-xs leading-relaxed"
              style={{
                background: "var(--card-bg)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-secondary)",
              }}
            >
              <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-amber-400" />
              <div>
                <div className="font-semibold" style={{ color: "var(--text-primary)" }}>
                  Đã tự động khóa tool hệ thống để chống Prompt Injection
                </div>
                <p className="mt-0.5">
                  Khi chạy Gemini CLI, Teza luôn truyền cờ vô hiệu hóa toàn bộ công cụ thực thi lệnh
                  và tệp cục bộ (<code className="font-mono">--allowed-tools &quot;&quot;</code>),
                  tuyệt đối không bật <code className="font-mono">--yolo</code>, và bọc dữ liệu
                  chat trong khung cách ly khi tóm tắt tin nhắn.
                </p>
              </div>
            </div>

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div>
                <label
                  className="mb-1 block text-xs font-medium"
                  style={{ color: "var(--text-secondary)" }}
                >
                  Đường dẫn Gemini CLI
                </label>
                <input
                  value={cliPath}
                  onChange={(e) => setCliPath(e.target.value)}
                  placeholder="gemini"
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
                  Model CLI (tùy chọn)
                </label>
                <input
                  value={cliModel}
                  onChange={(e) => setCliModel(e.target.value)}
                  placeholder="Để trống để dùng mặc định của CLI"
                  className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                  style={{
                    background: "var(--bg-stage)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-2.5">
              <button
                type="button"
                disabled={probingCli}
                onClick={() => void handleProbeCli()}
                className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-xs font-semibold transition disabled:opacity-50"
                style={{
                  background: "var(--card-bg)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--accent)",
                }}
              >
                {probingCli ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Terminal className="h-3.5 w-3.5" />
                )}
                <span>Kiểm tra Gemini CLI</span>
              </button>

              {cliProbe && (
                <div
                  className="flex min-w-0 flex-1 items-start gap-2 rounded-xl px-3 py-2 text-xs"
                  style={{
                    background: cliProbe.available
                      ? "rgba(34, 197, 94, 0.12)"
                      : "var(--danger-bg)",
                    color: cliProbe.available ? "#4ade80" : "var(--danger-text)",
                  }}
                >
                  {cliProbe.available ? (
                    <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />
                  ) : (
                    <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                  )}
                  <div className="min-w-0">
                    {cliProbe.available ? (
                      <>
                        <div className="font-semibold">
                          Đã sẵn sàng{cliProbe.version ? ` · v${cliProbe.version}` : ""}
                        </div>
                        {cliProbe.resolved_path && (
                          <div className="truncate font-mono text-[11px] opacity-85">
                            {cliProbe.resolved_path}
                          </div>
                        )}
                      </>
                    ) : (
                      <div className="wrap-anywhere">
                        {cliProbe.error ||
                          "Không tìm thấy Gemini CLI. Hãy kiểm tra PATH hoặc nhập đường dẫn đầy đủ."}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </section>

      {/* System Prompt & Unread Summary Settings */}
      <section className="space-y-2.5">
        <h4
          className="text-xs font-semibold uppercase tracking-wider"
          style={{ color: "var(--text-secondary)" }}
        >
          Chỉ dẫn hệ thống & Tóm tắt tin chưa đọc
        </h4>

        <div
          className="space-y-4 rounded-xl p-4"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-2">
            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Ngôn ngữ tóm tắt
              </label>
              <select
                value={summaryLanguage}
                onChange={(e) => setSummaryLanguage(e.target.value)}
                className="w-full rounded-xl px-3 py-2 text-xs outline-none"
                style={{
                  background: "var(--bg-stage)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {AI_SUMMARY_LANGUAGES.map((lang) => (
                  <option key={lang.code} value={lang.code}>
                    {lang.label}
                  </option>
                ))}
              </select>
            </div>

            <div>
              <div className="mb-1 flex items-center justify-between gap-2">
                <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  Số tin chưa đọc tối đa để tóm tắt
                </label>
                <span
                  className="rounded-md px-2 py-0.5 font-mono text-[11px] font-semibold"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  {maxSummaryMessages} tin
                </span>
              </div>
              <div className="flex items-center gap-2.5">
                <input
                  type="range"
                  min={5}
                  max={120}
                  step={5}
                  value={maxSummaryMessages}
                  onChange={(e) =>
                    setMaxSummaryMessages(clampSummaryMessageCount(Number(e.target.value)))
                  }
                  className="flex-1 cursor-pointer accent-[var(--accent)]"
                />
                <input
                  type="number"
                  min={5}
                  max={120}
                  value={maxSummaryMessages}
                  onChange={(e) =>
                    setMaxSummaryMessages(clampSummaryMessageCount(Number(e.target.value)))
                  }
                  className="w-16 rounded-lg px-2 py-1 text-center font-mono text-xs outline-none"
                  style={{
                    background: "var(--bg-stage)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
              </div>
            </div>
          </div>

          <div>
            <div className="mb-1 flex items-center justify-between gap-2">
              <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                System Prompt (Chỉ dẫn vai trò cho Trợ lý AI)
              </label>
              <button
                type="button"
                onClick={() => setSystemPrompt(DEFAULT_AI_SYSTEM_PROMPT)}
                className="hover-menu inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[11px] font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                <RotateCcw className="h-3 w-3" />
                <span>Mặc định</span>
              </button>
            </div>
            <textarea
              rows={3}
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              placeholder={DEFAULT_AI_SYSTEM_PROMPT}
              className="w-full resize-y rounded-xl px-3 py-2 text-xs leading-relaxed outline-none"
              style={{
                background: "var(--bg-stage)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            />
          </div>
        </div>
      </section>

      {/* Feedback & Save */}
      {feedback && (
        <div
          className="flex items-center gap-2 rounded-xl px-3.5 py-2.5 text-xs font-medium"
          style={{
            background:
              feedback.tone === "ok" ? "rgba(34, 197, 94, 0.12)" : "var(--danger-bg)",
            color: feedback.tone === "ok" ? "#4ade80" : "var(--danger-text)",
          }}
        >
          {feedback.tone === "ok" ? (
            <CheckCircle2 className="h-4 w-4 shrink-0" />
          ) : (
            <AlertTriangle className="h-4 w-4 shrink-0" />
          )}
          <span className="wrap-anywhere">{feedback.text}</span>
        </div>
      )}

      <div className="flex items-center justify-end gap-2">
        <button
          type="submit"
          disabled={saving}
          className="tf-btn inline-flex items-center gap-2 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
        >
          {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
          <span>Lưu cài đặt AI</span>
        </button>
      </div>
    </form>
  );
}

export default AiSettingsSection;
