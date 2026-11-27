import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Cpu,
  Download,
  Eye,
  EyeOff,
  Globe,
  KeyRound,
  Loader2,
  RefreshCw,
  RotateCcw,
  Save,
  ShieldAlert,
  Sparkles,
} from "lucide-react";
import {
  AI_CLOUD_PROVIDERS,
  AI_SUMMARY_LANGUAGES,
  DEFAULT_AI_SYSTEM_PROMPT,
  DEFAULT_CLOUD_API_BASE_URL,
  DEFAULT_CLOUD_API_MODEL,
  DEFAULT_ZEROCLAW_MODEL,
  DEFAULT_ZEROCLAW_PATH,
  clampSummaryMessageCount,
  detectAiCloudProvider,
  formatInstallSizeMb,
  getAiCloudProviderPreset,
  isPublicCloudModelsProvider,
  isSameCloudApiHost,
  isValidAiModelName,
  isValidHttpsApiBaseUrl,
  type AiBackendKind,
  type AiCliProbeDto,
  type AiCloudProviderPreset,
  type AiConfigDto,
  type AiConfigUpdateDto,
  type AiInstallZeroClawResponseDto,
  type AiModelInfoDto,
  type ZeroClawInstallProgressDto,
} from "../utils/automationHelpers";

export interface AiSettingsSectionProps {
  config: AiConfigDto | null;
  loading?: boolean;
  relayEnabled?: boolean;
  onSave: (update: AiConfigUpdateDto) => Promise<AiConfigDto | void>;
  onProbeCli: (cliPath: string, gatewayUrl: string) => Promise<AiCliProbeDto>;
  onInstallZeroClaw: () => Promise<AiInstallZeroClawResponseDto>;
  onListModels?: (baseUrl?: string, apiKey?: string) => Promise<AiModelInfoDto[]>;
  onSubscribeInstallProgress?: (
    handler: (ev: ZeroClawInstallProgressDto) => void,
  ) => Promise<() => void>;
  onOpenAiChat?: () => void;
}

function describeInstallProgress(progress: ZeroClawInstallProgressDto | null): string {
  if (!progress) return "Đang chuẩn bị cài đặt ZeroClaw...";
  switch (progress.stage) {
    case "checking":
      return "Đang tìm phiên bản ZeroClaw mới nhất...";
    case "downloading": {
      const done = formatInstallSizeMb(progress.downloaded_bytes);
      if (progress.total_bytes && progress.total_bytes > 0) {
        const total = formatInstallSizeMb(progress.total_bytes);
        const pct = Math.min(100, Math.round((progress.downloaded_bytes / progress.total_bytes) * 100));
        return `Đang tải ZeroClaw ${progress.tag ? `(${progress.tag}) ` : ""}— ${done} / ${total} (${pct}%)`;
      }
      return `Đang tải ZeroClaw... (${done})`;
    }
    case "verifying":
      return "Đang kiểm tra độ an toàn của tệp tải về...";
    case "extracting":
      return "Đang cài đặt ZeroClaw vào máy...";
    case "ready":
      return "Cài đặt hoàn tất!";
    default:
      return "Đang cài đặt ZeroClaw...";
  }
}

