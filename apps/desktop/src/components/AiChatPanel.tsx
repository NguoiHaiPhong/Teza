import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  Bot,
  Check,
  CheckCircle2,
  ChevronDown,
  CircleX,
  Clock,
  Copy,
  CornerDownLeft,
  Cpu,
  Download,
  FileCode2,
  KeyRound,
  Loader2,
  Lock,
  MessageSquare,
  OctagonX,
  Pencil,
  Plus,
  RefreshCw,
  Send,
  Shield,
  ShieldAlert,
  SlidersHorizontal,
  Sparkles,
  Square,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import { useI18n, type TranslationKey } from "../i18n";
import { copyTextReliable } from "../utils/clipboard";
import {
  computeApprovalRemainingSecs,
  extractApprovalDetailView,
  formatSystemAccessShortLabel,
  formatTezaAccessShortLabel,
  formatToolDisplayName,
  getAccessBadgeTone,
  isAllowSessionPermitted,
  mergeToolActivities,
  normalizeSystemAccessLevel,
  normalizeTezaAccessLevel,
  shouldShowEmergencyStop,
  type AccessBadgeTone,
  type AiApprovalDecision,
  type AiConfigUpdateWithPermissions,
  type AiConfigWithPermissions,
  type AiToolActivityEvent,
  type AiToolApprovalRequest,
  type SystemAccessLevel,
  type TezaAccessLevel,
} from "../utils/aiPermissions";
import {
  AI_CLOUD_PROVIDERS,
  DEFAULT_CLOUD_API_BASE_URL,
  detectAiCloudProvider,
  filterAiSlashCommands,
  formatAiHelpMarkdown,
  formatInstallSizeMb,
  formatLocalDateTime,
  formatShortAiModelLabel,
  getAiBackendBadgeLabel,
  getAiCloudProviderPreset,
  isPublicCloudModelsProvider,
  isSameCloudApiHost,
  needsZeroClawApiKeyLink,
  normalizeEpochMs,
  parseAiSlashCommand,
  parseInlineMarkdown,
  parseSimpleMarkdownBlocks,
  resolveQuickSwitchModelBackend,
  type AiBackendKind,
  type AiCloudProviderId,
  type AiCommandResultDto,
  type AiInstallZeroClawResponseDto,
  type AiModelInfoDto,
  type AiSessionDto,
  type AiSlashCommandAction,
  type AiSlashCommandDef,
  type AiStoredMessageDto,
  type ZeroClawInstallProgressDto,
} from "../utils/automationHelpers";

function accessPillStyle(tone: AccessBadgeTone): React.CSSProperties {
  switch (tone) {
    case "danger":
      return {
        background: "rgba(239, 68, 68, 0.14)",
        color: "#f87171",
        border: "1px solid rgba(239, 68, 68, 0.42)",
      };
    case "warn":
      return {
        background: "rgba(245, 158, 11, 0.14)",
        color: "#fbbf24",
        border: "1px solid rgba(245, 158, 11, 0.38)",
      };
    case "info":
      return {
        background: "var(--accent-soft)",
        color: "var(--accent)",
        border: "1px solid var(--chip-border)",
      };
    case "neutral":
    default:
      return {
        background: "var(--chip-bg)",
        color: "var(--text-secondary)",
        border: "1px solid var(--chip-border)",
      };
  }
}

interface PopoverViewportPos {
  left: number;
  top: number;
  width: number;
  maxHeight: number;
}

const POPOVER_VIEWPORT_MARGIN_PX = 12;
const MODEL_PICKER_WIDTH_PX = 320;
const SESSION_PICKER_WIDTH_PX = 336;

function computeClampedPopoverPos(
  triggerRect: DOMRect,
  desiredWidth: number,
  viewportWidth: number,
  viewportHeight: number,
): PopoverViewportPos {
  const maxWidth = Math.max(240, viewportWidth - POPOVER_VIEWPORT_MARGIN_PX * 2);
  const width = Math.min(desiredWidth, maxWidth);
  const maxLeft = Math.max(
    POPOVER_VIEWPORT_MARGIN_PX,
    viewportWidth - width - POPOVER_VIEWPORT_MARGIN_PX,
  );
  const left = Math.min(Math.max(POPOVER_VIEWPORT_MARGIN_PX, triggerRect.left), maxLeft);
  const top = Math.min(
    triggerRect.bottom + 6,
    Math.max(POPOVER_VIEWPORT_MARGIN_PX, viewportHeight - 180),
  );
  const maxHeight = Math.max(180, viewportHeight - top - POPOVER_VIEWPORT_MARGIN_PX);
  return { left, top, width, maxHeight };
}

export interface AiChatPanelProps {
  config: AiConfigWithPermissions | null;
  relayEnabled?: boolean;
  messages: AiStoredMessageDto[];
  sessions?: AiSessionDto[];
  activeSessionId?: string;
  streamingText: string;
  isStreaming: boolean;
  error: string | null;
  tezaAccessLevel?: TezaAccessLevel;
  systemAccessLevel?: SystemAccessLevel;
  toolActivities?: AiToolActivityEvent[];
  pendingApprovals?: AiToolApprovalRequest[];
  emergencyStopping?: boolean;
  onSend: (prompt: string) => void;
  onCancelStream?: () => void;
  onClearHistory: () => void;
  onSelectSession?: (sessionId: string) => void;
  onCreateSession?: () => Promise<void> | void;
  onRenameSession?: (sessionId: string, title: string) => Promise<void> | void;
  onDeleteSession?: (sessionId: string) => Promise<void> | void;
  onRunCommand?: (
    command: "memory" | "memory_clear" | "skills" | "status",
  ) => Promise<AiCommandResultDto>;
  onOpenSettings: () => void;
  onClose?: () => void;
  onQuickSwitchConfig?: (
    update: AiConfigUpdateWithPermissions,
  ) => Promise<AiConfigWithPermissions | void>;
  onListModels?: (baseUrl?: string, apiKey?: string) => Promise<AiModelInfoDto[]>;
  onInstallZeroClaw?: () => Promise<AiInstallZeroClawResponseDto>;
  onSubscribeInstallProgress?: (
    handler: (ev: ZeroClawInstallProgressDto) => void,
  ) => Promise<() => void>;
  onResolveApproval?: (approvalId: string, decision: AiApprovalDecision) => Promise<void> | void;
  onEmergencyStop?: () => Promise<void> | void;
}

interface LocalCommandCardState extends AiCommandResultDto {
  showSwitchModelHint?: boolean;
}