export function AiSettingsSection({
  config,
  loading = false,
  relayEnabled = false,
  onSave,
  onProbeCli,
  onInstallZeroClaw,
  onListModels,
  onSubscribeInstallProgress,
  onOpenAiChat,
}: AiSettingsSectionProps) {
  const [enabled, setEnabled] = useState(false);
  const [backend, setBackend] = useState<AiBackendKind>("zeroclaw");

  // Cloud API state (Groq / OpenRouter / xAI Grok / custom HTTPS)
  const [apiBaseUrl, setApiBaseUrl] = useState(DEFAULT_CLOUD_API_BASE_URL);
  const [apiKeyInput, setApiKeyInput] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);
  const [clearApiKey, setClearApiKey] = useState(false);
  const [cloudModel, setCloudModel] = useState(DEFAULT_CLOUD_API_MODEL);
  const [remoteModels, setRemoteModels] = useState<AiModelInfoDto[]>([]);
  const [modelFilter, setModelFilter] = useState("");
  const [showAllRemoteModels, setShowAllRemoteModels] = useState(false);
  const [loadingModels, setLoadingModels] = useState(false);
  const [modelsFeedback, setModelsFeedback] = useState<{
    tone: "ok" | "error";
    text: string;
  } | null>(null);

  // ZeroClaw local state
  const [zeroclawPath, setZeroclawPath] = useState(DEFAULT_ZEROCLAW_PATH);
  const [zeroclawGatewayUrl, setZeroclawGatewayUrl] = useState("");
  const [zeroclawTokenInput, setZeroclawTokenInput] = useState("");
  const [showZeroclawToken, setShowZeroclawToken] = useState(false);
  const [clearZeroclawToken, setClearZeroclawToken] = useState(false);
  const [zeroclawSafeTools, setZeroclawSafeTools] = useState(false);

  // Shared preferences
  const [systemPrompt, setSystemPrompt] = useState(DEFAULT_AI_SYSTEM_PROMPT);
  const [summaryLanguage, setSummaryLanguage] = useState("vi");
  const [maxSummaryMessages, setMaxSummaryMessages] = useState(40);

  const [showAdvanced, setShowAdvanced] = useState(false);
  const [saving, setSaving] = useState(false);
  const [probingCli, setProbingCli] = useState(false);
  const [cliProbe, setCliProbe] = useState<AiCliProbeDto | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installProgress, setInstallProgress] = useState<ZeroClawInstallProgressDto | null>(null);
  const [feedback, setFeedback] = useState<{ tone: "ok" | "warn" | "error"; text: string } | null>(
    null,
  );
  const modelsReqSeqRef = useRef(0);
  const hydratedConfigRef = useRef(false);
  const shouldHydrateNextConfigRef = useRef(true);
  const apiKeyInputRef = useRef(apiKeyInput);
  apiKeyInputRef.current = apiKeyInput;
  const zeroclawGatewayUrlRef = useRef(zeroclawGatewayUrl);
  zeroclawGatewayUrlRef.current = zeroclawGatewayUrl;
  const hasSavedKeyForCurrentHostRef = useRef(false);

  useEffect(() => {
    if (!config) return;
    if (hydratedConfigRef.current && !shouldHydrateNextConfigRef.current) {
      // Preserve unsaved form edits across non-save config refreshes (e.g. 1-click ZeroClaw install).
      setZeroclawPath(config.zeroclaw_path || DEFAULT_ZEROCLAW_PATH);
      return;
    }
    hydratedConfigRef.current = true;
    shouldHydrateNextConfigRef.current = false;
    setEnabled(config.enabled);
    setBackend(config.backend === "cloud_api" ? "cloud_api" : "zeroclaw");

    const baseUrl = config.api_base_url || DEFAULT_CLOUD_API_BASE_URL;
    setApiBaseUrl(baseUrl);
    const matchedPreset = getAiCloudProviderPreset(detectAiCloudProvider(baseUrl));
    const hasConcreteModel = Boolean(config.model && config.model !== DEFAULT_ZEROCLAW_MODEL);
    setCloudModel(
      hasConcreteModel
        ? config.model
        : config.backend === "zeroclaw" && config.zeroclaw_gateway_url
          ? DEFAULT_ZEROCLAW_MODEL
          : matchedPreset?.defaultModel || DEFAULT_CLOUD_API_MODEL,
    );
    setApiKeyInput("");
    setClearApiKey(false);

    setZeroclawPath(config.zeroclaw_path || DEFAULT_ZEROCLAW_PATH);
    setZeroclawGatewayUrl(config.zeroclaw_gateway_url || "");
    setZeroclawTokenInput("");
    setClearZeroclawToken(false);
    setZeroclawSafeTools(Boolean(config.zeroclaw_safe_tools));

    setSystemPrompt(config.system_prompt || DEFAULT_AI_SYSTEM_PROMPT);
    setSummaryLanguage(config.summary_language || "vi");
    setMaxSummaryMessages(clampSummaryMessageCount(config.max_summary_messages));
  }, [config]);

  const runProbe = useCallback(
    async (cliPath: string, gwUrl: string) => {
      setProbingCli(true);
      try {
        const result = await onProbeCli(cliPath.trim() || DEFAULT_ZEROCLAW_PATH, gwUrl.trim());
        setCliProbe(result);
      } catch (err) {
        setCliProbe({
          available: false,
          resolved_path: null,
          version: null,
          mode: gwUrl.trim() ? "gateway" : "cli",
          error: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setProbingCli(false);
      }
    },
    [onProbeCli],
  );

  // Auto-check ZeroClaw status when ZeroClaw backend is active.
  useEffect(() => {
    if (!config || backend !== "zeroclaw") return;
    void runProbe(config.zeroclaw_path || DEFAULT_ZEROCLAW_PATH, config.zeroclaw_gateway_url || "");
  }, [backend, config?.zeroclaw_path, config?.zeroclaw_gateway_url, runProbe]);

  useEffect(() => {
    if (!onSubscribeInstallProgress) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    void onSubscribeInstallProgress((ev) => {
      if (active) setInstallProgress(ev);
    }).then((fn) => {
      if (active) unlisten = fn;
      else fn();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, [onSubscribeInstallProgress]);

  const activeProviderId = detectAiCloudProvider(apiBaseUrl);
  const activeProviderPreset = getAiCloudProviderPreset(activeProviderId);
  const hasApiKey = Boolean(config?.has_api_key) && !clearApiKey;
  const hasSavedKeyForCurrentHost =
    hasApiKey && isSameCloudApiHost(apiBaseUrl, config?.api_base_url || DEFAULT_CLOUD_API_BASE_URL);
  hasSavedKeyForCurrentHostRef.current = hasSavedKeyForCurrentHost;
  const isPublicModelsHost = isPublicCloudModelsProvider(apiBaseUrl);
  const hasZeroclawToken = Boolean(config?.has_zeroclaw_token) && !clearZeroclawToken;

  const fetchLiveModels = useCallback(
    async (targetUrl: string, keyOverride: string | undefined, isAuto: boolean) => {
      if (!onListModels) return;
      const seq = ++modelsReqSeqRef.current;
      setLoadingModels(true);
      if (!isAuto) setModelsFeedback(null);
      try {
        const list = await onListModels(targetUrl, keyOverride);
        if (seq !== modelsReqSeqRef.current) return;
        setRemoteModels(list);
        setModelFilter("");
        setShowAllRemoteModels(false);
        if (list.length === 0) {
          if (!isAuto) {
            setModelsFeedback({
              tone: "error",
              text: "Không nhận được mẫu AI nào từ nhà cung cấp này. Bạn vẫn có thể chọn từ danh sách gợi ý.",
            });
          }
        } else {
          const freeCount = list.filter((m) => m.is_free).length;
          setModelsFeedback({
            tone: "ok",
            text:
              freeCount > 0
                ? `Đã quét ${list.length} mẫu AI (${freeCount} mẫu miễn phí ở trên cùng). Bấm để chọn.`
                : `Đã tải ${list.length} mẫu AI mới nhất. Bấm vào một mẫu bên dưới để chọn.`,
          });
        }
      } catch (err) {
        if (seq !== modelsReqSeqRef.current) return;
        if (!isAuto) {
          setModelsFeedback({
            tone: "error",
            text: err instanceof Error ? err.message : String(err),
          });
        }
      } finally {
        if (seq === modelsReqSeqRef.current) {
          setLoadingModels(false);
        }
      }
    },
    [onListModels],
  );

  // Auto-scan live models in BOTH Cloud API and ZeroClaw modes when OpenRouter (keyless) is
  // selected or when a key is saved / typed for Groq or xAI Grok (debounced 450ms while typing).
  useEffect(() => {
    if (!onListModels) return;
    const targetUrl = apiBaseUrl.trim() || DEFAULT_CLOUD_API_BASE_URL;
    if (!isValidHttpsApiBaseUrl(targetUrl)) {
      modelsReqSeqRef.current += 1;
      setLoadingModels(false);
      return;
    }

    const typedKey = apiKeyInput.trim();
    const canScan =
      isPublicModelsHost || hasSavedKeyForCurrentHost || typedKey.length >= 8;
    if (!canScan) {
      modelsReqSeqRef.current += 1;
      setLoadingModels(false);
      return;
    }

    const delayMs = typedKey.length > 0 ? 450 : 120;
    const timer = window.setTimeout(() => {
      void fetchLiveModels(targetUrl, typedKey || undefined, true);
    }, delayMs);
    return () => window.clearTimeout(timer);
  }, [
    backend,
    apiBaseUrl,
    apiKeyInput,
    isPublicModelsHost,
    hasSavedKeyForCurrentHost,
    onListModels,
    fetchLiveModels,
  ]);

  const filteredRemoteModels = useMemo(() => {
    const q = modelFilter.trim().toLowerCase();
    if (!q) return remoteModels;
    return remoteModels.filter(
      (m) =>
        m.id.toLowerCase().includes(q) ||
        (m.owned_by && m.owned_by.toLowerCase().includes(q)) ||
        (q === "free" && m.is_free) ||
        (q === "miễn phí" && m.is_free),
    );
  }, [remoteModels, modelFilter]);

  const visibleRemoteModels = showAllRemoteModels
    ? filteredRemoteModels
    : filteredRemoteModels.slice(0, 16);

  const handleSelectCloudProvider = (preset: AiCloudProviderPreset) => {
    modelsReqSeqRef.current += 1;
    setLoadingModels(false);
    setApiBaseUrl(preset.baseUrl);
    setCloudModel(preset.defaultModel);
    setRemoteModels([]);
    setModelFilter("");
    setShowAllRemoteModels(false);
    setModelsFeedback(null);
    setFeedback(null);
  };

  const handleLoadLatestModels = async () => {
    if (!onListModels || loadingModels) return;
    const targetUrl = apiBaseUrl.trim() || DEFAULT_CLOUD_API_BASE_URL;
    if (!isValidHttpsApiBaseUrl(targetUrl)) {
      setModelsFeedback({
        tone: "error",
        text: "Địa chỉ máy chủ AI phải bắt đầu bằng https:// và đúng định dạng.",
      });
      return;
    }
    const typedKey = apiKeyInput.trim();
    if (!typedKey && !hasSavedKeyForCurrentHost && !isPublicModelsHost) {
      setModelsFeedback({
        tone: "error",
        text: "Hãy dán khóa API ở ô trên trước, sau đó bấm tải danh sách mẫu.",
      });
      return;
    }

    await fetchLiveModels(targetUrl, typedKey || undefined, false);
  };

  const handleInstallZeroClaw = async () => {
    if (installing) return;
    setInstalling(true);
    setFeedback(null);
    setInstallProgress({
      stage: "checking",
      downloaded_bytes: 0,
      total_bytes: null,
      tag: null,
    });
    try {
      const res = await onInstallZeroClaw();
      setZeroclawPath(
        res.install.installed_path || res.config.zeroclaw_path || DEFAULT_ZEROCLAW_PATH,
      );
      setBackend("zeroclaw");
      setCliProbe({
        available: true,
        resolved_path: res.install.installed_path,
        version: res.install.version || res.install.tag,
        mode: "cli",
        error: null,
      });
      const hasKeyReady =
        Boolean(apiKeyInputRef.current.trim()) ||
        hasSavedKeyForCurrentHostRef.current ||
        Boolean(zeroclawGatewayUrlRef.current.trim());
      setFeedback({
        tone: "ok",
        text: hasKeyReady
          ? `Đã tải và cài đặt ZeroClaw (${res.install.version || res.install.tag}) thành công! Bạn có thể bật Trợ lý AI và bấm Lưu cài đặt để dùng ngay.`
          : `Đã tải và cài đặt ZeroClaw (${res.install.version || res.install.tag}) thành công! Tiếp theo, hãy chọn dịch vụ AI, dán khóa API ở mục bên dưới rồi bấm Lưu cài đặt.`,
      });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setInstalling(false);
      setInstallProgress(null);
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (saving) return;

    const cleanUrl = apiBaseUrl.trim() || DEFAULT_CLOUD_API_BASE_URL;
    if (!isValidHttpsApiBaseUrl(cleanUrl)) {
      setFeedback({
        tone: "error",
        text: "Địa chỉ máy chủ AI phải bắt đầu bằng https:// và không chứa tham số thừa.",
      });
      return;
    }

    const trimmedApiKey = apiKeyInput.trim();
    const willHaveApiKey =
      Boolean(trimmedApiKey) || (hasSavedKeyForCurrentHost && !clearApiKey);

    if (backend === "cloud_api") {
      const cleanModel =
        cloudModel.trim() && cloudModel.trim() !== DEFAULT_ZEROCLAW_MODEL
          ? cloudModel.trim()
          : activeProviderPreset?.defaultModel || DEFAULT_CLOUD_API_MODEL;
      if (!isValidAiModelName(cleanModel)) {
        setFeedback({
          tone: "error",
          text: "Tên mẫu AI chứa ký tự không hợp lệ. Hãy chọn một mẫu có sẵn bên dưới.",
        });
        return;
      }
      if (enabled && !willHaveApiKey) {
        setFeedback({
          tone: "error",
          text: `Vui lòng dán khóa API${activeProviderPreset ? ` của ${activeProviderPreset.name}` : ""} trước khi bật Trợ lý AI.`,
        });
        return;
      }

      setSaving(true);
      setFeedback(null);
      try {
        const payload: AiConfigUpdateDto = {
          enabled,
          backend: "cloud_api",
          api_base_url: cleanUrl,
          model: cleanModel,
          system_prompt: systemPrompt.trim() || DEFAULT_AI_SYSTEM_PROMPT,
          summary_language: summaryLanguage || "vi",
          max_summary_messages: clampSummaryMessageCount(maxSummaryMessages),
        };
        if (trimmedApiKey) {
          payload.api_key = trimmedApiKey;
          payload.clear_api_key = false;
        } else if (clearApiKey) {
          payload.clear_api_key = true;
        }

        shouldHydrateNextConfigRef.current = true;
        await onSave(payload);
        setApiKeyInput("");
        setClearApiKey(false);
        setFeedback({
          tone: "ok",
          text: enabled
            ? "Đã lưu cài đặt. Trợ lý AI đã sẵn sàng sử dụng ngay!"
            : "Đã lưu cài đặt (Trợ lý AI hiện đang tắt).",
        });
      } catch (err) {
        shouldHydrateNextConfigRef.current = false;
        setFeedback({
          tone: "error",
          text: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setSaving(false);
      }
      return;
    }

    const usingGateway = Boolean(zeroclawGatewayUrl.trim());
    const cleanModel = usingGateway
      ? cloudModel.trim() || DEFAULT_ZEROCLAW_MODEL
      : cloudModel.trim() && cloudModel.trim() !== DEFAULT_ZEROCLAW_MODEL
        ? cloudModel.trim()
        : activeProviderPreset?.defaultModel || DEFAULT_CLOUD_API_MODEL;
    if (!isValidAiModelName(cleanModel)) {
      setFeedback({
        tone: "error",
        text: "Tên mẫu AI chứa ký tự không hợp lệ. Hãy chọn một mẫu có sẵn bên dưới.",
      });
      return;
    }

    setSaving(true);
    setFeedback(null);
    try {
      const payload: AiConfigUpdateDto = {
        enabled,
        backend: "zeroclaw",
        api_base_url: cleanUrl,
        model: cleanModel,
        zeroclaw_path: zeroclawPath.trim() || DEFAULT_ZEROCLAW_PATH,
        zeroclaw_gateway_url: zeroclawGatewayUrl.trim(),
        zeroclaw_safe_tools: zeroclawSafeTools,
        system_prompt: systemPrompt.trim() || DEFAULT_AI_SYSTEM_PROMPT,
        summary_language: summaryLanguage || "vi",
        max_summary_messages: clampSummaryMessageCount(maxSummaryMessages),
      };
      if (trimmedApiKey) {
        payload.api_key = trimmedApiKey;
        payload.clear_api_key = false;
      } else if (clearApiKey) {
        payload.clear_api_key = true;
      }
      if (zeroclawTokenInput.trim()) {
        payload.zeroclaw_token = zeroclawTokenInput.trim();
        payload.clear_zeroclaw_token = false;
      } else if (clearZeroclawToken) {
        payload.clear_zeroclaw_token = true;
      }

      shouldHydrateNextConfigRef.current = true;
      await onSave(payload);
      setApiKeyInput("");
      setClearApiKey(false);
      setZeroclawTokenInput("");
      setClearZeroclawToken(false);
      if (!enabled) {
        setFeedback({
          tone: "ok",
          text: "Đã lưu cài đặt (Trợ lý AI hiện đang tắt).",
        });
      } else if (!usingGateway && !willHaveApiKey) {
        setFeedback({
          tone: "warn",
          text: "Đã lưu cài đặt ZeroClaw. Lưu ý: bạn chưa dán khóa API — nếu chưa tự cấu hình sẵn ZeroClaw trên máy, hãy chọn dịch vụ AI và dán khóa API ở trên để ZeroClaw trả lời.",
        });
      } else {
        setFeedback({
          tone: "ok",
          text: "Đã lưu cài đặt. ZeroClaw đã liên kết xong và sẵn sàng trả lời ngay!",
        });
      }
    } catch (err) {
      shouldHydrateNextConfigRef.current = false;
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
        <span>Đang tải cài đặt Trợ lý AI...</span>
      </div>
    );
  }

  const downloadPct =
    installProgress?.stage === "downloading" &&
    installProgress.total_bytes &&
    installProgress.total_bytes > 0
      ? Math.min(100, Math.round((installProgress.downloaded_bytes / installProgress.total_bytes) * 100))
      : installProgress?.stage === "verifying"
        ? 92
        : installProgress?.stage === "extracting"
          ? 96
          : installProgress?.stage === "ready"
            ? 100
            : null;

  const canOpenChatNow = enabled && (!relayEnabled || backend === "cloud_api") && Boolean(onOpenAiChat);

  return (
    <form onSubmit={(e) => void handleSubmit(e)} className="space-y-4">
      {/* 1. Bật / Tắt Trợ lý AI */}
      <section className="space-y-2.5">
        <div className="flex items-center justify-between gap-2">
          <h4
            className="text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--text-secondary)" }}
          >
            Trợ lý AI
          </h4>
          {canOpenChatNow && onOpenAiChat && (
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
              <span>Mở khung chat AI</span>
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
                <div className="text-[14px] font-semibold">Bật Trợ lý AI</div>
                <p className="mt-0.5 text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  Giúp bạn hỏi đáp nhanh, chỉnh sửa câu chữ và tóm tắt tin nhắn chưa đọc. Mặc định
                  đang tắt để bảo vệ quyền riêng tư.
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

        {relayEnabled && backend === "zeroclaw" && (
          <div
            className="flex items-start gap-2.5 rounded-xl p-3 text-xs leading-relaxed"
            style={{
              background: "rgba(245, 158, 11, 0.10)",
              border: "1px solid rgba(245, 158, 11, 0.35)",
              color: "var(--text-primary)",
            }}
          >
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
            <div>
              Máy tính đang kết nối qua <strong>mạng nội bộ công ty</strong>, nên ZeroClaw tạm
              thời được khóa. Bạn có thể chuyển sang mục <strong>Dùng khóa API</strong> bên dưới để
              trò chuyện với Trợ lý AI nếu mạng công ty cho phép.
            </div>
          </div>
        )}

        {relayEnabled && backend === "cloud_api" && (
          <div
            className="flex items-start gap-2.5 rounded-xl p-3 text-xs leading-relaxed"
            style={{
              background: "rgba(245, 158, 11, 0.08)",
              border: "1px solid rgba(245, 158, 11, 0.28)",
              color: "var(--text-primary)",
            }}
          >
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
            <div>
              Đang dùng <strong>mạng nội bộ công ty</strong>: bạn vẫn có thể hỏi đáp trong khung
              chat AI (nếu máy chủ công ty cho phép), còn tính năng tóm tắt và dịch tin nhắn hội
              thoại được tự động khóa để giữ riêng tư.
            </div>
          </div>
        )}
      </section>

      {/* 2. Chọn cách sử dụng Trợ lý AI (2 thẻ rõ ràng) */}
      <section className="space-y-2">
        <div
          className="text-xs font-semibold uppercase tracking-wider"
          style={{ color: "var(--text-secondary)" }}
        >
          Chọn cách kết nối Trợ lý AI
        </div>
        <div className="grid grid-cols-1 gap-2.5 sm:grid-cols-2">
          <button
            type="button"
            onClick={() => {
              setBackend("cloud_api");
              if (cloudModel.trim() === DEFAULT_ZEROCLAW_MODEL) {
                setCloudModel(activeProviderPreset?.defaultModel || DEFAULT_CLOUD_API_MODEL);
              }
              setFeedback(null);
            }}
            className="flex flex-col items-start gap-1.5 rounded-xl p-3.5 text-left transition"
            style={{
              background: backend === "cloud_api" ? "var(--accent-soft)" : "var(--bg-surface)",
              border:
                backend === "cloud_api"
                  ? "1.5px solid var(--accent)"
                  : "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex w-full items-center justify-between gap-2">
              <span className="inline-flex items-center gap-1.5 text-[13px] font-semibold">
                <KeyRound
                  className="h-4 w-4 shrink-0"
                  style={{ color: backend === "cloud_api" ? "var(--accent)" : "var(--text-secondary)" }}
                />
                <span>Dùng khóa API (Groq · OpenRouter · Grok)</span>
              </span>
              {backend === "cloud_api" && (
                <CheckCircle2 className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              )}
            </div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              Chỉ cần dán khóa API là dùng ngay, không cần cài thêm phần mềm.
            </p>
          </button>

          <button
            type="button"
            onClick={() => {
              setBackend("zeroclaw");
              setFeedback(null);
            }}
            className="flex flex-col items-start gap-1.5 rounded-xl p-3.5 text-left transition"
            style={{
              background: backend === "zeroclaw" ? "var(--accent-soft)" : "var(--bg-surface)",
              border:
                backend === "zeroclaw"
                  ? "1.5px solid var(--accent)"
                  : "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex w-full items-center justify-between gap-2">
              <span className="inline-flex items-center gap-1.5 text-[13px] font-semibold">
                <Cpu
                  className="h-4 w-4 shrink-0"
                  style={{ color: backend === "zeroclaw" ? "var(--accent)" : "var(--text-secondary)" }}
                />
                <span>ZeroClaw (Cài trên máy)</span>
              </span>
              {backend === "zeroclaw" && (
                <CheckCircle2 className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              )}
            </div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              Cài đặt 1 chạm trên máy tính, có bộ nhớ trợ lý riêng và liên kết với khóa API.
            </p>
          </button>
        </div>
      </section>

      {/* 3B. Bộ xử lý ZeroClaw trên máy (chỉ hiện khi chọn ZeroClaw) */}
      {backend === "zeroclaw" && (
        <section
          className="space-y-3 rounded-xl p-4"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div className="text-[13.5px] font-semibold">Bộ xử lý ZeroClaw trên máy</div>
              <p className="mt-0.5 text-xs" style={{ color: "var(--text-secondary)" }}>
                Chỉ cần bấm 1 nút để Teza tự tải và cài đặt sẵn cho bạn.
              </p>
            </div>
            {probingCli ? (
              <span
                className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-medium"
                style={{ background: "var(--bg-sidebar)", color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-3 w-3 animate-spin" />
                Đang kiểm tra...
              </span>
            ) : cliProbe?.available ? (
              <span className="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/15 px-2.5 py-1 text-[11px] font-semibold text-emerald-500">
                <CheckCircle2 className="h-3.5 w-3.5" />
                Đã sẵn sàng{cliProbe.version ? ` · ${cliProbe.version}` : ""}
              </span>
            ) : (
              <span
                className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-medium"
                style={{ background: "var(--bg-sidebar)", color: "var(--text-secondary)" }}
              >
                Chưa cài đặt
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={installing}
              onClick={() => void handleInstallZeroClaw()}
              className="tf-btn inline-flex items-center gap-2 rounded-xl px-4 py-2 text-xs font-semibold disabled:cursor-not-allowed disabled:opacity-60"
            >
              {installing ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Download className="h-4 w-4" />
              )}
              <span>
                {installing
                  ? "Đang tải & cài đặt..."
                  : cliProbe?.available
                    ? "Cập nhật ZeroClaw mới nhất"
                    : "Tải & Cài đặt ZeroClaw"}
              </span>
            </button>

            <button
              type="button"
              disabled={probingCli || installing}
              onClick={() => void runProbe(zeroclawPath, zeroclawGatewayUrl)}
              className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-medium disabled:opacity-55"
              style={{
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              {probingCli ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <RefreshCw className="h-3.5 w-3.5" />
              )}
              <span>Kiểm tra lại</span>
            </button>
          </div>

          {installing && (
            <div className="space-y-1.5 pt-1">
              <div
                className="flex items-center justify-between text-[11.5px]"
                style={{ color: "var(--text-secondary)" }}
              >
                <span>{describeInstallProgress(installProgress)}</span>
                {downloadPct !== null && (
                  <span className="font-semibold tabular-nums">{downloadPct}%</span>
                )}
              </div>
              <div
                className="h-2 w-full overflow-hidden rounded-full"
                style={{ background: "var(--bg-sidebar)" }}
              >
                <div
                  className="h-full rounded-full transition-all duration-200"
                  style={{
                    width: `${downloadPct ?? 18}%`,
                    background: "var(--accent)",
                  }}
                />
              </div>
            </div>
          )}

          {!installing && cliProbe && !cliProbe.available && cliProbe.error && (
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              {cliProbe.error}
            </p>
          )}

          {/* Tìm kiếm web & công cụ an toàn cho ZeroClaw */}
          <div
            className="rounded-xl p-3"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <label className="flex cursor-pointer items-start justify-between gap-3">
              <div className="flex min-w-0 items-start gap-2.5">
                <span
                  className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg"
                  style={{
                    background: zeroclawSafeTools ? "var(--accent-soft)" : "var(--bg-surface)",
                    color: zeroclawSafeTools ? "var(--accent)" : "var(--text-secondary)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <Globe className="h-3.5 w-3.5" />
                </span>
                <div className="min-w-0">
                  <div className="text-[13px] font-semibold">
                    Cho phép tìm kiếm web &amp; công cụ an toàn
                  </div>
                  <p
                    className="mt-0.5 text-xs leading-relaxed"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    Khi bật, ZeroClaw có thể tự tra cứu web (DuckDuckGo), đọc nội dung trang web,
                    tính toán và tra cứu bộ nhớ trong lúc trò chuyện. Lệnh hệ thống và ghi tệp máy
                    tính vẫn luôn bị khóa để an toàn.
                  </p>
                </div>
              </div>
              <input
                type="checkbox"
                checked={zeroclawSafeTools}
                onChange={(e) => setZeroclawSafeTools(e.target.checked)}
                className="mt-1 h-4 w-4 shrink-0 cursor-pointer accent-[var(--accent)]"
              />
            </label>
          </div>
        </section>
      )}

      {/* 3A. Thiết lập dịch vụ AI, khóa API & mẫu AI (dùng chung cho cả Cloud API và ZeroClaw) */}
      <section
        className="space-y-4 rounded-xl p-4"
        style={{
          background: "var(--bg-surface)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        {backend === "zeroclaw" && (
          <div className="space-y-0.5">
            <div className="text-[13.5px] font-semibold">
              Liên kết khóa API &amp; chọn mẫu AI cho ZeroClaw
            </div>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              ZeroClaw là bộ xử lý và bộ nhớ trợ lý trên máy; hãy chọn dịch vụ AI và dán khóa API để
              ZeroClaw trả lời.
            </p>
          </div>
        )}

        {/* 3 nút chọn nhà cung cấp */}
        <div className="space-y-2">
          <label className="text-xs font-semibold">Chọn dịch vụ AI</label>
          <div className="grid grid-cols-3 gap-2">
            {AI_CLOUD_PROVIDERS.map((provider) => {
              const active = activeProviderId === provider.id;
              return (
                <button
                  key={provider.id}
                  type="button"
                  onClick={() => handleSelectCloudProvider(provider)}
                  className="rounded-xl px-3 py-2 text-xs font-semibold transition"
                  style={{
                    background: active ? "var(--accent)" : "var(--bg-sidebar)",
                    color: active ? "var(--text-on-accent)" : "var(--text-primary)",
                    border: active
                      ? "1px solid var(--accent)"
                      : "1px solid var(--border-subtle)",
                  }}
                >
                  {provider.name}
                </button>
              );
            })}
          </div>
          {activeProviderPreset && (
            <p className="text-[11.5px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              {activeProviderPreset.hint}
            </p>
          )}
        </div>

        {/* Ô dán Khóa API */}
        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2">
            <label className="text-xs font-semibold">Khóa API</label>
            <span className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
              {clearApiKey
                ? "Khóa cũ sẽ được xóa khi bấm Lưu"
                : hasSavedKeyForCurrentHost
                  ? `Đã lưu: ${config?.api_key_preview || "••••"}`
                  : hasApiKey
                    ? `Chưa có khóa cho ${activeProviderPreset?.name || "dịch vụ này"}`
                    : "Chưa có khóa"}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <div
              className="flex min-w-0 flex-1 items-center rounded-xl px-3"
              style={{
                background: "var(--bg-input)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <input
                type={showApiKey ? "text" : "password"}
                value={apiKeyInput}
                onChange={(e) => {
                  setApiKeyInput(e.target.value);
                  if (clearApiKey) setClearApiKey(false);
                }}
                placeholder={
                  hasSavedKeyForCurrentHost
                    ? "Để trống để giữ nguyên khóa đã lưu, hoặc dán khóa mới..."
                    : `Dán khóa API ${activeProviderPreset?.name || ""} của bạn (${activeProviderPreset?.keyPlaceholder || "sk-..."})`
                }
                className="min-w-0 flex-1 bg-transparent py-2 text-xs outline-none"
                style={{ color: "var(--text-primary)" }}
              />
              <button
                type="button"
                onClick={() => setShowApiKey((v) => !v)}
                className="p-1 opacity-75 hover:opacity-100"
                title={showApiKey ? "Ẩn khóa API" : "Hiện khóa API"}
              >
                {showApiKey ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
              </button>
            </div>
            {hasSavedKeyForCurrentHost && (
              <button
                type="button"
                onClick={() => {
                  setClearApiKey(true);
                  setApiKeyInput("");
                }}
                className="shrink-0 rounded-xl px-3 py-2 text-xs font-medium text-rose-400 transition hover:bg-rose-500/10"
                style={{ border: "1px solid var(--border-subtle)" }}
              >
                Xóa khóa
              </button>
            )}
          </div>
          <p className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
            {hasApiKey && !hasSavedKeyForCurrentHost
              ? `Bạn vừa đổi sang ${activeProviderPreset?.name || "dịch vụ khác"} — hãy dán khóa API tương ứng để dùng (khóa cũ sẽ không được gửi sang dịch vụ mới).`
              : "Khóa được lưu bảo mật ngay trên máy tính của bạn."}
          </p>
        </div>

        {/* Chọn mẫu AI */}
        <div className="space-y-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label className="text-xs font-semibold">Chọn mẫu AI</label>
            {onListModels && (
              <button
                type="button"
                disabled={loadingModels}
                onClick={() => void handleLoadLatestModels()}
                className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-55"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--accent)",
                }}
              >
                {loadingModels ? (
                  <Loader2 className="h-3 w-3 animate-spin" />
                ) : (
                  <RefreshCw className="h-3 w-3" />
                )}
                <span>{loadingModels ? "Đang tải..." : "Tải danh sách mẫu mới nhất"}</span>
              </button>
            )}
          </div>

          <input
            type="text"
            value={cloudModel}
            onChange={(e) => setCloudModel(e.target.value)}
            placeholder={activeProviderPreset?.defaultModel || DEFAULT_CLOUD_API_MODEL}
            className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          />

          {modelsFeedback && (
            <p
              className="text-[11.5px] leading-relaxed"
              style={{
                color: modelsFeedback.tone === "ok" ? "#22c55e" : "#f87171",
              }}
            >
              {modelsFeedback.text}
            </p>
          )}

          {/* Quick clickable model chips (remote if loaded, else curated presets) */}
          {remoteModels.length > 0 ? (
            <div className="space-y-2">
              {remoteModels.length > 12 && (
                <input
                  type="text"
                  value={modelFilter}
                  onChange={(e) => setModelFilter(e.target.value)}
                  placeholder="Lọc nhanh tên mẫu (ví dụ: llama, grok, free)..."
                  className="w-full rounded-lg px-2.5 py-1.5 text-xs outline-none"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                />
              )}
              <div className="flex flex-wrap gap-1.5">
                {visibleRemoteModels.map((item) => {
                  const selected = cloudModel.trim() === item.id;
                  return (
                    <button
                      key={item.id}
                      type="button"
                      onClick={() => setCloudModel(item.id)}
                      className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11.5px] font-medium transition"
                      style={{
                        background: selected ? "var(--accent)" : "var(--bg-sidebar)",
                        color: selected ? "var(--text-on-accent)" : "var(--text-primary)",
                        border: selected
                          ? "1px solid var(--accent)"
                          : "1px solid var(--border-subtle)",
                      }}
                    >
                      <span className="max-w-[230px] truncate">{item.id}</span>
                      {item.is_free && (
                        <span
                          className="rounded-full px-1.5 py-0.5 text-[10px] font-semibold"
                          style={{
                            background: selected
                              ? "rgba(255,255,255,0.22)"
                              : "rgba(16,185,129,0.16)",
                            color: selected ? "var(--text-on-accent)" : "#34d399",
                          }}
                        >
                          Miễn phí
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
              {filteredRemoteModels.length > 16 && (
                <button
                  type="button"
                  onClick={() => setShowAllRemoteModels((v) => !v)}
                  className="text-[11.5px] font-medium underline opacity-85 hover:opacity-100"
                  style={{ color: "var(--accent)" }}
                >
                  {showAllRemoteModels
                    ? "Thu gọn danh sách"
                    : `Xem tất cả ${filteredRemoteModels.length} mẫu`}
                </button>
              )}
            </div>
          ) : (
            activeProviderPreset && (
              <div className="flex flex-wrap gap-1.5">
                {activeProviderPreset.recommendedModels.map((item) => {
                  const selected = cloudModel.trim() === item.id;
                  return (
                    <button
                      key={item.id}
                      type="button"
                      onClick={() => setCloudModel(item.id)}
                      className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[11.5px] font-medium transition"
                      style={{
                        background: selected ? "var(--accent)" : "var(--bg-sidebar)",
                        color: selected ? "var(--text-on-accent)" : "var(--text-primary)",
                        border: selected
                          ? "1px solid var(--accent)"
                          : "1px solid var(--border-subtle)",
                      }}
                      title={item.id}
                    >
                      <span>{item.label}</span>
                      {item.isFree && (
                        <span
                          className="rounded-full px-1.5 py-0.5 text-[10px] font-semibold"
                          style={{
                            background: selected
                              ? "rgba(255,255,255,0.22)"
                              : "rgba(16,185,129,0.16)",
                            color: selected ? "var(--text-on-accent)" : "#34d399",
                          }}
                        >
                          Miễn phí
                        </span>
                      )}
                    </button>
                  );
                })}
              </div>
            )
          )}
        </div>
      </section>

      {/* 4. Tùy chỉnh cách trả lời & tóm tắt */}
      <section
        className="space-y-3.5 rounded-xl p-4"
        style={{
          background: "var(--bg-surface)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <label className="text-xs font-semibold">Ngôn ngữ tóm tắt tin nhắn</label>
            <select
              value={summaryLanguage}
              onChange={(e) => setSummaryLanguage(e.target.value)}
              className="w-full rounded-xl px-3 py-2 text-xs outline-none"
              style={{
                background: "var(--bg-input)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              {AI_SUMMARY_LANGUAGES.map((item) => (
                <option key={item.code} value={item.code}>
                  {item.label}
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-1.5">
            <div className="flex items-center justify-between text-xs">
              <label className="font-semibold">Số tin nhắn đọc khi tóm tắt</label>
              <span className="font-semibold tabular-nums" style={{ color: "var(--accent)" }}>
                {maxSummaryMessages} tin
              </span>
            </div>
            <input
              type="range"
              min={5}
              max={120}
              step={5}
              value={maxSummaryMessages}
              onChange={(e) => setMaxSummaryMessages(clampSummaryMessageCount(Number(e.target.value)))}
              className="mt-1.5 w-full cursor-pointer accent-[var(--accent)]"
            />
          </div>
        </div>

        <div className="space-y-1.5">
          <div className="flex items-center justify-between gap-2">
            <label className="text-xs font-semibold">Cách Trợ lý trả lời bạn</label>
            <button
              type="button"
              onClick={() => setSystemPrompt(DEFAULT_AI_SYSTEM_PROMPT)}
              className="inline-flex items-center gap-1 text-[11px] opacity-80 hover:opacity-100"
              style={{ color: "var(--accent)" }}
            >
              <RotateCcw className="h-3 w-3" />
              <span>Dùng mặc định</span>
            </button>
          </div>
          <textarea
            rows={3}
            value={systemPrompt}
            onChange={(e) => setSystemPrompt(e.target.value)}
            placeholder="Ví dụ: Trả lời ngắn gọn, dễ hiểu bằng tiếng Việt..."
            className="w-full resize-y rounded-xl px-3 py-2 text-xs leading-relaxed outline-none"
            style={{
              background: "var(--bg-input)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          />
        </div>
      </section>

      {/* 5. Tùy chọn nâng cao (thu gọn mặc định để không làm rối người dùng phổ thông) */}
      <section
        className="rounded-xl"
        style={{
          background: "var(--bg-surface)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <button
          type="button"
          onClick={() => setShowAdvanced((v) => !v)}
          className="flex w-full items-center justify-between px-4 py-3 text-left text-xs font-semibold"
          style={{ color: "var(--text-secondary)" }}
        >
          <span>Tùy chọn nâng cao (không bắt buộc)</span>
          {showAdvanced ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </button>

        {showAdvanced && (
          <div
            className="space-y-3 border-t px-4 pb-4 pt-3"
            style={{ borderColor: "var(--border-subtle)" }}
          >
            <div className="space-y-1">
              <label className="text-xs font-medium">
                Địa chỉ máy chủ AI tùy chỉnh (chỉ hỗ trợ HTTPS)
              </label>
              <input
                type="text"
                value={apiBaseUrl}
                onChange={(e) => {
                  modelsReqSeqRef.current += 1;
                  setLoadingModels(false);
                  setApiBaseUrl(e.target.value);
                  setRemoteModels([]);
                  setModelsFeedback(null);
                }}
                placeholder={DEFAULT_CLOUD_API_BASE_URL}
                className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                style={{
                  background: "var(--bg-input)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
              <p className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                Mục này đã được tự điền sẵn khi bạn chọn Groq, OpenRouter hoặc xAI Grok. Chỉ cần
                thay đổi nếu bạn dùng địa chỉ máy chủ riêng hỗ trợ https://.
              </p>
            </div>

            {backend === "zeroclaw" && (
              <>
                <div className="space-y-1">
                  <label className="text-xs font-medium">Vị trí tệp ZeroClaw trên máy</label>
                  <input
                    type="text"
                    value={zeroclawPath}
                    onChange={(e) => setZeroclawPath(e.target.value)}
                    placeholder="Để mặc định là zeroclaw"
                    className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                    style={{
                      background: "var(--bg-input)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  />
                  <p className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                    Nếu bạn dùng nút "Tải &amp; Cài đặt ZeroClaw" ở trên, mục này sẽ tự động được
                    điền sẵn.
                  </p>
                </div>

                <div className="space-y-1">
                  <label className="text-xs font-medium">
                    Địa chỉ ZeroClaw đang chạy nền trên máy (để trống nếu không dùng)
                  </label>
                  <input
                    type="text"
                    value={zeroclawGatewayUrl}
                    onChange={(e) => setZeroclawGatewayUrl(e.target.value)}
                    placeholder="Ví dụ: http://127.0.0.1:42617"
                    className="w-full rounded-xl px-3 py-2 font-mono text-xs outline-none"
                    style={{
                      background: "var(--bg-input)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  />
                </div>

                {zeroclawGatewayUrl.trim() && (
                  <div className="space-y-1">
                    <div className="flex items-center justify-between gap-2">
                      <label className="text-xs font-medium">Mã bảo vệ kết nối (nếu có)</label>
                      <span className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {hasZeroclawToken
                          ? `Đã lưu: ${config?.zeroclaw_token_preview || "••••"}`
                          : "Không bắt buộc"}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <div
                        className="flex min-w-0 flex-1 items-center rounded-xl px-3"
                        style={{
                          background: "var(--bg-input)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <input
                          type={showZeroclawToken ? "text" : "password"}
                          value={zeroclawTokenInput}
                          onChange={(e) => {
                            setZeroclawTokenInput(e.target.value);
                            if (clearZeroclawToken) setClearZeroclawToken(false);
                          }}
                          placeholder={
                            hasZeroclawToken
                              ? "Để trống để giữ nguyên mã đã lưu..."
                              : "Nhập mã nếu ZeroClaw có bật xác thực"
                          }
                          className="min-w-0 flex-1 bg-transparent py-2 text-xs outline-none"
                          style={{ color: "var(--text-primary)" }}
                        />
                        <button
                          type="button"
                          onClick={() => setShowZeroclawToken((v) => !v)}
                          className="p-1 opacity-75 hover:opacity-100"
                        >
                          {showZeroclawToken ? (
                            <EyeOff className="h-3.5 w-3.5" />
                          ) : (
                            <Eye className="h-3.5 w-3.5" />
                          )}
                        </button>
                      </div>
                      {hasZeroclawToken && (
                        <button
                          type="button"
                          onClick={() => {
                            setClearZeroclawToken(true);
                            setZeroclawTokenInput("");
                          }}
                          className="rounded-xl px-2.5 py-2 text-xs font-medium text-rose-400 transition hover:bg-rose-500/10"
                          style={{ border: "1px solid var(--border-subtle)" }}
                        >
                          Xóa
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
        )}
      </section>

      {feedback && (
        <div
          className="rounded-xl px-3.5 py-2.5 text-xs font-medium leading-relaxed"
          style={{
            background:
              feedback.tone === "ok"
                ? "rgba(34, 197, 94, 0.12)"
                : feedback.tone === "warn"
                  ? "rgba(245, 158, 11, 0.12)"
                  : "rgba(239, 68, 68, 0.12)",
            border: `1px solid ${
              feedback.tone === "ok"
                ? "rgba(34, 197, 94, 0.35)"
                : feedback.tone === "warn"
                  ? "rgba(245, 158, 11, 0.38)"
                  : "rgba(239, 68, 68, 0.35)"
            }`,
            color:
              feedback.tone === "ok"
                ? "#22c55e"
                : feedback.tone === "warn"
                  ? "var(--text-primary)"
                  : "#f87171",
          }}
        >
          {feedback.text}
        </div>
      )}

      <div className="flex items-center justify-end gap-2 pt-1">
        <button
          type="submit"
          disabled={saving || installing}
          className="tf-btn inline-flex items-center gap-2 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-60"
        >
          {saving ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
          <span>{saving ? "Đang lưu..." : "Lưu cài đặt"}</span>
        </button>
      </div>
    </form>
  );
}

export default AiSettingsSection;