function formatSessionRelativeTime(ts: number): string {
  const ms = normalizeEpochMs(ts);
  if (ms <= 0) return "";
  const diffMs = Date.now() - ms;
  if (diffMs < 60_000) return "Vừa xong";
  const mins = Math.floor(diffMs / 60_000);
  if (mins < 60) return `${mins} phút trước`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} giờ trước`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "Hôm qua";
  if (days < 7) return `${days} ngày trước`;
  return formatLocalDateTime(ms);
}

function describeQuickInstallProgress(progress: ZeroClawInstallProgressDto | null): string {
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
      return "Đang kiểm tra độ an toàn tệp tải về...";
    case "extracting":
      return "Đang cài đặt ZeroClaw vào máy...";
    case "ready":
      return "Cài đặt hoàn tất!";
    default:
      return "Đang cài đặt ZeroClaw...";
  }
}

interface StarterChip {
  id: string;
  label: string;
  prompt: string;
  fillOnly?: boolean;
}

const STARTER_CHIPS: StarterChip[] = [
  {
    id: "pro-msg",
    label: "Viết tin nhắn chuyên nghiệp",
    prompt: "Viết giúp tôi một tin nhắn chuyên nghiệp, lịch sự để hẹn lịch trao đổi công việc vào sáng mai.",
  },
  {
    id: "translate-en",
    label: "Dịch sang tiếng Anh",
    prompt: "Dịch đoạn sau sang tiếng Anh tự nhiên và chuyên nghiệp:\n",
    fillOnly: true,
  },
  {
    id: "summarize",
    label: "Tóm tắt nội dung",
    prompt: "Hãy tóm tắt ngắn gọn các ý chính và đầu việc cần làm từ đoạn sau:\n",
    fillOnly: true,
  },
  {
    id: "polish",
    label: "Chỉnh văn phong gọn gàng",
    prompt: "Kiểm tra lỗi chính tả và viết lại tự nhiên, ngắn gọn hơn cho nội dung sau:\n",
    fillOnly: true,
  },
];

function renderInlineTokens(text: string, keyPrefix: string) {
  const tokens = parseInlineMarkdown(text);
  return tokens.map((tok, idx) => {
    const key = `${keyPrefix}-${idx}`;
    if (tok.kind === "code") {
      return (
        <code
          key={key}
          className="rounded px-1.5 py-0.5 font-mono text-[0.88em]"
          style={{
            background: "var(--card-bg)",
            border: "1px solid var(--border-subtle)",
          }}
        >
          {tok.value}
        </code>
      );
    }
    if (tok.kind === "bold") {
      return (
        <strong key={key} className="font-semibold">
          {tok.value}
        </strong>
      );
    }
    if (tok.kind === "italic") {
      return (
        <em key={key} className="italic">
          {tok.value}
        </em>
      );
    }
    return <span key={key}>{tok.value}</span>;
  });
}

function SafeMarkdownView({
  text,
  copiedCodeKey,
  onCopyCode,
  msgKey,
}: {
  text: string;
  copiedCodeKey: string | null;
  onCopyCode: (key: string, code: string) => void;
  msgKey: string;
}) {
  const blocks = useMemo(() => parseSimpleMarkdownBlocks(text), [text]);
  if (blocks.length === 0) return null;

  return (
    <div className="space-y-2 leading-relaxed">
      {blocks.map((block, idx) => {
        const bKey = `${msgKey}-b-${idx}`;
        if (block.kind === "heading") {
          const sizeClass =
            block.level === 1
              ? "text-[15.5px] font-bold"
              : block.level === 2
                ? "text-[14.5px] font-semibold"
                : "text-[13.5px] font-semibold";
          return (
            <div key={bKey} className={sizeClass}>
              {renderInlineTokens(block.text, bKey)}
            </div>
          );
        }

        if (block.kind === "code") {
          const isCopied = copiedCodeKey === bKey;
          return (
            <div
              key={bKey}
              className="overflow-hidden rounded-xl"
              style={{
                background: "var(--bg-stage)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <div
                className="flex items-center justify-between px-3 py-1.5 text-[11px]"
                style={{
                  background: "var(--card-bg)",
                  borderBottom: "1px solid var(--border-subtle)",
                  color: "var(--text-secondary)",
                }}
              >
                <span className="font-mono uppercase tracking-wide">
                  {block.lang || "code"}
                </span>
                <button
                  type="button"
                  onClick={() => onCopyCode(bKey, block.code)}
                  className="hover-menu inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] font-medium transition"
                  style={{ color: isCopied ? "#22c55e" : "var(--text-secondary)" }}
                >
                  {isCopied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
                  <span>{isCopied ? "Đã chép" : "Sao chép"}</span>
                </button>
              </div>
              <pre className="overflow-x-auto p-3 font-mono text-[12.5px] leading-relaxed">
                <code>{block.code}</code>
              </pre>
            </div>
          );
        }

        if (block.kind === "ul") {
          return (
            <ul key={bKey} className="list-disc space-y-1 pl-5">
              {block.items.map((item, itemIdx) => (
                <li key={`${bKey}-li-${itemIdx}`}>
                  {renderInlineTokens(item, `${bKey}-li-${itemIdx}`)}
                </li>
              ))}
            </ul>
          );
        }

        if (block.kind === "ol") {
          return (
            <ol key={bKey} className="list-decimal space-y-1 pl-5">
              {block.items.map((item, itemIdx) => (
                <li key={`${bKey}-li-${itemIdx}`}>
                  {renderInlineTokens(item, `${bKey}-li-${itemIdx}`)}
                </li>
              ))}
            </ol>
          );
        }

        const lines = block.text.split("\n");
        return (
          <p key={bKey} className="whitespace-pre-wrap">
            {lines.map((ln, lineIdx) => (
              <span key={`${bKey}-ln-${lineIdx}`}>
                {lineIdx > 0 && <br />}
                {renderInlineTokens(ln, `${bKey}-ln-${lineIdx}`)}
              </span>
            ))}
          </p>
        );
      })}
    </div>
  );
}

export function AiChatPanel({
  config,
  relayEnabled = false,
  messages,
  sessions = [],
  activeSessionId = "default",
  streamingText,
  isStreaming,
  error,
  tezaAccessLevel,
  systemAccessLevel,
  toolActivities = [],
  pendingApprovals = [],
  emergencyStopping = false,
  onSend,
  onCancelStream,
  onClearHistory,
  onSelectSession,
  onCreateSession,
  onRenameSession,
  onDeleteSession,
  onRunCommand,
  onOpenSettings,
  onClose,
  onQuickSwitchConfig,
  onListModels,
  onInstallZeroClaw,
  onSubscribeInstallProgress,
  onResolveApproval,
  onEmergencyStop,
}: AiChatPanelProps) {
  const { t, lang } = useI18n();
  const tr = useCallback(
    (key: string, fallback: string, vars?: Record<string, string | number>): string => {
      const translated = t(key as TranslationKey, vars);
      if (translated !== key) return translated;
      if (!vars) return fallback;
      let text = fallback;
      for (const [k, v] of Object.entries(vars)) {
        text = text.replaceAll(`{${k}}`, String(v));
      }
      return text;
    },
    [t],
  );

  const [draft, setDraft] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [copiedCodeKey, setCopiedCodeKey] = useState<string | null>(null);
  const [cancellingStream, setCancellingStream] = useState(false);

  // Live approvals & E-Stop local state
  const [approvalNowMs, setApprovalNowMs] = useState(() => Date.now());
  const [busyApprovalDecisions, setBusyApprovalDecisions] = useState<
    Record<string, AiApprovalDecision>
  >({});
  const [approvalErrors, setApprovalErrors] = useState<Record<string, string>>({});
  const [localEstopBusy, setLocalEstopBusy] = useState(false);
  const busyApprovalSetRef = useRef<Set<string>>(new Set());

  // Multi-session switcher popover state
  const [sessionPickerOpen, setSessionPickerOpen] = useState(false);
  const [renamingSessionId, setRenamingSessionId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [confirmDeleteSessionId, setConfirmDeleteSessionId] = useState<string | null>(null);
  const [sessionBusy, setSessionBusy] = useState(false);
  const [sessionError, setSessionError] = useState<string | null>(null);

  // Slash command autocomplete & local command output card state
  const [slashSelectedIndex, setSlashSelectedIndex] = useState(0);
  const [slashMenuDismissed, setSlashMenuDismissed] = useState(false);
  const [commandRunning, setCommandRunning] = useState<string | null>(null);
  const [commandOutput, setCommandOutput] = useState<LocalCommandCardState | null>(null);

  // In-chat source & model picker state
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerPos, setPickerPos] = useState<PopoverViewportPos | null>(null);
  const [sessionPickerPos, setSessionPickerPos] = useState<PopoverViewportPos | null>(null);
  const [pickerProviderId, setPickerProviderId] = useState<Exclude<AiCloudProviderId, "custom">>(
    "groq",
  );
  const [pickerModels, setPickerModels] = useState<AiModelInfoDto[]>([]);
  const [pickerModelsLoading, setPickerModelsLoading] = useState(false);
  const [pickerFilter, setPickerFilter] = useState("");
  const [pickerBusy, setPickerBusy] = useState(false);
  const [pickerNotice, setPickerNotice] = useState<string | null>(null);

  // Instant-start 1-click ZeroClaw install state (welcome card)
  const [quickInstalling, setQuickInstalling] = useState(false);
  const [quickInstallProgress, setQuickInstallProgress] =
    useState<ZeroClawInstallProgressDto | null>(null);
  const [quickInstallError, setQuickInstallError] = useState<string | null>(null);

  const listRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const pickerRef = useRef<HTMLDivElement | null>(null);
  const pickerPopoverRef = useRef<HTMLDivElement | null>(null);
  const sessionPickerRef = useRef<HTMLDivElement | null>(null);
  const sessionPickerPopoverRef = useRef<HTMLDivElement | null>(null);
  const pickerReqSeqRef = useRef(0);
  const commandReqSeqRef = useRef(0);
  const copyTimerRef = useRef<number | null>(null);
  const codeCopyTimerRef = useRef<number | null>(null);

  const enabled = Boolean(config?.enabled);
  const isZeroClawMode = config?.backend === "zeroclaw";
  const relayBlocksChat = relayEnabled && config?.backend !== "cloud_api";
  const needsZeroClawKeyBanner = needsZeroClawApiKeyLink(config, relayEnabled);
  const backendLabel = getAiBackendBadgeLabel(config);
  const shortModelLabel = enabled && config ? formatShortAiModelLabel(config.model) : "";

  const effectiveTezaLevel = normalizeTezaAccessLevel(tezaAccessLevel ?? config?.teza_access_level);
  const effectiveSystemLevel = normalizeSystemAccessLevel(
    systemAccessLevel ?? config?.system_access_level,
  );
  const accessBadge = useMemo(
    () => getAccessBadgeTone(effectiveTezaLevel, effectiveSystemLevel),
    [effectiveTezaLevel, effectiveSystemLevel],
  );
  const showPermissionPills =
    enabled && isZeroClawMode && !relayBlocksChat && accessBadge.isElevated;
  const estopBusy = emergencyStopping || localEstopBusy;
  const showEstopButton =
    Boolean(onEmergencyStop) &&
    shouldShowEmergencyStop({
      isStreaming,
      pendingApprovalsCount: pendingApprovals.length,
      tezaAccessLevel: effectiveTezaLevel,
      systemAccessLevel: effectiveSystemLevel,
    });

  const currentSessionActivities = useMemo(() => {
    const filtered = toolActivities.filter(
      (ev) => !ev.session_id || ev.session_id === activeSessionId,
    );
    return mergeToolActivities(filtered);
  }, [toolActivities, activeSessionId]);

  const currentSessionApprovals = useMemo(
    () =>
      pendingApprovals.filter((req) => !req.session_id || req.session_id === activeSessionId),
    [pendingApprovals, activeSessionId],
  );

  const otherSessionApprovals = useMemo(
    () =>
      pendingApprovals.filter(
        (req) => Boolean(req.session_id) && req.session_id !== activeSessionId,
      ),
    [pendingApprovals, activeSessionId],
  );

  const firstOtherApprovalSession = useMemo(() => {
    const first = otherSessionApprovals[0];
    if (!first) return null;
    const match = sessions.find((s) => s.id === first.session_id);
    return {
      id: first.session_id,
      title: match?.title?.trim() || first.session_id,
    };
  }, [otherSessionApprovals, sessions]);

  // Live 1s countdown tick only while at least one approval card is pending
  useEffect(() => {
    if (pendingApprovals.length === 0) return;
    setApprovalNowMs(Date.now());
    const timer = window.setInterval(() => {
      setApprovalNowMs(Date.now());
    }, 1000);
    return () => window.clearInterval(timer);
  }, [pendingApprovals.length]);

  // Prune stale per-card errors/busy states when approvals are resolved or expire
  useEffect(() => {
    const activeIds = new Set(pendingApprovals.map((a) => a.approval_id));
    for (const id of Array.from(busyApprovalSetRef.current)) {
      if (!activeIds.has(id)) busyApprovalSetRef.current.delete(id);
    }
    setBusyApprovalDecisions((prev) => {
      const keys = Object.keys(prev);
      if (keys.every((k) => activeIds.has(k))) return prev;
      const next: Record<string, AiApprovalDecision> = {};
      for (const k of keys) {
        if (activeIds.has(k)) next[k] = prev[k];
      }
      return next;
    });
    setApprovalErrors((prev) => {
      const keys = Object.keys(prev);
      if (keys.every((k) => activeIds.has(k))) return prev;
      const next: Record<string, string> = {};
      for (const k of keys) {
        if (activeIds.has(k)) next[k] = prev[k];
      }
      return next;
    });
  }, [pendingApprovals]);

  const handleResolveApprovalClick = async (
    approvalId: string,
    decision: AiApprovalDecision,
  ) => {
    if (!onResolveApproval || busyApprovalSetRef.current.has(approvalId)) return;
    busyApprovalSetRef.current.add(approvalId);
    setBusyApprovalDecisions((prev) => ({ ...prev, [approvalId]: decision }));
    setApprovalErrors((prev) => {
      if (!(approvalId in prev)) return prev;
      const next = { ...prev };
      delete next[approvalId];
      return next;
    });
    try {
      await onResolveApproval(approvalId, decision);
    } catch (err) {
      setApprovalErrors((prev) => ({
        ...prev,
        [approvalId]: err instanceof Error ? err.message : String(err),
      }));
    } finally {
      busyApprovalSetRef.current.delete(approvalId);
      setBusyApprovalDecisions((prev) => {
        if (!(approvalId in prev)) return prev;
        const next = { ...prev };
        delete next[approvalId];
        return next;
      });
    }
  };

  const handleEmergencyStopClick = async () => {
    if (!onEmergencyStop || estopBusy) return;
    setLocalEstopBusy(true);
    try {
      await onEmergencyStop();
    } finally {
      setLocalEstopBusy(false);
    }
  };

  const activeSession = useMemo(
    () => sessions.find((s) => s.id === activeSessionId) ?? null,
    [sessions, activeSessionId],
  );
  const activeSessionTitle = activeSession?.title?.trim() || "Cuộc trò chuyện mới";
  const hasSessionControls = Boolean(onSelectSession || onCreateSession);

  const slashSuggestions = useMemo(
    () =>
      !enabled || isStreaming || slashMenuDismissed ? [] : filterAiSlashCommands(draft, true),
    [enabled, isStreaming, slashMenuDismissed, draft],
  );
  const activeSlashIndex =
    slashSuggestions.length > 0 ? Math.min(slashSelectedIndex, slashSuggestions.length - 1) : 0;

  useEffect(() => {
    return () => {
      if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
      if (codeCopyTimerRef.current !== null) window.clearTimeout(codeCopyTimerRef.current);
    };
  }, []);

  useEffect(() => {
    if (!isStreaming) {
      setCancellingStream(false);
    }
  }, [isStreaming]);

  // Clear local command result & clear banner when switching sessions
  useEffect(() => {
    commandReqSeqRef.current += 1;
    setCommandRunning(null);
    setCommandOutput(null);
    setConfirmClear(false);
  }, [activeSessionId]);

  // Close session picker on outside click or Escape
  useEffect(() => {
    if (!sessionPickerOpen) return;
    const onMouseDown = (ev: MouseEvent) => {
      const target = ev.target as Node;
      const insideTrigger = Boolean(sessionPickerRef.current?.contains(target));
      const insidePopover = Boolean(sessionPickerPopoverRef.current?.contains(target));
      if (!insideTrigger && !insidePopover) {
        setSessionPickerOpen(false);
        setRenamingSessionId(null);
        setConfirmDeleteSessionId(null);
        setSessionError(null);
      }
    };
    const onKeyDown = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        if (renamingSessionId) {
          setRenamingSessionId(null);
          return;
        }
        setSessionPickerOpen(false);
        setConfirmDeleteSessionId(null);
        setSessionError(null);
      }
    };
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [sessionPickerOpen, renamingSessionId]);

  // Keep portaled session picker aligned & viewport-clamped across resizes
  useLayoutEffect(() => {
    if (!sessionPickerOpen) {
      setSessionPickerPos(null);
      return;
    }
    const updatePos = () => {
      const triggerEl = sessionPickerRef.current;
      if (!triggerEl) return;
      setSessionPickerPos(
        computeClampedPopoverPos(
          triggerEl.getBoundingClientRect(),
          SESSION_PICKER_WIDTH_PX,
          window.innerWidth,
          window.innerHeight,
        ),
      );
    };
    updatePos();
    window.addEventListener("resize", updatePos);
    window.addEventListener("scroll", updatePos, true);
    return () => {
      window.removeEventListener("resize", updatePos);
      window.removeEventListener("scroll", updatePos, true);
    };
  }, [sessionPickerOpen]);

  // Sync picker tab with current saved provider whenever config updates
  useEffect(() => {
    const detected = detectAiCloudProvider(config?.api_base_url || DEFAULT_CLOUD_API_BASE_URL);
    if (detected !== "custom") {
      setPickerProviderId(detected);
    }
  }, [config?.api_base_url]);

  // Close picker on outside click or Escape
  useEffect(() => {
    if (!pickerOpen) return;
    const onMouseDown = (ev: MouseEvent) => {
      const target = ev.target as Node;
      const insideTrigger = Boolean(pickerRef.current?.contains(target));
      const insidePopover = Boolean(pickerPopoverRef.current?.contains(target));
      if (!insideTrigger && !insidePopover) {
        setPickerOpen(false);
      }
    };
    const onKeyDown = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        setPickerOpen(false);
      }
    };
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [pickerOpen]);

  // Keep portaled model picker aligned & viewport-clamped across resizes
  useLayoutEffect(() => {
    if (!pickerOpen) {
      setPickerPos(null);
      return;
    }
    const updatePos = () => {
      const triggerEl = pickerRef.current;
      if (!triggerEl) return;
      setPickerPos(
        computeClampedPopoverPos(
          triggerEl.getBoundingClientRect(),
          MODEL_PICKER_WIDTH_PX,
          window.innerWidth,
          window.innerHeight,
        ),
      );
    };
    updatePos();
    window.addEventListener("resize", updatePos);
    window.addEventListener("scroll", updatePos, true);
    return () => {
      window.removeEventListener("resize", updatePos);
      window.removeEventListener("scroll", updatePos, true);
    };
  }, [pickerOpen]);

  // Subscribe to live ZeroClaw install progress while installing from onboarding card
  useEffect(() => {
    if (!onSubscribeInstallProgress) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    void onSubscribeInstallProgress((ev) => {
      if (active) setQuickInstallProgress(ev);
    }).then((fn) => {
      if (active) unlisten = fn;
      else fn();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, [onSubscribeInstallProgress]);

  const activePickerPreset = getAiCloudProviderPreset(pickerProviderId) ?? AI_CLOUD_PROVIDERS[0];
  const hasSavedKeyForPickerProvider =
    Boolean(config?.has_api_key) &&
    isSameCloudApiHost(
      activePickerPreset.baseUrl,
      config?.api_base_url || DEFAULT_CLOUD_API_BASE_URL,
    );
  const canScanPickerModels =
    Boolean(onListModels) &&
    (isPublicCloudModelsProvider(activePickerPreset.baseUrl) || hasSavedKeyForPickerProvider);

  const loadPickerModels = useCallback(
    async (baseUrl: string, showError: boolean) => {
      if (!onListModels) return;
      const seq = ++pickerReqSeqRef.current;
      setPickerModelsLoading(true);
      if (showError) setPickerNotice(null);
      try {
        const list = await onListModels(baseUrl);
        if (seq !== pickerReqSeqRef.current) return;
        setPickerModels(list);
      } catch (err) {
        if (seq !== pickerReqSeqRef.current) return;
        if (showError) {
          setPickerNotice(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (seq === pickerReqSeqRef.current) {
          setPickerModelsLoading(false);
        }
      }
    },
    [onListModels],
  );

  // Auto-scan live free models whenever the picker opens or switches provider
  useEffect(() => {
    if (!pickerOpen) {
      pickerReqSeqRef.current += 1;
      setPickerModelsLoading(false);
      return;
    }
    pickerReqSeqRef.current += 1;
    setPickerModelsLoading(false);
    setPickerModels([]);
    setPickerFilter("");
    setPickerNotice(null);
    if (!canScanPickerModels) return;
    void loadPickerModels(activePickerPreset.baseUrl, false);
  }, [pickerOpen, activePickerPreset.baseUrl, canScanPickerModels, loadPickerModels]);

  const pickerDisplayModels = useMemo(() => {
    const baseList: { id: string; label: string; isFree: boolean }[] =
      pickerModels.length > 0
        ? pickerModels.map((m) => ({
            id: m.id,
            label: m.id,
            isFree: m.is_free,
          }))
        : activePickerPreset.recommendedModels.map((m) => ({
            id: m.id,
            label: m.label,
            isFree: Boolean(m.isFree),
          }));
    const q = pickerFilter.trim().toLowerCase();
    if (!q) return baseList.slice(0, 24);
    return baseList
      .filter(
        (m) =>
          m.id.toLowerCase().includes(q) ||
          m.label.toLowerCase().includes(q) ||
          ((q === "free" || q === "miễn phí") && m.isFree),
      )
      .slice(0, 24);
  }, [pickerModels, activePickerPreset, pickerFilter]);

  const handleQuickSelectBackend = async (targetBackend: AiBackendKind) => {
    if (!onQuickSwitchConfig || pickerBusy) return;
    if (targetBackend === "zeroclaw" && relayEnabled) return;
    if (targetBackend === "cloud_api" && !config?.has_api_key) {
      setPickerNotice("Để gọi trực tiếp khóa API, hãy dán khóa API trong Cài đặt AI.");
      return;
    }
    setPickerBusy(true);
    setPickerNotice(null);
    try {
      await onQuickSwitchConfig({ backend: targetBackend, enabled: true });
      setPickerOpen(false);
    } catch (err) {
      setPickerNotice(err instanceof Error ? err.message : String(err));
    } finally {
      setPickerBusy(false);
    }
  };

  const handleQuickSelectCloudModel = async (modelId: string) => {
    if (!onQuickSwitchConfig || pickerBusy) return;
    if (!hasSavedKeyForPickerProvider) {
      setPickerNotice(
        `Để dùng ${activePickerPreset.name}, hãy dán khóa API trong Cài đặt AI.`,
      );
      return;
    }
    setPickerBusy(true);
    setPickerNotice(null);
    try {
      const targetBackend = resolveQuickSwitchModelBackend(config?.backend, relayEnabled);
      await onQuickSwitchConfig({
        backend: targetBackend,
        api_base_url: activePickerPreset.baseUrl,
        model: modelId,
        enabled: true,
      });
      setPickerOpen(false);
    } catch (err) {
      setPickerNotice(err instanceof Error ? err.message : String(err));
    } finally {
      setPickerBusy(false);
    }
  };

  const handleQuickInstallZeroClaw = async () => {
    if (quickInstalling) return;
    if (!onInstallZeroClaw) {
      onOpenSettings();
      return;
    }
    setQuickInstalling(true);
    setQuickInstallError(null);
    setQuickInstallProgress({
      stage: "checking",
      downloaded_bytes: 0,
      total_bytes: null,
      tag: null,
    });
    try {
      await onInstallZeroClaw();
      if (onQuickSwitchConfig) {
        await onQuickSwitchConfig({ enabled: true, backend: "zeroclaw" });
      }
    } catch (err) {
      setQuickInstallError(err instanceof Error ? err.message : String(err));
    } finally {
      setQuickInstalling(false);
      setQuickInstallProgress(null);
    }
  };

  const quickInstallPct =
    quickInstallProgress?.stage === "downloading" &&
    quickInstallProgress.total_bytes &&
    quickInstallProgress.total_bytes > 0
      ? Math.min(
          100,
          Math.round((quickInstallProgress.downloaded_bytes / quickInstallProgress.total_bytes) * 100),
        )
      : quickInstallProgress?.stage === "verifying"
        ? 92
        : quickInstallProgress?.stage === "extracting"
          ? 96
          : quickInstallProgress?.stage === "ready"
            ? 100
            : null;

  // Auto-scroll to bottom on new messages, streaming chunks, tool activities, or command results
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const distanceToBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (
      distanceToBottom < 220 ||
      isStreaming ||
      currentSessionActivities.length > 0 ||
      commandOutput ||
      commandRunning
    ) {
      el.scrollTop = el.scrollHeight;
    }
  }, [
    messages.length,
    streamingText,
    isStreaming,
    currentSessionActivities.length,
    commandOutput,
    commandRunning,
  ]);

  // Reset clear confirmation when messages empty
  useEffect(() => {
    if (messages.length === 0) {
      setConfirmClear(false);
    }
  }, [messages.length]);

  // Auto-resize composer textarea
  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(160, Math.max(40, ta.scrollHeight))}px`;
  }, [draft]);

  const handleCopyBubble = (id: string, text: string) => {
    void copyTextReliable(text)
      .then(() => {
        setCopiedId(id);
        if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
        copyTimerRef.current = window.setTimeout(() => setCopiedId(null), 1800);
      })
      .catch(() => {
        // Ignore clipboard failure silently
      });
  };

  const handleCopyCode = (key: string, code: string) => {
    void copyTextReliable(code)
      .then(() => {
        setCopiedCodeKey(key);
        if (codeCopyTimerRef.current !== null) window.clearTimeout(codeCopyTimerRef.current);
        codeCopyTimerRef.current = window.setTimeout(() => setCopiedCodeKey(null), 1800);
      })
      .catch(() => {
        // Ignore clipboard failure silently
      });
  };

  const handleCancelStreamClick = () => {
    if (!onCancelStream || !isStreaming || cancellingStream) return;
    setCancellingStream(true);
    onCancelStream();
  };

  const handleCreateNewSession = async () => {
    if (!onCreateSession || isStreaming || sessionBusy) return;
    setSessionBusy(true);
    setSessionError(null);
    try {
      await onCreateSession();
      setSessionPickerOpen(false);
      setRenamingSessionId(null);
      setConfirmDeleteSessionId(null);
      setConfirmClear(false);
      setCommandOutput(null);
      window.setTimeout(() => textareaRef.current?.focus(), 20);
    } catch (err) {
      setSessionError(err instanceof Error ? err.message : String(err));
    } finally {
      setSessionBusy(false);
    }
  };

  const handleSaveRenameSession = async (sessionId: string) => {
    if (!onRenameSession || sessionBusy) return;
    const clean = renameDraft.trim();
    if (!clean) {
      setSessionError("Tên cuộc trò chuyện không được để trống.");
      return;
    }
    setSessionBusy(true);
    setSessionError(null);
    try {
      await onRenameSession(sessionId, clean);
      setRenamingSessionId(null);
      setRenameDraft("");
    } catch (err) {
      setSessionError(err instanceof Error ? err.message : String(err));
    } finally {
      setSessionBusy(false);
    }
  };

  const handleDeleteSessionClick = async (sessionId: string) => {
    if (!onDeleteSession || sessionBusy) return;
    if (isStreaming && sessionId === activeSessionId) return;
    setSessionBusy(true);
    setSessionError(null);
    try {
      await onDeleteSession(sessionId);
      setConfirmDeleteSessionId(null);
      if (renamingSessionId === sessionId) {
        setRenamingSessionId(null);
      }
    } catch (err) {
      setSessionError(err instanceof Error ? err.message : String(err));
    } finally {
      setSessionBusy(false);
    }
  };

  const executeSlashAction = async (action: AiSlashCommandAction) => {
    commandReqSeqRef.current += 1;
    setCommandRunning(null);
    setDraft("");
    setSlashMenuDismissed(false);
    setSlashSelectedIndex(0);
    setConfirmClear(false);

    switch (action.kind) {
      case "new":
        await handleCreateNewSession();
        return;
      case "clear":
        setCommandOutput(null);
        onClearHistory();
        return;
      case "model":
        setSessionPickerOpen(false);
        if (onQuickSwitchConfig) {
          setPickerOpen(true);
        } else {
          onOpenSettings();
        }
        return;
      case "help":
        setCommandOutput({
          command: "help",
          title: "Lệnh nhanh Trợ lý AI",
          output: formatAiHelpMarkdown(config?.backend !== "cloud_api"),
          ok: true,
        });
        return;
      case "zeroclaw": {
        if (config?.backend !== "zeroclaw") {
          setCommandOutput({
            command: action.command,
            title: "Lệnh dành riêng cho ZeroClaw",
            output:
              "Các lệnh `/memory`, `/skills`, `/status` chỉ hoạt động khi Trợ lý AI đang dùng chế độ **ZeroClaw (trên máy)**.\n\nBạn có thể bấm nút chọn mẫu AI trên thanh tiêu đề (hoặc gõ `/model`) để chuyển sang **ZeroClaw**.",
            ok: false,
            showSwitchModelHint: true,
          });
          return;
        }
        if (relayBlocksChat) {
          setCommandOutput({
            command: action.command,
            title: "ZeroClaw đang tạm khóa",
            output:
              "Máy đang kết nối qua mạng nội bộ công ty nên các lệnh ZeroClaw tạm thời không khả dụng.",
            ok: false,
          });
          return;
        }
        if (!onRunCommand) {
          setCommandOutput({
            command: action.command,
            title: "Chưa hỗ trợ lệnh này",
            output: "Tính năng chạy lệnh ZeroClaw chưa được kết nối trong phiên làm việc hiện tại.",
            ok: false,
          });
          return;
        }
        const reqSeq = ++commandReqSeqRef.current;
        const labelMap: Record<typeof action.command, string> = {
          memory: "Đang đọc trí nhớ ZeroClaw...",
          memory_clear: "Đang xóa trí nhớ ZeroClaw...",
          skills: "Đang kiểm tra kỹ năng ZeroClaw...",
          status: "Đang kiểm tra trạng thái ZeroClaw...",
        };
        setCommandRunning(labelMap[action.command]);
        setCommandOutput(null);
        try {
          const result = await onRunCommand(action.command);
          if (reqSeq !== commandReqSeqRef.current) return;
          setCommandOutput(result);
        } catch (err) {
          if (reqSeq !== commandReqSeqRef.current) return;
          setCommandOutput({
            command: action.command,
            title: "Không thể chạy lệnh ZeroClaw",
            output: err instanceof Error ? err.message : String(err),
            ok: false,
          });
        } finally {
          if (reqSeq === commandReqSeqRef.current) {
            setCommandRunning(null);
          }
        }
        return;
      }
    }
  };

  const handleSelectSlashSuggestion = (item: AiSlashCommandDef) => {
    if (!enabled || isStreaming) return;
    void executeSlashAction(item.action);
  };

  const handleSubmit = () => {
    if (!enabled || isStreaming) return;
    const clean = draft.trim();
    if (!clean) return;
    const slash = parseAiSlashCommand(clean);
    if (slash) {
      void executeSlashAction(slash);
      return;
    }
    if (relayBlocksChat) return;
    setDraft("");
    setSlashMenuDismissed(false);
    setSlashSelectedIndex(0);
    setConfirmClear(false);
    onSend(clean);
  };

  const handleComposerKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (slashSuggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashSelectedIndex((prev) => (prev + 1) % slashSuggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashSelectedIndex(
          (prev) => (prev - 1 + slashSuggestions.length) % slashSuggestions.length,
        );
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setSlashMenuDismissed(true);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        const chosen = slashSuggestions[activeSlashIndex];
        if (chosen) {
          setDraft(chosen.cmd);
          setSlashSelectedIndex(0);
        }
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        const chosen = slashSuggestions[activeSlashIndex];
        if (chosen) {
          void executeSlashAction(chosen.action);
          return;
        }
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSubmit();
    }
  };

  const handleChipClick = (chip: StarterChip) => {
    if (!enabled || isStreaming || relayBlocksChat) return;
    if (chip.fillOnly) {
      setDraft(chip.prompt);
      setSlashMenuDismissed(false);
      window.setTimeout(() => {
        const ta = textareaRef.current;
        if (ta) {
          ta.focus();
          ta.setSelectionRange(chip.prompt.length, chip.prompt.length);
        }
      }, 10);
      return;
    }
    onSend(chip.prompt);
  };

  return (
    <div
      className="flex h-full min-h-0 w-full flex-1 flex-col overflow-hidden"
      style={{ background: "var(--bg-stage)", color: "var(--text-primary)" }}
    >
      {/* Top Bar */}
      <header
        className="flex shrink-0 items-center justify-between gap-3 px-5 py-3"
        style={{
          background: "var(--bg-sidebar)",
          borderBottom: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex min-w-0 items-center gap-3">
          <div
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl shadow-sm"
            style={{
              background: "var(--accent-soft)",
              border: "1px solid var(--border-subtle)",
              color: "var(--accent)",
            }}
          >
            <Sparkles className="h-5 w-5" />
          </div>
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="truncate text-[15px] font-semibold">Trợ lý AI Teza</h2>
              {onQuickSwitchConfig ? (
                <div ref={pickerRef} className="relative">
                  <button
                    type="button"
                    disabled={isStreaming}
                    onClick={() => {
                      setSessionPickerOpen(false);
                      setPickerOpen((v) => !v);
                    }}
                    className="inline-flex max-w-[300px] items-center gap-1 rounded-full px-2.5 py-0.5 text-[11px] font-medium transition hover:opacity-90 disabled:opacity-60"
                    style={{
                      background: enabled ? "var(--accent-soft)" : "var(--chip-bg)",
                      color: enabled ? "var(--accent)" : "var(--text-secondary)",
                      border: "1px solid var(--chip-border)",
                    }}
                    title="Đổi nhanh chế độ chạy hoặc mẫu AI"
                  >
                    {enabled ? (
                      <>
                        <Bot className="h-3 w-3 shrink-0" />
                        <span className="truncate">
                          {backendLabel}
                          {shortModelLabel ? ` · ${shortModelLabel}` : ""}
                        </span>
                      </>
                    ) : (
                      <span>Chọn nguồn AI</span>
                    )}
                    <ChevronDown className="h-3 w-3 shrink-0 opacity-80" />
                  </button>

                  {pickerOpen &&
                    pickerPos &&
                    typeof document !== "undefined" &&
                    createPortal(
                      <div
                        ref={pickerPopoverRef}
                        data-teza-floating-popup="true"
                        className="fixed z-[75] overflow-y-auto rounded-2xl p-3 shadow-2xl"
                        style={{
                          left: `${pickerPos.left}px`,
                          top: `${pickerPos.top}px`,
                          width: `${pickerPos.width}px`,
                          maxHeight: `${pickerPos.maxHeight}px`,
                          background: "var(--bg-sidebar)",
                          border: "1px solid var(--border-subtle)",
                          color: "var(--text-primary)",
                        }}
                      >
                      {/* Execution mode toggle: ZeroClaw (local) vs direct Cloud API */}
                      <div className="space-y-1.5">
                        <div
                          className="px-1 text-[10.5px] font-semibold uppercase tracking-wider"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          Chế độ chạy
                        </div>
                        <div className="space-y-1">
                          <button
                            type="button"
                            disabled={pickerBusy || relayEnabled}
                            onClick={() => void handleQuickSelectBackend("zeroclaw")}
                            className="flex w-full items-center justify-between gap-2 rounded-xl px-2.5 py-2 text-left text-xs font-medium transition disabled:cursor-not-allowed disabled:opacity-50"
                            style={{
                              background:
                                enabled && config?.backend === "zeroclaw"
                                  ? "var(--accent-soft)"
                                  : "var(--bg-surface)",
                              border:
                                enabled && config?.backend === "zeroclaw"
                                  ? "1px solid var(--accent)"
                                  : "1px solid var(--border-subtle)",
                            }}
                          >
                            <span className="flex items-center gap-2">
                              <Cpu
                                className="h-3.5 w-3.5 shrink-0"
                                style={{ color: "var(--accent)" }}
                              />
                              <span>
                                ZeroClaw (trên máy)
                                {relayEnabled ? " · tạm khóa mạng công ty" : ""}
                              </span>
                            </span>
                            {enabled && config?.backend === "zeroclaw" && (
                              <Check
                                className="h-3.5 w-3.5 shrink-0"
                                style={{ color: "var(--accent)" }}
                              />
                            )}
                          </button>

                          <button
                            type="button"
                            disabled={pickerBusy}
                            onClick={() => void handleQuickSelectBackend("cloud_api")}
                            className="flex w-full items-center justify-between gap-2 rounded-xl px-2.5 py-2 text-left text-xs font-medium transition disabled:cursor-not-allowed disabled:opacity-50"
                            style={{
                              background:
                                enabled && config?.backend === "cloud_api"
                                  ? "var(--accent-soft)"
                                  : "var(--bg-surface)",
                              border:
                                enabled && config?.backend === "cloud_api"
                                  ? "1px solid var(--accent)"
                                  : "1px solid var(--border-subtle)",
                            }}
                          >
                            <span className="flex items-center gap-2">
                              <KeyRound
                                className="h-3.5 w-3.5 shrink-0"
                                style={{ color: "var(--accent)" }}
                              />
                              <span>Gọi trực tiếp khóa API</span>
                            </span>
                            {enabled && config?.backend === "cloud_api" && (
                              <Check
                                className="h-3.5 w-3.5 shrink-0"
                                style={{ color: "var(--accent)" }}
                              />
                            )}
                          </button>
                        </div>
                      </div>

                      {/* Provider tabs + live/free model selector (works for both ZeroClaw and Cloud API) */}
                      <div
                        className="mt-3 space-y-2 border-t pt-2.5"
                        style={{ borderColor: "var(--border-subtle)" }}
                      >
                        <div className="flex items-center justify-between gap-2 px-1">
                          <span
                            className="text-[10.5px] font-semibold uppercase tracking-wider"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            Dịch vụ &amp; mẫu AI
                          </span>
                          {canScanPickerModels && (
                            <button
                              type="button"
                              disabled={pickerModelsLoading}
                              onClick={() => void loadPickerModels(activePickerPreset.baseUrl, true)}
                              className="inline-flex items-center gap-1 text-[11px] font-medium hover:underline disabled:opacity-55"
                              style={{ color: "var(--accent)" }}
                            >
                              {pickerModelsLoading ? (
                                <Loader2 className="h-3 w-3 animate-spin" />
                              ) : (
                                <RefreshCw className="h-3 w-3" />
                              )}
                              <span>Tải mới</span>
                            </button>
                          )}
                        </div>

                        <div className="grid grid-cols-3 gap-1">
                          {AI_CLOUD_PROVIDERS.map((prov) => {
                            const activeTab = pickerProviderId === prov.id;
                            return (
                              <button
                                key={prov.id}
                                type="button"
                                onClick={() => {
                                  pickerReqSeqRef.current += 1;
                                  setPickerModelsLoading(false);
                                  setPickerProviderId(prov.id);
                                }}
                                className="rounded-lg px-2 py-1.5 text-[11px] font-semibold transition"
                                style={{
                                  background: activeTab ? "var(--accent)" : "var(--bg-surface)",
                                  color: activeTab ? "var(--text-on-accent)" : "var(--text-primary)",
                                  border: activeTab
                                    ? "1px solid var(--accent)"
                                    : "1px solid var(--border-subtle)",
                                }}
                              >
                                {prov.name}
                              </button>
                            );
                          })}
                        </div>

                        {pickerModels.length > 8 && (
                          <input
                            type="text"
                            value={pickerFilter}
                            onChange={(e) => setPickerFilter(e.target.value)}
                            placeholder="Tìm nhanh mẫu (llama, grok, free)..."
                            className="w-full rounded-lg px-2.5 py-1.5 text-[11.5px] outline-none"
                            style={{
                              background: "var(--bg-input)",
                              border: "1px solid var(--border-subtle)",
                              color: "var(--text-primary)",
                            }}
                          />
                        )}

                        <div className="max-h-48 space-y-1 overflow-y-auto pr-0.5">
                          {pickerModelsLoading && pickerModels.length === 0 && (
                            <div
                              className="flex items-center gap-1.5 px-2 py-1 text-[11px]"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              <Loader2 className="h-3 w-3 animate-spin" />
                              <span>Đang quét danh sách mẫu mới nhất...</span>
                            </div>
                          )}
                          {pickerDisplayModels.map((item) => {
                            const isCurrent =
                              enabled &&
                              Boolean(config) &&
                              isSameCloudApiHost(
                                config!.api_base_url,
                                activePickerPreset.baseUrl,
                              ) &&
                              config!.model === item.id;
                            return (
                              <button
                                key={item.id}
                                type="button"
                                disabled={pickerBusy}
                                onClick={() => void handleQuickSelectCloudModel(item.id)}
                                className="flex w-full items-center justify-between gap-2 rounded-lg px-2.5 py-1.5 text-left text-[11.5px] transition hover:opacity-95 disabled:opacity-55"
                                style={{
                                  background: isCurrent ? "var(--accent-soft)" : "var(--bg-surface)",
                                  border: isCurrent
                                    ? "1px solid var(--accent)"
                                    : "1px solid transparent",
                                }}
                                title={item.id}
                              >
                                <span className="min-w-0 flex-1 truncate font-medium">
                                  {item.label}
                                </span>
                                <span className="flex shrink-0 items-center gap-1">
                                  {item.isFree && (
                                    <span
                                      className="rounded-full px-1.5 py-0.5 text-[9.5px] font-semibold"
                                      style={{
                                        background: "rgba(16,185,129,0.16)",
                                        color: "#34d399",
                                      }}
                                    >
                                      Miễn phí
                                    </span>
                                  )}
                                  {isCurrent && (
                                    <Check
                                      className="h-3.5 w-3.5"
                                      style={{ color: "var(--accent)" }}
                                    />
                                  )}
                                </span>
                              </button>
                            );
                          })}
                        </div>

                        {!hasSavedKeyForPickerProvider && (
                          <div
                            className="flex items-center justify-between gap-2 rounded-xl px-2.5 py-2 text-[11px]"
                            style={{
                              background: "var(--bg-surface)",
                              border: "1px solid var(--border-subtle)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            <span>
                              {pickerNotice ||
                                `Để dùng ${activePickerPreset.name}, hãy dán khóa API trong Cài đặt AI.`}
                            </span>
                            <button
                              type="button"
                              onClick={() => {
                                setPickerOpen(false);
                                onOpenSettings();
                              }}
                              className="shrink-0 font-semibold underline"
                              style={{ color: "var(--accent)" }}
                            >
                              Mở Cài đặt AI
                            </button>
                          </div>
                        )}

                        {hasSavedKeyForPickerProvider && pickerNotice && (
                          <p
                            className="px-1 text-[11px] leading-relaxed"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            {pickerNotice}
                          </p>
                        )}
                      </div>
                    </div>,
                      document.body,
                    )}
                </div>
              ) : (
                <span
                  className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium"
                  style={{
                    background: enabled ? "var(--accent-soft)" : "var(--chip-bg)",
                    color: enabled ? "var(--accent)" : "var(--text-secondary)",
                    border: "1px solid var(--chip-border)",
                  }}
                >
                  {enabled ? (
                    <>
                      <Bot className="h-3 w-3" />
                      <span>
                        {backendLabel}
                        {shortModelLabel ? ` · ${shortModelLabel}` : ""}
                      </span>
                    </>
                  ) : (
                    <span>Chưa bật</span>
                  )}
                </span>
              )}

              {/* Multi-Session Switcher Pill & Popover */}
              {enabled && hasSessionControls && (
                <div ref={sessionPickerRef} className="relative">
                  <button
                    type="button"
                    disabled={isStreaming || sessionBusy}
                    onClick={() => {
                      setPickerOpen(false);
                      setSessionError(null);
                      setRenamingSessionId(null);
                      setConfirmDeleteSessionId(null);
                      setSessionPickerOpen((v) => !v);
                    }}
                    className="hover-menu inline-flex max-w-[240px] items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11px] font-medium transition disabled:cursor-not-allowed disabled:opacity-55"
                    style={{
                      background: "var(--bg-surface)",
                      color: "var(--text-primary)",
                      border: sessionPickerOpen
                        ? "1px solid var(--accent)"
                        : "1px solid var(--border-subtle)",
                    }}
                    title="Chuyển đổi hoặc quản lý các cuộc trò chuyện AI"
                  >
                    <MessageSquare
                      className="h-3 w-3 shrink-0"
                      style={{ color: "var(--accent)" }}
                    />
                    <span className="truncate">{activeSessionTitle}</span>
                    {sessions.length > 0 && (
                      <span
                        className="shrink-0 rounded-full px-1.5 py-px text-[10px] font-semibold tabular-nums"
                        style={{
                          background: "var(--chip-bg)",
                          color: "var(--text-secondary)",
                        }}
                      >
                        {sessions.length}
                      </span>
                    )}
                    <ChevronDown className="h-3 w-3 shrink-0 opacity-75" />
                  </button>

                  {sessionPickerOpen &&
                    sessionPickerPos &&
                    typeof document !== "undefined" &&
                    createPortal(
                      <div
                        ref={sessionPickerPopoverRef}
                        data-teza-floating-popup="true"
                        className="fixed z-[75] overflow-y-auto rounded-2xl p-2.5 shadow-2xl"
                        style={{
                          left: `${sessionPickerPos.left}px`,
                          top: `${sessionPickerPos.top}px`,
                          width: `${sessionPickerPos.width}px`,
                          maxHeight: `${sessionPickerPos.maxHeight}px`,
                          background: "var(--bg-sidebar)",
                          border: "1px solid var(--border-subtle)",
                          color: "var(--text-primary)",
                        }}
                      >
                      {onCreateSession && (
                        <button
                          type="button"
                          disabled={isStreaming || sessionBusy}
                          onClick={() => void handleCreateNewSession()}
                          className="tf-btn flex w-full items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:cursor-not-allowed disabled:opacity-55"
                        >
                          <Plus className="h-3.5 w-3.5" />
                          <span>Cuộc trò chuyện mới</span>
                        </button>
                      )}

                      <div
                        className={`${onCreateSession ? "mt-2.5 border-t pt-2" : ""} flex items-center justify-between px-1 pb-1.5`}
                        style={{ borderColor: "var(--border-subtle)" }}
                      >
                        <span
                          className="text-[10.5px] font-semibold uppercase tracking-wider"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          Các cuộc trò chuyện ({sessions.length})
                        </span>
                      </div>

                      {sessionError && (
                        <div
                          className="mb-2 rounded-xl px-2.5 py-1.5 text-[11px]"
                          style={{
                            background: "var(--danger-bg)",
                            color: "var(--danger-text)",
                          }}
                        >
                          {sessionError}
                        </div>
                      )}

                      {sessions.length === 0 ? (
                        <div
                          className="rounded-xl px-3 py-4 text-center text-xs"
                          style={{
                            background: "var(--bg-surface)",
                            color: "var(--text-secondary)",
                          }}
                        >
                          Chưa có cuộc trò chuyện nào được lưu. Gửi tin nhắn đầu tiên hoặc bấm tạo
                          mới ở trên.
                        </div>
                      ) : (
                        <div className="max-h-72 space-y-1 overflow-y-auto pr-0.5">
                          {sessions.map((session) => {
                            const isActive = session.id === activeSessionId;
                            const isRenaming = renamingSessionId === session.id;
                            const isConfirmingDelete = confirmDeleteSessionId === session.id;
                            const timeText = formatSessionRelativeTime(session.updated_ts);

                            return (
                              <div
                                key={session.id}
                                className="rounded-xl p-2 transition"
                                style={{
                                  background: isActive ? "var(--accent-soft)" : "var(--bg-surface)",
                                  border: isActive
                                    ? "1px solid var(--accent)"
                                    : "1px solid var(--border-subtle)",
                                }}
                              >
                                {isRenaming ? (
                                  <div className="flex items-center gap-1.5">
                                    <input
                                      type="text"
                                      autoFocus
                                      maxLength={120}
                                      value={renameDraft}
                                      onChange={(e) => setRenameDraft(e.target.value)}
                                      onKeyDown={(e) => {
                                        if (e.key === "Enter") {
                                          e.preventDefault();
                                          void handleSaveRenameSession(session.id);
                                        } else if (e.key === "Escape") {
                                          e.preventDefault();
                                          setRenamingSessionId(null);
                                          setSessionError(null);
                                        }
                                      }}
                                      placeholder="Nhập tên cuộc trò chuyện..."
                                      className="min-w-0 flex-1 rounded-lg px-2 py-1 text-xs outline-none"
                                      style={{
                                        background: "var(--bg-input)",
                                        border: "1px solid var(--accent)",
                                        color: "var(--text-primary)",
                                      }}
                                    />
                                    <button
                                      type="button"
                                      disabled={sessionBusy}
                                      onClick={() => void handleSaveRenameSession(session.id)}
                                      className="rounded-lg p-1.5 transition hover:opacity-90 disabled:opacity-50"
                                      style={{
                                        background: "var(--accent)",
                                        color: "var(--text-on-accent)",
                                      }}
                                      title="Lưu tên mới"
                                    >
                                      <Check className="h-3.5 w-3.5" />
                                    </button>
                                    <button
                                      type="button"
                                      disabled={sessionBusy}
                                      onClick={() => {
                                        setRenamingSessionId(null);
                                        setSessionError(null);
                                      }}
                                      className="hover-menu rounded-lg p-1.5 transition"
                                      style={{ color: "var(--text-secondary)" }}
                                      title="Hủy đổi tên"
                                    >
                                      <X className="h-3.5 w-3.5" />
                                    </button>
                                  </div>
                                ) : isConfirmingDelete ? (
                                  <div className="flex items-center justify-between gap-2 py-0.5 text-xs">
                                    <span
                                      className="min-w-0 truncate font-medium"
                                      style={{ color: "var(--danger-text)" }}
                                    >
                                      Xóa "{session.title || "Cuộc trò chuyện"}"?
                                    </span>
                                    <div className="flex shrink-0 items-center gap-1">
                                      <button
                                        type="button"
                                        disabled={sessionBusy}
                                        onClick={() => setConfirmDeleteSessionId(null)}
                                        className="rounded-lg px-2 py-1 text-[11px] font-medium hover:opacity-85"
                                        style={{
                                          background: "var(--bg-sidebar)",
                                          color: "var(--text-primary)",
                                        }}
                                      >
                                        Hủy
                                      </button>
                                      <button
                                        type="button"
                                        disabled={sessionBusy}
                                        onClick={() => void handleDeleteSessionClick(session.id)}
                                        className="rounded-lg bg-rose-600 px-2.5 py-1 text-[11px] font-semibold text-white hover:bg-rose-500 disabled:opacity-50"
                                      >
                                        Xóa
                                      </button>
                                    </div>
                                  </div>
                                ) : (
                                  <div className="flex items-start justify-between gap-2">
                                    <button
                                      type="button"
                                      disabled={isStreaming || sessionBusy}
                                      onClick={() => {
                                        setSessionPickerOpen(false);
                                        setConfirmDeleteSessionId(null);
                                        setRenamingSessionId(null);
                                        onSelectSession?.(session.id);
                                      }}
                                      className="min-w-0 flex-1 text-left disabled:cursor-not-allowed"
                                    >
                                      <div className="flex items-center gap-1.5">
                                        <span className="truncate text-xs font-semibold">
                                          {session.title || "Cuộc trò chuyện mới"}
                                        </span>
                                        {isActive && (
                                          <Check
                                            className="h-3.5 w-3.5 shrink-0"
                                            style={{ color: "var(--accent)" }}
                                          />
                                        )}
                                      </div>
                                      <div
                                        className="mt-0.5 flex items-center gap-1.5 text-[10.5px]"
                                        style={{ color: "var(--text-secondary)" }}
                                      >
                                        {timeText && <span>{timeText}</span>}
                                        {timeText && <span>·</span>}
                                        <span>{session.message_count} tin</span>
                                      </div>
                                      {session.last_preview && (
                                        <p
                                          className="mt-0.5 truncate text-[11px]"
                                          style={{ color: "var(--text-secondary)" }}
                                        >
                                          {session.last_preview}
                                        </p>
                                      )}
                                    </button>

                                    {(onRenameSession || onDeleteSession) && (
                                      <div className="flex shrink-0 items-center gap-0.5 pt-0.5">
                                        {onRenameSession && (
                                          <button
                                            type="button"
                                            disabled={sessionBusy}
                                            onClick={(e) => {
                                              e.stopPropagation();
                                              setConfirmDeleteSessionId(null);
                                              setSessionError(null);
                                              setRenamingSessionId(session.id);
                                              setRenameDraft(session.title || "");
                                            }}
                                            className="hover-menu rounded-lg p-1 opacity-75 transition hover:opacity-100 disabled:opacity-40"
                                            style={{ color: "var(--text-secondary)" }}
                                            title="Đổi tên cuộc trò chuyện"
                                          >
                                            <Pencil className="h-3.5 w-3.5" />
                                          </button>
                                        )}
                                        {onDeleteSession && (
                                          <button
                                            type="button"
                                            disabled={sessionBusy || (isStreaming && isActive)}
                                            onClick={(e) => {
                                              e.stopPropagation();
                                              setRenamingSessionId(null);
                                              setSessionError(null);
                                              setConfirmDeleteSessionId(session.id);
                                            }}
                                            className="rounded-lg p-1 text-rose-400 opacity-75 transition hover:bg-rose-500/10 hover:opacity-100 disabled:opacity-40"
                                            title="Xóa cuộc trò chuyện"
                                          >
                                            <Trash2 className="h-3.5 w-3.5" />
                                          </button>
                                        )}
                                      </div>
                                    )}
                                  </div>
                                )}
                              </div>
                            );
                          })}
                        </div>
                      )}
                    </div>,
                      document.body,
                    )}
                </div>
              )}

              {/* 2-Axis Permission Status Pills (clickable to open AI Settings) */}
              {showPermissionPills && (
                <button
                  type="button"
                  onClick={onOpenSettings}
                  className="inline-flex max-w-full flex-wrap items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold transition hover:opacity-90"
                  style={accessPillStyle(accessBadge.tone)}
                  title={tr(
                    "aiPermPillTooltip",
                    "Bấm để mở Cài đặt phân quyền 2 trục cho ZeroClaw",
                  )}
                >
                  {accessBadge.isHighRisk ? (
                    <ShieldAlert className="h-3 w-3 shrink-0" />
                  ) : (
                    <Shield className="h-3 w-3 shrink-0" />
                  )}
                  <span className="truncate">
                    Teza: {formatTezaAccessShortLabel(effectiveTezaLevel, lang)}
                  </span>
                  <span className="opacity-65">·</span>
                  <span className="truncate">
                    Windows: {formatSystemAccessShortLabel(effectiveSystemLevel, lang)}
                  </span>
                </button>
              )}
            </div>
            <p className="truncate text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
              Trợ lý tùy chọn · Lịch sử mã hóa cục bộ trên thiết bị
            </p>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          {showEstopButton && (
            <button
              type="button"
              disabled={estopBusy}
              onClick={() => void handleEmergencyStopClick()}
              className="inline-flex items-center gap-1 rounded-xl bg-rose-600 px-2.5 py-1.5 text-xs font-semibold text-white shadow-xs transition hover:bg-rose-500 disabled:cursor-not-allowed disabled:opacity-60"
              title={tr(
                "aiEstopTooltip",
                "Dừng khẩn cấp: hủy ngay lượt AI đang chạy, từ chối thẻ chờ duyệt và diệt mọi tiến trình PowerShell con",
              )}
            >
              {estopBusy ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <OctagonX className="h-3.5 w-3.5" />
              )}
              <span>
                {estopBusy
                  ? tr("aiEstopStopping", "Đang dừng khẩn cấp...")
                  : tr("aiEstopBtn", "E-Stop")}
              </span>
            </button>
          )}
          {enabled && onCreateSession && (
            <button
              type="button"
              disabled={isStreaming || sessionBusy}
              onClick={() => void handleCreateNewSession()}
              className="hover-menu inline-flex items-center gap-1 rounded-xl px-2.5 py-1.5 text-xs font-semibold transition disabled:cursor-not-allowed disabled:opacity-45"
              style={{
                background: "var(--accent-soft)",
                border: "1px solid var(--chip-border)",
                color: "var(--accent)",
              }}
              title="Tạo cuộc trò chuyện AI mới (/new)"
            >
              <Plus className="h-3.5 w-3.5" />
              <span>Mới</span>
            </button>
          )}
          {enabled && messages.length > 0 && (
            <button
              type="button"
              disabled={isStreaming}
              onClick={() => setConfirmClear((v) => !v)}
              className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-medium transition disabled:opacity-45"
              style={{
                border: "1px solid var(--border-subtle)",
                color: "var(--text-secondary)",
              }}
              title="Xóa lịch sử trò chuyện AI"
            >
              <Trash2 className="h-3.5 w-3.5" />
              <span className="hidden sm:inline">Xóa chat</span>
            </button>
          )}
          <button
            type="button"
            onClick={onOpenSettings}
            className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-2.5 py-1.5 text-xs font-medium transition"
            style={{
              border: "1px solid var(--border-subtle)",
              color: "var(--accent)",
            }}
            title="Mở Cài đặt AI"
          >
            <SlidersHorizontal className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">Cài đặt AI</span>
          </button>
          {onClose && (
            <button
              type="button"
              onClick={onClose}
              className="hover-menu rounded-xl p-1.5 transition"
              style={{ color: "var(--text-secondary)" }}
              title="Đóng Trợ lý AI"
            >
              <X className="h-4 w-4" />
            </button>
          )}
        </div>
      </header>

      {/* Clear history inline confirmation bar */}
      {confirmClear && (
        <div
          className="flex shrink-0 flex-wrap items-center justify-between gap-2 px-4 py-2.5 text-xs"
          style={{
            background: "var(--danger-bg)",
            borderBottom: "1px solid var(--border-subtle)",
            color: "var(--danger-text)",
          }}
        >
          <span className="font-medium">
            Xác nhận xóa toàn bộ tin nhắn và bộ nhớ của cuộc trò chuyện này?
          </span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={() => setConfirmClear(false)}
              className="rounded-lg px-2.5 py-1 font-medium hover:opacity-85"
              style={{ background: "var(--bg-surface)", color: "var(--text-primary)" }}
            >
              Hủy
            </button>
            <button
              type="button"
              onClick={() => {
                commandReqSeqRef.current += 1;
                setCommandRunning(null);
                setCommandOutput(null);
                setConfirmClear(false);
                onClearHistory();
              }}
              className="rounded-lg bg-rose-600 px-3 py-1 font-semibold text-white hover:bg-rose-500"
            >
              Xóa ngay
            </button>
          </div>
        </div>
      )}

      {/* Body */}
      {enabled && relayBlocksChat && (
        <div
          className="mx-5 mt-3 flex items-center justify-between gap-2 rounded-xl px-3.5 py-2.5 text-xs"
          style={{
            background: "rgba(239, 68, 68, 0.12)",
            border: "1px solid rgba(239, 68, 68, 0.35)",
            color: "var(--text-primary)",
          }}
        >
          <span className="flex items-center gap-2">
            <AlertTriangle className="h-4 w-4 shrink-0 text-rose-500" />
            <span>
              Trợ lý AI ZeroClaw tạm khóa khi máy đang dùng <strong>mạng nội bộ công ty</strong>{" "}
              để đảm bảo bảo mật dữ liệu. Bạn có thể chuyển sang dùng khóa API trong Cài đặt AI.
            </span>
          </span>
        </div>
      )}
      {needsZeroClawKeyBanner && (
        <div
          className="mx-5 mt-3 flex flex-wrap items-center justify-between gap-2.5 rounded-xl px-3.5 py-2.5 text-xs"
          style={{
            background: "var(--accent-soft)",
            border: "1px solid var(--border-subtle)",
            color: "var(--text-primary)",
          }}
        >
          <span className="flex min-w-0 flex-1 items-center gap-2 leading-relaxed">
            <KeyRound className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <span>
              ZeroClaw cần liên kết khóa API (Groq, OpenRouter hoặc xAI Grok) để bắt đầu trả lời
              câu hỏi của bạn.
            </span>
          </span>
          <button
            type="button"
            onClick={onOpenSettings}
            className="tf-btn shrink-0 rounded-lg px-3 py-1.5 text-xs font-semibold"
          >
            Liên kết khóa API
          </button>
        </div>
      )}
      {!enabled ? (
        <div className="flex flex-1 items-center justify-center overflow-y-auto p-6">
          <div
            className="w-full max-w-lg space-y-4 rounded-2xl p-6 text-center shadow-lg"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div
              className="mx-auto flex h-12 w-12 items-center justify-center rounded-2xl"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
            >
              <Sparkles className="h-6 w-6" />
            </div>
            <div className="space-y-1.5">
              <h3 className="text-[16px] font-semibold">Bắt đầu với Trợ lý AI Teza</h3>
              <p className="text-[13px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                Trợ lý AI được <strong>tắt mặc định</strong> để giữ riêng tư. Chọn một trong hai
                cách dưới đây để dùng ngay:
              </p>
            </div>

            <div className="grid grid-cols-1 gap-2.5 text-left sm:grid-cols-2">
              <button
                type="button"
                disabled={quickInstalling}
                onClick={onOpenSettings}
                className="hover-surface flex flex-col items-start gap-1.5 rounded-xl p-3.5 transition disabled:opacity-60"
                style={{
                  background: "var(--card-bg)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <span
                  className="inline-flex items-center gap-1.5 text-[13px] font-semibold"
                  style={{ color: "var(--accent)" }}
                >
                  <KeyRound className="h-4 w-4 shrink-0" />
                  <span>Nhập khóa API (Groq / OpenRouter / Grok)</span>
                </span>
                <span className="text-[12px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  Chỉ cần dán khóa API là hỏi đáp được ngay, không cần cài thêm phần mềm.
                </span>
              </button>

              <button
                type="button"
                disabled={quickInstalling || relayEnabled}
                onClick={() => void handleQuickInstallZeroClaw()}
                className="hover-surface flex flex-col items-start gap-1.5 rounded-xl p-3.5 transition disabled:cursor-not-allowed disabled:opacity-55"
                style={{
                  background: "var(--card-bg)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <span
                  className="inline-flex items-center gap-1.5 text-[13px] font-semibold"
                  style={{ color: "var(--accent)" }}
                >
                  {quickInstalling ? (
                    <Loader2 className="h-4 w-4 shrink-0 animate-spin" />
                  ) : (
                    <Download className="h-4 w-4 shrink-0" />
                  )}
                  <span>
                    {quickInstalling ? "Đang cài ZeroClaw..." : "Tải & Cài đặt ZeroClaw"}
                  </span>
                </span>
                <span className="text-[12px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                  {relayEnabled
                    ? "Tạm khóa khi dùng mạng nội bộ công ty. Hãy dùng khóa API bên cạnh."
                    : "Tự tải & cài đặt trực tiếp trên máy tính chỉ với 1 nút bấm."}
                </span>
              </button>
            </div>

            {quickInstalling && (
              <div className="space-y-1.5 text-left">
                <div
                  className="flex items-center justify-between text-[11.5px]"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <span>{describeQuickInstallProgress(quickInstallProgress)}</span>
                  {quickInstallPct !== null && (
                    <span className="font-semibold tabular-nums">{quickInstallPct}%</span>
                  )}
                </div>
                <div
                  className="h-2 w-full overflow-hidden rounded-full"
                  style={{ background: "var(--bg-sidebar)" }}
                >
                  <div
                    className="h-full rounded-full transition-all duration-200"
                    style={{
                      width: `${quickInstallPct ?? 18}%`,
                      background: "var(--accent)",
                    }}
                  />
                </div>
              </div>
            )}

            {quickInstallError && (
              <div
                className="rounded-xl px-3 py-2 text-left text-xs"
                style={{
                  background: "var(--danger-bg)",
                  color: "var(--danger-text)",
                }}
              >
                {quickInstallError}
              </div>
            )}

            <div
              className="flex items-start gap-2 rounded-xl px-3.5 py-2.5 text-left text-[12px]"
              style={{
                background: "var(--card-bg)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-secondary)",
              }}
            >
              <Lock className="mt-0.5 h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
              <span>
                Lịch sử trò chuyện với Trợ lý chỉ lưu bảo mật trên máy tính của bạn và chỉ hoạt động
                khi bạn chủ động đặt câu hỏi hoặc bấm tóm tắt.
              </span>
            </div>
          </div>
        </div>
      ) : (
        <>
          {/* Conversation Scroll Area */}
          <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
            <div className="flex w-full flex-col gap-3">
              {messages.length === 0 && !isStreaming && (
                <div className="mx-auto my-8 flex w-full max-w-xl flex-col items-center text-center">
                  <div
                    className="mb-3 flex h-12 w-12 items-center justify-center rounded-2xl"
                    style={{
                      background: "var(--accent-soft)",
                      color: "var(--accent)",
                      border: "1px solid var(--border-subtle)",
                    }}
                  >
                    <Sparkles className="h-6 w-6" />
                  </div>
                  <h3 className="text-[16px] font-semibold">Tôi có thể giúp gì cho bạn hôm nay?</h3>
                  <p
                    className="mt-1 max-w-md text-[12.5px]"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    Soạn thảo tin nhắn, dịch nhanh, tóm tắt văn bản hoặc giải đáp câu hỏi ngay
                    trong không gian làm việc Teza.
                  </p>

                  <div className="mt-5 flex flex-wrap items-center justify-center gap-2">
                    {STARTER_CHIPS.map((chip) => (
                      <button
                        key={chip.id}
                        type="button"
                        onClick={() => handleChipClick(chip)}
                        className="hover-surface rounded-full px-3.5 py-1.5 text-[12.5px] font-medium transition"
                        style={{
                          background: "var(--chip-bg)",
                          border: "1px solid var(--chip-border)",
                          color: "var(--text-primary)",
                        }}
                      >
                        {chip.label}
                      </button>
                    ))}
                  </div>
                </div>
              )}

              {messages.map((msg) => {
                const isUser = msg.role === "user";
                const isCopied = copiedId === msg.id;
                return (
                  <div
                    key={msg.id}
                    className={`flex w-full ${isUser ? "justify-end" : "justify-start"}`}
                  >
                    <div
                      className={`group max-w-[78%] rounded-2xl px-3.5 py-2.5 text-[14px] shadow-xs ${
                        isUser ? "rounded-br-md" : "rounded-bl-md"
                      }`}
                      style={{
                        background: isUser ? "var(--bg-bubble-out)" : "var(--bg-bubble-in)",
                        border: `1px solid ${
                          isUser ? "var(--bubble-out-border)" : "var(--bubble-in-border)"
                        }`,
                        color: "var(--text-primary)",
                      }}
                    >
                      <div className="selectable-text wrap-anywhere">
                        {isUser ? (
                          <p className="whitespace-pre-wrap leading-relaxed">{msg.content}</p>
                        ) : (
                          <SafeMarkdownView
                            text={msg.content}
                            copiedCodeKey={copiedCodeKey}
                            onCopyCode={handleCopyCode}
                            msgKey={msg.id}
                          />
                        )}
                      </div>

                      <div
                        className="mt-1.5 flex items-center justify-between gap-3 text-[11px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <span className="truncate">
                          {formatLocalDateTime(msg.ts)}
                          {!isUser && msg.model ? ` · ${msg.model}` : ""}
                        </span>

                        {!isUser && (
                          <button
                            type="button"
                            onClick={() => handleCopyBubble(msg.id, msg.content)}
                            className="hover-menu inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] font-medium transition"
                            style={{ color: isCopied ? "#22c55e" : "var(--text-secondary)" }}
                            title="Sao chép câu trả lời"
                          >
                            {isCopied ? (
                              <Check className="h-3 w-3" />
                            ) : (
                              <Copy className="h-3 w-3" />
                            )}
                            <span>{isCopied ? "Đã chép" : "Sao chép"}</span>
                          </button>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}

              {/* Streaming assistant response */}
              {isStreaming && (
                <div className="flex w-full justify-start">
                  <div
                    className="max-w-[78%] rounded-2xl rounded-bl-md px-3.5 py-2.5 text-[14px] shadow-xs"
                    style={{
                      background: "var(--bg-bubble-in)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {/* Live Tool Activity Chips inside streaming turn */}
                    {currentSessionActivities.length > 0 && (
                      <div className="mb-2 flex flex-wrap gap-1.5">
                        {currentSessionActivities.map((act) => {
                          const label = formatToolDisplayName(act.tool_name, lang);
                          const borderStyle =
                            act.status === "failed"
                              ? "1px solid rgba(239, 68, 68, 0.4)"
                              : act.status === "completed"
                                ? "1px solid rgba(34, 197, 94, 0.35)"
                                : "1px solid var(--chip-border)";
                          const bgStyle =
                            act.status === "failed"
                              ? "rgba(239, 68, 68, 0.10)"
                              : act.status === "completed"
                                ? "rgba(34, 197, 94, 0.10)"
                                : "var(--chip-bg)";
                          return (
                            <span
                              key={act.tool_call_id}
                              className="inline-flex max-w-full items-center gap-1.5 rounded-full px-2.5 py-1 text-[11px] font-medium"
                              style={{
                                background: bgStyle,
                                border: borderStyle,
                                color: "var(--text-primary)",
                              }}
                              title={act.summary ? `${label} — ${act.summary}` : label}
                            >
                              {act.status === "pending" ? (
                                <Loader2
                                  className="h-3 w-3 shrink-0 animate-spin"
                                  style={{ color: "var(--accent)" }}
                                />
                              ) : act.status === "completed" ? (
                                <CheckCircle2 className="h-3 w-3 shrink-0 text-emerald-500" />
                              ) : (
                                <CircleX className="h-3 w-3 shrink-0 text-rose-500" />
                              )}
                              <span className="shrink-0 font-semibold">{label}</span>
                              {act.summary && (
                                <span
                                  className="max-w-[220px] truncate"
                                  style={{ color: "var(--text-secondary)" }}
                                >
                                  · {act.summary}
                                </span>
                              )}
                            </span>
                          );
                        })}
                      </div>
                    )}

                    {streamingText ? (
                      <div className="selectable-text wrap-anywhere">
                        <SafeMarkdownView
                          text={streamingText}
                          copiedCodeKey={copiedCodeKey}
                          onCopyCode={handleCopyCode}
                          msgKey="streaming"
                        />
                        <span
                          className="ml-1 inline-block h-3.5 w-1.5 animate-pulse rounded-xs align-middle"
                          style={{ background: "var(--accent)" }}
                        />
                      </div>
                    ) : (
                      <div
                        className="flex items-center gap-2 py-0.5 text-[13px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <Sparkles
                          className="h-3.5 w-3.5 animate-pulse"
                          style={{ color: "var(--accent)" }}
                        />
                        <span>Đang suy nghĩ...</span>
                      </div>
                    )}

                    {onCancelStream && (
                      <div className="mt-2 flex justify-end">
                        <button
                          type="button"
                          disabled={cancellingStream}
                          onClick={handleCancelStreamClick}
                          className="hover-menu inline-flex items-center gap-1 rounded-lg px-2 py-0.5 text-[11px] font-medium disabled:cursor-not-allowed disabled:opacity-60"
                          style={{
                            border: "1px solid var(--border-subtle)",
                            color: "var(--text-secondary)",
                          }}
                        >
                          {cancellingStream ? (
                            <Loader2 className="h-2.5 w-2.5 animate-spin" />
                          ) : (
                            <Square className="h-2.5 w-2.5 fill-current" />
                          )}
                          <span>{cancellingStream ? "Đang dừng..." : "Dừng tạo"}</span>
                        </button>
                      </div>
                    )}
                  </div>
                </div>
              )}

              {/* Tool Activity Chips retained when not streaming */}
              {!isStreaming && currentSessionActivities.length > 0 && (
                <div className="flex w-full justify-start">
                  <div
                    className="flex max-w-[78%] flex-wrap items-center gap-1.5 rounded-xl px-3 py-2"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                    }}
                  >
                    <span
                      className="text-[10.5px] font-semibold uppercase tracking-wide"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {tr("aiToolActivityTitle", "Công cụ đang chạy")}
                    </span>
                    {currentSessionActivities.map((act) => {
                      const label = formatToolDisplayName(act.tool_name, lang);
                      return (
                        <span
                          key={act.tool_call_id}
                          className="inline-flex max-w-full items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-medium"
                          style={{
                            background: "var(--chip-bg)",
                            border: "1px solid var(--chip-border)",
                            color: "var(--text-primary)",
                          }}
                          title={act.summary ? `${label} — ${act.summary}` : label}
                        >
                          {act.status === "pending" ? (
                            <Loader2
                              className="h-3 w-3 shrink-0 animate-spin"
                              style={{ color: "var(--accent)" }}
                            />
                          ) : act.status === "completed" ? (
                            <CheckCircle2 className="h-3 w-3 shrink-0 text-emerald-500" />
                          ) : (
                            <CircleX className="h-3 w-3 shrink-0 text-rose-500" />
                          )}
                          <span className="shrink-0 font-semibold">{label}</span>
                          {act.summary && (
                            <span
                              className="max-w-[200px] truncate"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              · {act.summary}
                            </span>
                          )}
                        </span>
                      );
                    })}
                  </div>
                </div>
              )}

              {/* Error banner */}
              {error && (
                <div
                  className="flex flex-wrap items-center justify-between gap-2 rounded-xl px-3.5 py-2.5 text-xs"
                  style={{
                    background: "var(--danger-bg)",
                    color: "var(--danger-text)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <div className="flex min-w-0 flex-1 items-start gap-2">
                    <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                    <span className="selectable-text wrap-anywhere font-medium leading-relaxed">
                      {error}
                    </span>
                  </div>
                  <button
                    type="button"
                    onClick={onOpenSettings}
                    className="shrink-0 rounded-lg px-2.5 py-1 font-semibold underline hover:opacity-85"
                  >
                    Kiểm tra Cài đặt AI
                  </button>
                </div>
              )}

              {/* Running slash command indicator */}
              {commandRunning && (
                <div
                  className="flex w-full items-center gap-2 rounded-2xl px-3.5 py-2.5 text-xs shadow-xs"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-secondary)",
                  }}
                >
                  <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" style={{ color: "var(--accent)" }} />
                  <span>{commandRunning}</span>
                </div>
              )}

              {/* Slash command output card */}
              {commandOutput && (
                <div
                  className="w-full rounded-2xl p-3.5 text-[13.5px] shadow-xs"
                  style={{
                    background: "var(--bg-surface)",
                    border: commandOutput.ok
                      ? "1px solid var(--border-subtle)"
                      : "1px solid rgba(245, 158, 11, 0.45)",
                    color: "var(--text-primary)",
                  }}
                >
                  <div
                    className="mb-2 flex items-center justify-between gap-2 border-b pb-2"
                    style={{ borderColor: "var(--border-subtle)" }}
                  >
                    <div className="flex min-w-0 items-center gap-2">
                      {commandOutput.ok ? (
                        <Terminal
                          className="h-4 w-4 shrink-0"
                          style={{ color: "var(--accent)" }}
                        />
                      ) : (
                        <AlertTriangle className="h-4 w-4 shrink-0 text-amber-500" />
                      )}
                      <span className="truncate text-xs font-semibold">{commandOutput.title}</span>
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      <button
                        type="button"
                        onClick={() => handleCopyBubble("__command__", commandOutput.output)}
                        className="hover-menu inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11px] font-medium transition"
                        style={{
                          color: copiedId === "__command__" ? "#22c55e" : "var(--text-secondary)",
                        }}
                        title="Sao chép kết quả"
                      >
                        {copiedId === "__command__" ? (
                          <Check className="h-3 w-3" />
                        ) : (
                          <Copy className="h-3 w-3" />
                        )}
                        <span>{copiedId === "__command__" ? "Đã chép" : "Sao chép"}</span>
                      </button>
                      <button
                        type="button"
                        onClick={() => setCommandOutput(null)}
                        className="hover-menu rounded-lg p-1 transition"
                        style={{ color: "var(--text-secondary)" }}
                        title="Đóng bảng kết quả"
                      >
                        <X className="h-3.5 w-3.5" />
                      </button>
                    </div>
                  </div>

                  <div className="selectable-text wrap-anywhere text-[13px]">
                    <SafeMarkdownView
                      text={commandOutput.output}
                      copiedCodeKey={copiedCodeKey}
                      onCopyCode={handleCopyCode}
                      msgKey="cmd-output"
                    />
                  </div>

                  {commandOutput.showSwitchModelHint && onQuickSwitchConfig && (
                    <div className="mt-2.5 flex justify-end">
                      <button
                        type="button"
                        onClick={() => {
                          setCommandOutput(null);
                          setSessionPickerOpen(false);
                          setPickerOpen(true);
                        }}
                        className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-semibold"
                        style={{
                          background: "var(--accent-soft)",
                          border: "1px solid var(--chip-border)",
                          color: "var(--accent)",
                        }}
                      >
                        <Cpu className="h-3.5 w-3.5" />
                        <span>Mở chọn chế độ AI</span>
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>

          {/* Pinned Interactive Action Approval Cards (right above composer) */}
          {pendingApprovals.length > 0 && (
            <div
              className="max-h-[45%] shrink-0 space-y-2.5 overflow-y-auto px-5 py-3"
              style={{
                background: "var(--bg-sidebar)",
                borderTop: "1px solid var(--border-subtle)",
              }}
            >
              {/* Notice when another AI session has pending approval(s) */}
              {otherSessionApprovals.length > 0 && firstOtherApprovalSession && (
                <div
                  className="flex flex-wrap items-center justify-between gap-2 rounded-xl px-3 py-2 text-xs"
                  style={{
                    background: "rgba(245, 158, 11, 0.12)",
                    border: "1px solid rgba(245, 158, 11, 0.38)",
                    color: "var(--text-primary)",
                  }}
                >
                  <span className="flex min-w-0 flex-1 items-center gap-2">
                    <AlertTriangle className="h-4 w-4 shrink-0 text-amber-500" />
                    <span className="truncate font-medium">
                      {tr(
                        "aiApprovalOtherSessionNotice",
                        "Có {count} yêu cầu duyệt đang chờ ở cuộc trò chuyện AI khác ({session}).",
                        {
                          count: otherSessionApprovals.length,
                          session: firstOtherApprovalSession.title,
                        },
                      )}
                    </span>
                  </span>
                  {onSelectSession && (
                    <button
                      type="button"
                      disabled={isStreaming}
                      onClick={() => onSelectSession(firstOtherApprovalSession.id)}
                      className="shrink-0 rounded-lg px-2.5 py-1 text-[11px] font-semibold transition hover:opacity-90 disabled:opacity-50"
                      style={{
                        background: "var(--accent)",
                        color: "var(--text-on-accent)",
                      }}
                    >
                      {tr("aiApprovalSwitchSessionBtn", "Mở phiên đó")}
                    </button>
                  )}
                </div>
              )}

              {/* Current session pending approval cards */}
              {currentSessionApprovals.map((req) => {
                const isHighRisk = req.risk === "high";
                const allowSessionOk = isAllowSessionPermitted(req);
                const remainingSecs = computeApprovalRemainingSecs(
                  req.created_ts,
                  req.timeout_secs,
                  approvalNowMs,
                );
                const detailView = extractApprovalDetailView(req);
                const busyDecision = busyApprovalDecisions[req.approval_id];
                const isCardBusy = Boolean(busyDecision);
                const cardError = approvalErrors[req.approval_id];
                const copyKey = `approval-cmd-${req.approval_id}`;
                const isCmdCopied = copiedCodeKey === copyKey;
                const displayTitle =
                  req.title?.trim() || formatToolDisplayName(req.tool_name, lang);

                return (
                  <div
                    key={req.approval_id}
                    className="space-y-2.5 rounded-2xl p-3.5 shadow-md"
                    style={{
                      background: isHighRisk
                        ? "rgba(239, 68, 68, 0.08)"
                        : "var(--bg-surface)",
                      border: isHighRisk
                        ? "1.5px solid rgba(239, 68, 68, 0.55)"
                        : "1.5px solid rgba(245, 158, 11, 0.52)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {/* Header badges & live countdown */}
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span
                          className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                          style={{
                            background: "var(--accent-soft)",
                            color: "var(--accent)",
                            border: "1px solid var(--chip-border)",
                          }}
                        >
                          {req.category === "system" ? (
                            <Terminal className="h-3 w-3" />
                          ) : (
                            <MessageSquare className="h-3 w-3" />
                          )}
                          <span>
                            {req.category === "system"
                              ? tr("aiApprovalCategorySystem", "Windows / PowerShell")
                              : tr("aiApprovalCategoryTeza", "Teza")}
                          </span>
                        </span>

                        <span
                          className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                          style={
                            isHighRisk
                              ? {
                                  background: "rgba(239, 68, 68, 0.16)",
                                  color: "#f87171",
                                }
                              : {
                                  background: "rgba(245, 158, 11, 0.16)",
                                  color: "#fbbf24",
                                }
                          }
                        >
                          {isHighRisk ? (
                            <ShieldAlert className="h-3 w-3" />
                          ) : (
                            <AlertTriangle className="h-3 w-3" />
                          )}
                          <span>
                            {isHighRisk
                              ? tr("aiApprovalRiskHigh", "Rủi ro cao")
                              : tr("aiApprovalRiskMedium", "Cần xác nhận")}
                          </span>
                        </span>

                        <span
                          className="text-[11px] font-medium"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {formatToolDisplayName(req.tool_name, lang)}
                        </span>
                      </div>

                      <span
                        className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10.5px] font-semibold tabular-nums"
                        style={{
                          background:
                            remainingSecs <= 15
                              ? "rgba(239, 68, 68, 0.14)"
                              : "var(--chip-bg)",
                          color:
                            remainingSecs <= 15
                              ? "#f87171"
                              : "var(--text-secondary)",
                        }}
                      >
                        <Clock className="h-3 w-3" />
                        <span>
                          {remainingSecs > 0
                            ? tr("aiApprovalCountdown", "Còn {secs}s", {
                                secs: remainingSecs,
                              })
                            : tr("aiApprovalExpired", "Đã hết hạn")}
                        </span>
                      </span>
                    </div>

                    {/* Title & summary */}
                    <div className="space-y-0.5">
                      <div className="text-[13px] font-semibold leading-snug">
                        {displayTitle}
                      </div>
                      {req.summary && (
                        <p
                          className="text-xs leading-relaxed"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {req.summary}
                        </p>
                      )}
                    </div>

                    {/* Structured detail box tailored per tool kind */}
                    {detailView.kind === "powershell" && (
                      <div
                        className="overflow-hidden rounded-xl"
                        style={{
                          background: "var(--bg-stage)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <div
                          className="flex flex-wrap items-center justify-between gap-2 px-3 py-1.5 text-[11px]"
                          style={{
                            background: "var(--card-bg)",
                            borderBottom: "1px solid var(--border-subtle)",
                            color: "var(--text-secondary)",
                          }}
                        >
                          <span className="inline-flex items-center gap-1.5 font-semibold">
                            <Terminal className="h-3 w-3" style={{ color: "var(--accent)" }} />
                            <span>
                              {tr("aiApprovalDetailCommand", "Lệnh PowerShell nguyên văn:")}
                            </span>
                          </span>
                          {detailView.command && (
                            <button
                              type="button"
                              onClick={() => handleCopyCode(copyKey, detailView.command)}
                              className="hover-menu inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10.5px] font-medium"
                              style={{
                                color: isCmdCopied ? "#22c55e" : "var(--text-secondary)",
                              }}
                            >
                              {isCmdCopied ? (
                                <Check className="h-3 w-3" />
                              ) : (
                                <Copy className="h-3 w-3" />
                              )}
                              <span>{isCmdCopied ? "Đã chép" : "Sao chép"}</span>
                            </button>
                          )}
                        </div>
                        {detailView.cwd && (
                          <div
                            className="border-b px-3 py-1 text-[11px]"
                            style={{
                              borderColor: "var(--border-subtle)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            <span className="font-semibold">
                              {tr("aiApprovalDetailCwd", "Thư mục chạy:")}{" "}
                            </span>
                            <code className="break-all font-mono">{detailView.cwd}</code>
                          </div>
                        )}
                        <pre className="selectable-text max-h-40 overflow-auto p-3 font-mono text-[12px] leading-relaxed">
                          <code>{detailView.command || "(trống)"}</code>
                        </pre>
                      </div>
                    )}

                    {detailView.kind === "write_file" && (
                      <div
                        className="space-y-1.5 rounded-xl p-2.5 text-xs"
                        style={{
                          background: "var(--bg-stage)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <div className="flex flex-wrap items-center gap-1.5">
                          <FileCode2
                            className="h-3.5 w-3.5 shrink-0"
                            style={{ color: "var(--accent)" }}
                          />
                          <span
                            className="font-semibold"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            {tr("aiApprovalDetailTargetPath", "Đường dẫn tệp:")}
                          </span>
                          <code className="selectable-text break-all font-mono text-[11.5px]">
                            {detailView.path}
                          </code>
                          {detailView.mode && (
                            <span
                              className="rounded-full px-2 py-0.5 font-mono text-[10px] font-semibold"
                              style={{
                                background: "var(--chip-bg)",
                                color: "var(--text-secondary)",
                              }}
                            >
                              {detailView.mode}
                            </span>
                          )}
                        </div>
                        {detailView.contentPreview && (
                          <div className="space-y-1">
                            <div
                              className="text-[11px] font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {tr("aiApprovalDetailContentPreview", "Nội dung xem trước:")}
                            </div>
                            <pre
                              className="selectable-text max-h-32 overflow-auto rounded-lg p-2 font-mono text-[11.5px] leading-relaxed"
                              style={{
                                background: "var(--bg-sidebar)",
                                border: "1px solid var(--border-subtle)",
                              }}
                            >
                              <code>{detailView.contentPreview}</code>
                            </pre>
                          </div>
                        )}
                      </div>
                    )}

                    {detailView.kind === "open_path" && detailView.path && (
                      <div
                        className="flex flex-wrap items-center gap-1.5 rounded-xl px-3 py-2 text-xs"
                        style={{
                          background: "var(--bg-stage)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <span
                          className="font-semibold"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {tr("aiApprovalDetailTargetPath", "Đường dẫn tệp:")}
                        </span>
                        <code className="selectable-text break-all font-mono text-[11.5px]">
                          {detailView.path}
                        </code>
                      </div>
                    )}

                    {detailView.kind === "message" && (
                      <div
                        className="space-y-1.5 rounded-xl p-2.5 text-xs"
                        style={{
                          background: "var(--bg-stage)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        {detailView.recipient && (
                          <div className="flex flex-wrap items-baseline gap-1.5">
                            <span
                              className="font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {tr("aiApprovalDetailRecipient", "Người nhận / Hội thoại:")}
                            </span>
                            <span className="font-semibold">{detailView.recipient}</span>
                          </div>
                        )}
                        {detailView.scheduleTime && (
                          <div className="flex flex-wrap items-baseline gap-1.5">
                            <span
                              className="font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {tr("aiApprovalDetailScheduleTime", "Thời gian hẹn gửi:")}
                            </span>
                            <span className="font-mono text-[11.5px]">
                              {detailView.scheduleTime}
                              {detailView.recurrence ? ` (${detailView.recurrence})` : ""}
                            </span>
                          </div>
                        )}
                        {detailView.text && (
                          <div className="space-y-1">
                            <div
                              className="text-[11px] font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {tr("aiApprovalDetailMessage", "Nội dung tin nhắn:")}
                            </div>
                            <div
                              className="selectable-text max-h-28 overflow-auto whitespace-pre-wrap break-words rounded-lg px-2.5 py-2 text-xs leading-relaxed"
                              style={{
                                background: "var(--bg-sidebar)",
                                border: "1px solid var(--border-subtle)",
                              }}
                            >
                              {detailView.text}
                            </div>
                          </div>
                        )}
                        {detailView.filePath && (
                          <div className="flex flex-wrap items-baseline gap-1.5">
                            <span
                              className="font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {tr("aiApprovalDetailAttachment", "Tệp đính kèm:")}
                            </span>
                            <code className="selectable-text break-all font-mono text-[11.5px]">
                              {detailView.filePath}
                            </code>
                          </div>
                        )}
                      </div>
                    )}

                    {detailView.kind === "generic" && detailView.entries.length > 0 && (
                      <div
                        className="max-h-32 space-y-1 overflow-auto rounded-xl p-2.5 text-[11.5px]"
                        style={{
                          background: "var(--bg-stage)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        {detailView.entries.map((item) => (
                          <div key={item.key} className="flex flex-wrap items-baseline gap-1.5">
                            <span
                              className="font-mono text-[11px] font-semibold"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {item.key}:
                            </span>
                            <span className="selectable-text break-all">{item.value}</span>
                          </div>
                        ))}
                      </div>
                    )}

                    {cardError && (
                      <div
                        className="rounded-lg px-2.5 py-1.5 text-[11.5px]"
                        style={{
                          background: "var(--danger-bg)",
                          color: "var(--danger-text)",
                        }}
                      >
                        {cardError}
                      </div>
                    )}

                    {/* Action buttons with per-card busy lock */}
                    <div className="flex flex-wrap items-center justify-between gap-2 pt-0.5">
                      <div className="flex flex-wrap items-center gap-2">
                        <button
                          type="button"
                          disabled={isCardBusy || !onResolveApproval}
                          onClick={() =>
                            void handleResolveApprovalClick(req.approval_id, "allow_once")
                          }
                          className="inline-flex items-center gap-1.5 rounded-xl bg-emerald-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:bg-emerald-500 disabled:cursor-not-allowed disabled:opacity-55"
                        >
                          {busyDecision === "allow_once" ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Check className="h-3.5 w-3.5" />
                          )}
                          <span>{tr("aiApprovalAllowOnce", "Cho phép lần này")}</span>
                        </button>

                        {allowSessionOk && (
                          <button
                            type="button"
                            disabled={isCardBusy || !onResolveApproval}
                            onClick={() =>
                              void handleResolveApprovalClick(req.approval_id, "allow_session")
                            }
                            className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold transition disabled:cursor-not-allowed disabled:opacity-55"
                            style={{
                              background: "var(--accent-soft)",
                              border: "1px solid var(--chip-border)",
                              color: "var(--accent)",
                            }}
                          >
                            {busyDecision === "allow_session" && (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            )}
                            <span>
                              {tr(
                                "aiApprovalAllowSession",
                                "Luôn cho phép loại này trong phiên",
                              )}
                            </span>
                          </button>
                        )}

                        <button
                          type="button"
                          disabled={isCardBusy || !onResolveApproval}
                          onClick={() =>
                            void handleResolveApprovalClick(req.approval_id, "deny")
                          }
                          className="inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-55"
                          style={{
                            background: "var(--danger-bg)",
                            color: "var(--danger-text)",
                            border: "1px solid rgba(239, 68, 68, 0.35)",
                          }}
                        >
                          {busyDecision === "deny" ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <X className="h-3.5 w-3.5" />
                          )}
                          <span>{tr("aiApprovalDeny", "Từ chối")}</span>
                        </button>
                      </div>

                      {!allowSessionOk && (
                        <span
                          className="text-[10.5px] italic"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {tr(
                            "aiApprovalHighRiskNoSessionHint",
                            "Thao tác rủi ro cao luôn yêu cầu xác nhận từng lần.",
                          )}
                        </span>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}

          {/* Composer */}
          <div
            className="shrink-0 px-5 py-3"
            style={{
              background: "var(--bg-sidebar)",
              borderTop: "1px solid var(--border-subtle)",
            }}
          >
            <div className="relative flex w-full flex-col gap-1.5">
              {/* Floating Slash Command Autocomplete Menu */}
              {slashSuggestions.length > 0 && (
                <div
                  className="absolute bottom-full left-0 right-0 z-30 mb-2 overflow-hidden rounded-2xl shadow-2xl"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--text-primary)",
                  }}
                >
                  <div
                    className="flex items-center justify-between gap-2 border-b px-3 py-1.5 text-[11px]"
                    style={{
                      background: "var(--bg-surface)",
                      borderColor: "var(--border-subtle)",
                      color: "var(--text-secondary)",
                    }}
                  >
                    <span className="inline-flex items-center gap-1.5 font-semibold">
                      <Terminal className="h-3 w-3" style={{ color: "var(--accent)" }} />
                      <span>Lệnh nhanh</span>
                    </span>
                    <span className="hidden sm:inline">
                      ↑↓ chọn · Enter chạy · Tab điền · Esc đóng
                    </span>
                  </div>
                  <div className="max-h-60 overflow-y-auto p-1.5">
                    {slashSuggestions.map((item, idx) => {
                      const highlighted = idx === activeSlashIndex;
                      return (
                        <button
                          key={item.cmd}
                          type="button"
                          onMouseDown={(e) => e.preventDefault()}
                          onMouseEnter={() => setSlashSelectedIndex(idx)}
                          onClick={() => handleSelectSlashSuggestion(item)}
                          className="flex w-full items-center justify-between gap-3 rounded-xl px-2.5 py-2 text-left transition"
                          style={{
                            background: highlighted ? "var(--accent-soft)" : "transparent",
                            border: highlighted
                              ? "1px solid var(--accent)"
                              : "1px solid transparent",
                          }}
                        >
                          <div className="flex min-w-0 flex-1 items-baseline gap-2.5">
                            <span
                              className="shrink-0 font-mono text-xs font-semibold"
                              style={{ color: "var(--accent)" }}
                            >
                              {item.cmd}
                            </span>
                            <span className="truncate text-xs font-medium">{item.label}</span>
                            <span
                              className="hidden truncate text-[11.5px] sm:inline"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              — {item.description}
                            </span>
                          </div>
                          {item.zeroclawOnly && (
                            <span
                              className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold"
                              style={{
                                background: isZeroClawMode ? "var(--accent-soft)" : "var(--chip-bg)",
                                color: isZeroClawMode ? "var(--accent)" : "var(--text-secondary)",
                                border: "1px solid var(--chip-border)",
                              }}
                              title={
                                isZeroClawMode
                                  ? "Chạy trực tiếp trên ZeroClaw"
                                  : "Cần chế độ ZeroClaw để sử dụng"
                              }
                            >
                              ZeroClaw
                            </span>
                          )}
                        </button>
                      );
                    })}
                  </div>
                </div>
              )}

              <div
                className="flex items-end gap-2 rounded-2xl px-3 py-2"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <textarea
                  ref={textareaRef}
                  rows={1}
                  value={draft}
                  onChange={(e) => {
                    setDraft(e.target.value);
                    setSlashMenuDismissed(false);
                    setSlashSelectedIndex(0);
                  }}
                  onKeyDown={handleComposerKeyDown}
                  placeholder="Nhập câu hỏi hoặc gõ / để xem lệnh nhanh... (Enter gửi, Shift+Enter xuống dòng)"
                  className="max-h-40 min-h-[36px] flex-1 resize-none bg-transparent py-1 text-[14px] leading-relaxed outline-none"
                  style={{ color: "var(--text-primary)" }}
                />

                {isStreaming && onCancelStream ? (
                  <button
                    type="button"
                    disabled={cancellingStream}
                    onClick={handleCancelStreamClick}
                    className="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-xl px-3 text-xs font-semibold transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-60"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                    }}
                    title="Dừng phản hồi AI"
                  >
                    {cancellingStream ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Square className="h-3.5 w-3.5 fill-current" />
                    )}
                    <span>{cancellingStream ? "Đang dừng..." : "Dừng tạo"}</span>
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={
                      isStreaming ||
                      !draft.trim() ||
                      (relayBlocksChat && !parseAiSlashCommand(draft.trim()))
                    }
                    onClick={handleSubmit}
                    className="tf-btn inline-flex h-9 shrink-0 items-center gap-1.5 rounded-xl px-3.5 text-xs font-semibold disabled:cursor-not-allowed disabled:opacity-45"
                    title="Gửi (Enter)"
                  >
                    <Send className="h-3.5 w-3.5" />
                    <span>Gửi</span>
                  </button>
                )}
              </div>

              <div
                className="flex items-center justify-between px-1 text-[11px]"
                style={{ color: "var(--text-secondary)" }}
              >
                <span className="inline-flex items-center gap-1">
                  <CornerDownLeft className="h-3 w-3" />
                  Enter gửi · Shift+Enter xuống dòng · Gõ / xem lệnh nhanh
                </span>
                <span>Trợ lý AI có thể mắc sai sót — hãy kiểm chứng thông tin quan trọng.</span>
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

export default AiChatPanel;
