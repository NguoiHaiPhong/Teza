import { useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  Ban,
  CalendarClock,
  CheckCircle2,
  Clock,
  Cloud,
  FileText,
  Hash,
  Image as ImageIcon,
  Loader2,
  Paperclip,
  Pencil,
  RefreshCw,
  Repeat,
  Search,
  Send,
  Trash2,
  User,
  Users,
  X,
} from "lucide-react";
import { bridge, isTauri, MAX_ATTACHMENT_BYTES } from "../bridge";
import { useI18n } from "../i18n";
import type { AttachmentMeta, ThreadDto, ThreadKind } from "../types";
import { filterThreadsBySearchQuery, isImeComposingEvent } from "../utils/searchNormalize";
import {
  extractAttachmentBasename,
  formatLocalDateTime,
  formatScheduleMisfirePolicy,
  formatScheduleRecurrence,
  formatScheduleStatus,
  formatThreadKindLabel,
  getScheduleStatusTone,
  isImageAttachmentPath,
  toLocalDatetimeInputValue,
  validateScheduleTimestamp,
  type BadgeTone,
  type CreateScheduledMessageInput,
  type ScheduleMisfirePolicy,
  type ScheduleRecurrence,
  type ScheduledMessageDto,
  type UpdateScheduledMessageInput,
} from "../utils/automationHelpers";

export interface ScheduledMessagesModalProps {
  open: boolean;
  threads: ThreadDto[];
  schedules: ScheduledMessageDto[];
  loading?: boolean;
  initialThreadId?: string | null;
  isLocked?: boolean;
  onClose: () => void;
  onRefresh?: () => Promise<void> | void;
  onPickAttachment?: () => Promise<AttachmentMeta | null>;
  onCreate: (input: CreateScheduledMessageInput) => Promise<ScheduledMessageDto | void>;
  onUpdate: (
    id: string,
    input: UpdateScheduledMessageInput,
  ) => Promise<ScheduledMessageDto | null | void>;
  onCancel: (id: string) => Promise<boolean | void>;
  onDelete: (id: string) => Promise<boolean | void>;
  onSendNow: (id: string) => Promise<ScheduledMessageDto | null | void>;
}

function formatAttachmentBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

type RecipientMode = "thread" | "manual";
type ScheduleFilterTab = "pending" | "sent" | "attention" | "all";

const RECURRENCE_OPTIONS: { value: ScheduleRecurrence; label: string }[] = [
  { value: "once", label: "Một lần" },
  { value: "daily", label: "Hằng ngày" },
  { value: "weekly", label: "Hằng tuần" },
  { value: "monthly", label: "Hằng tháng" },
];

const MISFIRE_OPTIONS: { value: ScheduleMisfirePolicy; label: string; desc: string }[] = [
  {
    value: "send_on_resume",
    label: "Gửi ngay khi mở lại",
    desc: "Nếu máy tắt/mất mạng lúc đến giờ, gửi ngay khi mở Teza lại",
  },
  {
    value: "skip",
    label: "Bỏ qua nếu trễ",
    desc: "Đánh dấu bỏ lỡ nếu trễ quá hạn, không gửi bù",
  },
  {
    value: "ask",
    label: "Hỏi xác nhận",
    desc: "Giữ lại ở mục Bỏ lỡ để bạn tự bấm Gửi ngay khi cần",
  },
];

const KIND_OPTIONS: { value: ThreadKind; label: string; icon: typeof User }[] = [
  { value: "direct", label: "Cá nhân", icon: User },
  { value: "group", label: "Nhóm", icon: Users },
  { value: "cloud", label: "Cloud", icon: Cloud },
];

function badgeStyle(tone: BadgeTone): React.CSSProperties {
  switch (tone) {
    case "info":
      return {
        background: "var(--accent-soft)",
        color: "var(--accent)",
      };
    case "success":
      return {
        background: "rgba(34, 197, 94, 0.14)",
        color: "#4ade80",
      };
    case "warn":
      return {
        background: "rgba(245, 158, 11, 0.16)",
        color: "#fbbf24",
      };
    case "danger":
      return {
        background: "var(--danger-bg)",
        color: "var(--danger-text)",
      };
    case "neutral":
    default:
      return {
        background: "var(--chip-bg)",
        color: "var(--text-secondary)",
      };
  }
}

export function ScheduledMessagesModal({
  open,
  threads,
  schedules,
  loading = false,
  initialThreadId = null,
  isLocked = false,
  onClose,
  onRefresh,
  onPickAttachment,
  onCreate,
  onUpdate,
  onCancel,
  onDelete,
  onSendNow,
}: ScheduledMessagesModalProps) {
  const { t } = useI18n();
  const [recipientMode, setRecipientMode] = useState<RecipientMode>("thread");
  const [threadSearch, setThreadSearch] = useState("");
  const [selectedThreadId, setSelectedThreadId] = useState("");
  const [manualThreadId, setManualThreadId] = useState("");
  const [manualThreadTitle, setManualThreadTitle] = useState("");
  const [manualKind, setManualKind] = useState<ThreadKind>("direct");

  const [text, setText] = useState("");
  const [attachmentPath, setAttachmentPath] = useState("");
  const [attachmentSizeBytes, setAttachmentSizeBytes] = useState<number | null>(null);
  const [pickingAttachment, setPickingAttachment] = useState(false);
  const [dateTimeStr, setDateTimeStr] = useState(() =>
    toLocalDatetimeInputValue(Date.now() + 10 * 60_000),
  );
  const [recurrence, setRecurrence] = useState<ScheduleRecurrence>("once");
  const [misfirePolicy, setMisfirePolicy] = useState<ScheduleMisfirePolicy>("send_on_resume");

  const [editingItem, setEditingItem] = useState<ScheduledMessageDto | null>(null);
  const [filterTab, setFilterTab] = useState<ScheduleFilterTab>("pending");

  const [submitting, setSubmitting] = useState(false);
  const [busyRowId, setBusyRowId] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [feedback, setFeedback] = useState<{
    tone: "ok" | "warn" | "error";
    text: string;
  } | null>(null);

  const submittingRef = useRef(false);
  const busyRowRef = useRef(false);
  const pickingAttachmentRef = useRef(false);
  const fallbackFileInputRef = useRef<HTMLInputElement | null>(null);
  const threadsRef = useRef(threads);
  threadsRef.current = threads;

  // Initialize form whenever modal opens (read threads via ref so incoming messages never wipe form state)
  useEffect(() => {
    if (!open) return;
    setEditingItem(null);
    setText("");
    setAttachmentPath("");
    setAttachmentSizeBytes(null);
    setDateTimeStr(toLocalDatetimeInputValue(Date.now() + 10 * 60_000));
    setRecurrence("once");
    setMisfirePolicy("send_on_resume");
    setThreadSearch("");
    setFeedback(null);

    const currentThreads = threadsRef.current;
    if (initialThreadId) {
      const matched = currentThreads.find((th) => th.id === initialThreadId);
      if (matched) {
        setRecipientMode("thread");
        setSelectedThreadId(matched.id);
      } else {
        setRecipientMode("manual");
        setManualThreadId(initialThreadId);
        setManualThreadTitle("");
        setManualKind("direct");
      }
    } else if (currentThreads.length > 0) {
      setRecipientMode("thread");
      setSelectedThreadId(currentThreads[0].id);
    } else {
      setRecipientMode("manual");
      setSelectedThreadId("");
    }
  }, [open, initialThreadId]);

  // Escape to close (suppressed while app is locked or IME is composing)
  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e)) return;
      if (e.defaultPrevented || e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, onClose]);

  const filteredThreads = useMemo(
    () => filterThreadsBySearchQuery(threads, threadSearch, 60),
    [threads, threadSearch],
  );

  const selectedThread = useMemo(
    () => threads.find((t) => t.id === selectedThreadId) ?? null,
    [threads, selectedThreadId],
  );

  const tabCounts = useMemo(() => {
    let pending = 0;
    let sent = 0;
    let attention = 0;
    for (const s of schedules) {
      if (s.status === "pending" || s.status === "sending") pending += 1;
      else if (s.status === "sent") sent += 1;
      else attention += 1;
    }
    return { pending, sent, attention, all: schedules.length };
  }, [schedules]);

  const filteredSchedules = useMemo(() => {
    const sorted = [...schedules].sort((a, b) => {
      if (a.status === "pending" && b.status !== "pending") return -1;
      if (b.status === "pending" && a.status !== "pending") return 1;
      if (a.status === "pending" && b.status === "pending") {
        return a.send_at_ts - b.send_at_ts;
      }
      return b.updated_at - a.updated_at;
    });
    return sorted.filter((s) => {
      if (filterTab === "pending") return s.status === "pending" || s.status === "sending";
      if (filterTab === "sent") return s.status === "sent";
      if (filterTab === "attention") {
        return s.status === "failed" || s.status === "missed" || s.status === "cancelled";
      }
      return true;
    });
  }, [schedules, filterTab]);

  if (!open) return null;

  const applyTimePreset = (mode: "10m" | "1h" | "today18" | "tomorrow9") => {
    const now = Date.now();
    if (mode === "10m") {
      setDateTimeStr(toLocalDatetimeInputValue(now + 10 * 60_000));
      return;
    }
    if (mode === "1h") {
      setDateTimeStr(toLocalDatetimeInputValue(now + 60 * 60_000));
      return;
    }
    if (mode === "today18") {
      const d = new Date();
      d.setHours(18, 0, 0, 0);
      if (d.getTime() <= now) d.setDate(d.getDate() + 1);
      setDateTimeStr(toLocalDatetimeInputValue(d.getTime()));
      return;
    }
    const d = new Date();
    d.setDate(d.getDate() + 1);
    d.setHours(9, 0, 0, 0);
    setDateTimeStr(toLocalDatetimeInputValue(d.getTime()));
  };

  const resetFormFields = () => {
    setEditingItem(null);
    setText("");
    setAttachmentPath("");
    setAttachmentSizeBytes(null);
    setDateTimeStr(toLocalDatetimeInputValue(Date.now() + 10 * 60_000));
    setRecurrence("once");
    setMisfirePolicy("send_on_resume");
  };

  const resetFormToCreate = () => {
    resetFormFields();
    setFeedback(null);
  };

  const startEditing = (item: ScheduledMessageDto) => {
    const now = Date.now();
    setEditingItem(item);
    setText(item.text);
    setAttachmentPath(item.attachment_path || "");
    setAttachmentSizeBytes(null);
    const futureFallback = item.send_at_ts > now ? item.send_at_ts : now + 10 * 60_000;
    setDateTimeStr(toLocalDatetimeInputValue(futureFallback, now));
    setRecurrence(item.recurrence);
    setMisfirePolicy(item.misfire_policy);
    setFeedback(null);
  };

  const handlePickAttachment = async () => {
    if (pickingAttachmentRef.current || submittingRef.current) return;
    setFeedback(null);
    if (onPickAttachment || isTauri()) {
      pickingAttachmentRef.current = true;
      setPickingAttachment(true);
      try {
        const meta = onPickAttachment
          ? await onPickAttachment()
          : await bridge.pickAttachmentFile();
        if (meta?.path) {
          setAttachmentPath(meta.path);
          setAttachmentSizeBytes(typeof meta.size_bytes === "number" ? meta.size_bytes : null);
        }
      } catch (err) {
        setFeedback({
          tone: "error",
          text: err instanceof Error ? err.message : String(err),
        });
      } finally {
        pickingAttachmentRef.current = false;
        setPickingAttachment(false);
      }
      return;
    }
    fallbackFileInputRef.current?.click();
  };

  const handleFallbackFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setFeedback(null);
    if (file.size === 0) {
      setFeedback({ tone: "error", text: t("composerAttachmentEmptyError") });
      return;
    }
    if (file.size > MAX_ATTACHMENT_BYTES) {
      setFeedback({
        tone: "error",
        text: t("composerAttachmentTooLargeError", {
          name: file.name,
          size: formatAttachmentBytes(file.size),
          maxMb: MAX_ATTACHMENT_BYTES / (1024 * 1024),
        }),
      });
      return;
    }
    setAttachmentPath(file.name);
    setAttachmentSizeBytes(file.size);
  };

  const preventInputEnterSubmit = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (isImeComposingEvent(e)) return;
    if (e.key === "Enter") {
      e.preventDefault();
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (submittingRef.current) return;
    setFeedback(null);

    const cleanText = text.trim();
    const cleanAttachment = attachmentPath.trim();
    if (!cleanText && !cleanAttachment) {
      setFeedback({ tone: "error", text: t("schedEmptyContentError") });
      return;
    }
    if (cleanText.length > 4000) {
      setFeedback({ tone: "error", text: "Nội dung tin nhắn không được vượt quá 4000 ký tự." });
      return;
    }

    const parsedMs = new Date(dateTimeStr).getTime();
    const tsCheck = validateScheduleTimestamp(parsedMs, Date.now());
    if (!tsCheck.valid) {
      setFeedback({ tone: "error", text: tsCheck.error || "Thời gian gửi không hợp lệ." });
      return;
    }

    if (!editingItem) {
      if (recipientMode === "thread" && !selectedThread) {
        setFeedback({
          tone: "error",
          text: "Vui lòng chọn hội thoại nhận tin hoặc chuyển sang chế độ Nhập ID thủ công.",
        });
        return;
      }
      if (recipientMode === "manual" && !manualThreadId.trim()) {
        setFeedback({ tone: "error", text: "Vui lòng nhập ID hội thoại / người nhận." });
        return;
      }
    }

    submittingRef.current = true;
    setSubmitting(true);
    try {
      if (editingItem) {
        const previousStatus = editingItem.status;
        const updated = await onUpdate(editingItem.id, {
          text: cleanText,
          attachment_path: cleanAttachment,
          send_at_ts: parsedMs,
          recurrence,
          misfire_policy: misfirePolicy,
        });
        if (!updated) {
          setEditingItem(null);
          setFeedback({ tone: "warn", text: t("schedActionStaleNotice") });
          void onRefresh?.();
          return;
        }
        setFeedback({ tone: "ok", text: "Đã cập nhật lịch gửi tin nhắn." });
        resetFormFields();
        if (previousStatus !== "pending") {
          setFilterTab("pending");
        }
      } else {
        let targetId = "";
        let targetTitle = "";
        let targetKind: ThreadKind = "direct";

        if (recipientMode === "thread" && selectedThread) {
          targetId = selectedThread.id;
          targetTitle = selectedThread.title;
          targetKind = selectedThread.kind;
        } else {
          targetId = manualThreadId.trim();
          targetTitle = manualThreadTitle.trim() || `ID ${targetId}`;
          targetKind = manualKind;
        }

        await onCreate({
          thread_id: targetId,
          thread_title: targetTitle,
          thread_kind: targetKind,
          text: cleanText,
          attachment_path: cleanAttachment || undefined,
          send_at_ts: parsedMs,
          recurrence,
          misfire_policy: misfirePolicy,
        });
        setFeedback({ tone: "ok", text: "Đã tạo lịch hẹn gửi tin nhắn thành công." });
        resetFormFields();
        setFilterTab("pending");
      }
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  };

  const runRowAction = async (
    id: string,
    fn: () => Promise<boolean | ScheduledMessageDto | null | void>,
    okMsg: string,
    options?: {
      expectTruthy?: boolean;
      onSuccess?: () => void;
    },
  ) => {
    if (busyRowRef.current) return;
    busyRowRef.current = true;
    setBusyRowId(id);
    setFeedback(null);
    try {
      const result = await fn();
      const stale = options?.expectTruthy ? !result : result === false;
      if (stale) {
        if (editingItem?.id === id) {
          resetFormFields();
        }
        setFeedback({ tone: "warn", text: t("schedActionStaleNotice") });
        void onRefresh?.();
        return;
      }
      if (editingItem?.id === id) {
        resetFormFields();
      }
      options?.onSuccess?.();
      setFeedback({ tone: "ok", text: okMsg });
    } catch (err) {
      setFeedback({
        tone: "error",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      busyRowRef.current = false;
      setBusyRowId(null);
    }
  };

  const handleRefresh = async () => {
    if (!onRefresh || refreshing) return;
    setRefreshing(true);
    try {
      await onRefresh();
    } finally {
      setRefreshing(false);
    }
  };

  const hasDraftContent = Boolean(text.trim() || attachmentPath.trim());
  const selectedAttachmentBasename = extractAttachmentBasename(attachmentPath);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={() => {
        if (!isLocked) onClose();
      }}
    >
      <div
        className="flex h-[86vh] max-h-[760px] w-full max-w-5xl flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Modal Header */}
        <div
          className="flex shrink-0 items-center justify-between gap-3 px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2.5">
            <CalendarClock className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
            <h3 className="truncate text-[16px] font-semibold">Tin nhắn hẹn giờ</h3>
            <span
              className="rounded-full px-2 py-0.5 text-[11px] font-semibold"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
            >
              {tabCounts.pending} chờ gửi
            </span>
          </div>

          <div className="flex items-center gap-1.5">
            {onRefresh && (
              <button
                type="button"
                disabled={refreshing}
                onClick={() => void handleRefresh()}
                className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs font-medium transition disabled:opacity-50"
                style={{ color: "var(--text-secondary)" }}
                title="Làm mới danh sách"
              >
                <RefreshCw className={`h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`} />
                <span className="hidden sm:inline">Làm mới</span>
              </button>
            )}
            <button
              type="button"
              onClick={onClose}
              className="hover-menu rounded-lg p-1.5"
              title="Đóng (Esc)"
            >
              <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
            </button>
          </div>
        </div>

        {/* 2-Column Body */}
        <div className="flex min-h-0 flex-1 flex-col md:flex-row">
          {/* Left Column: Create / Edit Form */}
          <form
            onSubmit={(e) => void handleSubmit(e)}
            onKeyDown={(e) => {
              if (isImeComposingEvent(e) && e.key === "Enter") {
                e.preventDefault();
              }
            }}
            className="flex shrink-0 flex-col gap-3.5 overflow-y-auto p-4 md:w-[390px]"
            style={{
              background: "var(--bg-stage)",
              borderRight: "1px solid var(--border-subtle)",
              borderBottom: "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex items-center justify-between">
              <h4 className="text-[13.5px] font-semibold">
                {editingItem ? "Chỉnh sửa lịch gửi" : "Tạo lịch hẹn mới"}
              </h4>
              {editingItem && (
                <button
                  type="button"
                  onClick={resetFormToCreate}
                  className="text-xs font-medium underline hover:opacity-80"
                  style={{ color: "var(--accent)" }}
                >
                  Hủy sửa
                </button>
              )}
            </div>

            {/* Recipient section */}
            {editingItem ? (
              <div
                className="rounded-xl p-3 text-xs"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <div className="mb-1 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                  Người nhận / Hội thoại
                </div>
                <div className="flex items-center justify-between gap-2">
                  <span className="truncate font-semibold">{editingItem.thread_title}</span>
                  <span
                    className="shrink-0 rounded-full px-2 py-0.5 text-[10.5px] font-medium"
                    style={{ background: "var(--chip-bg)", color: "var(--text-secondary)" }}
                  >
                    {formatThreadKindLabel(editingItem.thread_kind)}
                  </span>
                </div>
                <div className="mt-0.5 font-mono text-[11px]" style={{ color: "var(--text-secondary)" }}>
                  ID: {editingItem.thread_id}
                </div>
              </div>
            ) : (
              <div className="space-y-2">
                <div className="grid grid-cols-2 gap-1.5 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                  <button
                    type="button"
                    onClick={() => setRecipientMode("thread")}
                    className="rounded-lg px-2.5 py-1.5 text-xs font-medium transition"
                    style={{
                      background: recipientMode === "thread" ? "var(--bg-active)" : "transparent",
                      color:
                        recipientMode === "thread"
                          ? "var(--text-on-active)"
                          : "var(--text-secondary)",
                    }}
                  >
                    Chọn hội thoại
                  </button>
                  <button
                    type="button"
                    onClick={() => setRecipientMode("manual")}
                    className="rounded-lg px-2.5 py-1.5 text-xs font-medium transition"
                    style={{
                      background: recipientMode === "manual" ? "var(--bg-active)" : "transparent",
                      color:
                        recipientMode === "manual"
                          ? "var(--text-on-active)"
                          : "var(--text-secondary)",
                    }}
                  >
                    Nhập ID thủ công
                  </button>
                </div>

                {recipientMode === "thread" ? (
                  <div className="space-y-1.5">
                    {selectedThread && (
                      <div
                        className="flex items-center justify-between gap-2 rounded-xl px-2.5 py-1.5 text-[11.5px]"
                        style={{
                          background: "var(--accent-soft)",
                          border: "1px solid var(--border-subtle)",
                        }}
                        title={`${selectedThread.title || selectedThread.id} (${selectedThread.id})`}
                      >
                        <div className="flex min-w-0 items-center gap-1.5">
                          <span className="shrink-0" style={{ color: "var(--text-secondary)" }}>
                            {t("schedSelectedRecipientLabel")}
                          </span>
                          <span className="truncate font-semibold" style={{ color: "var(--accent)" }}>
                            {selectedThread.title || selectedThread.id}
                          </span>
                        </div>
                        <span
                          className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium"
                          style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                        >
                          {formatThreadKindLabel(selectedThread.kind)}
                        </span>
                      </div>
                    )}

                    <div
                      className="flex items-center gap-2 rounded-xl px-2.5 py-1.5"
                      style={{
                        background: "var(--bg-surface)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      <Search className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--text-secondary)" }} />
                      <input
                        value={threadSearch}
                        onChange={(e) => setThreadSearch(e.target.value)}
                        onKeyDown={preventInputEnterSubmit}
                        placeholder="Lọc theo tên hoặc ID hội thoại..."
                        className="min-w-0 flex-1 bg-transparent text-xs outline-none"
                      />
                    </div>

                    <div
                      className="max-h-36 space-y-1 overflow-y-auto rounded-xl p-1.5"
                      style={{
                        background: "var(--bg-surface)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      {filteredThreads.length === 0 ? (
                        <div
                          className="py-4 text-center text-xs"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          Không tìm thấy hội thoại phù hợp.
                        </div>
                      ) : (
                        filteredThreads.map((th) => {
                          const active = th.id === selectedThreadId;
                          return (
                            <button
                              key={th.id}
                              type="button"
                              onClick={() => setSelectedThreadId(th.id)}
                              className="flex w-full items-center justify-between gap-2 rounded-lg px-2.5 py-1.5 text-left text-xs transition"
                              style={{
                                background: active ? "var(--bg-active)" : "transparent",
                                color: active ? "var(--text-on-active)" : "var(--text-primary)",
                              }}
                            >
                              <div className="min-w-0 flex-1">
                                <div className="truncate font-medium">{th.title || th.id}</div>
                                <div
                                  className="truncate font-mono text-[10.5px]"
                                  style={{
                                    color: active
                                      ? "var(--text-secondary-on-active)"
                                      : "var(--text-secondary)",
                                  }}
                                >
                                  {th.id}
                                </div>
                              </div>
                              <span
                                className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-medium"
                                style={{
                                  background: active ? "rgba(255,255,255,0.14)" : "var(--chip-bg)",
                                }}
                              >
                                {formatThreadKindLabel(th.kind)}
                              </span>
                            </button>
                          );
                        })
                      )}
                    </div>
                  </div>
                ) : (
                  <div className="space-y-2">
                    <div
                      className="flex items-center gap-2 rounded-xl px-3 py-2"
                      style={{
                        background: "var(--bg-surface)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    >
                      <Hash className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--text-secondary)" }} />
                      <input
                        value={manualThreadId}
                        onChange={(e) => setManualThreadId(e.target.value)}
                        onKeyDown={preventInputEnterSubmit}
                        placeholder="Nhập thread_id (VD: 123456789)"
                        className="min-w-0 flex-1 bg-transparent font-mono text-xs outline-none"
                      />
                    </div>
                    <input
                      value={manualThreadTitle}
                      onChange={(e) => setManualThreadTitle(e.target.value)}
                      onKeyDown={preventInputEnterSubmit}
                      placeholder="Tên gợi nhớ (tùy chọn)"
                      className="w-full rounded-xl px-3 py-2 text-xs outline-none"
                      style={{
                        background: "var(--bg-surface)",
                        border: "1px solid var(--border-subtle)",
                      }}
                    />
                    <div className="grid grid-cols-3 gap-1.5">
                      {KIND_OPTIONS.map((k) => {
                        const Icon = k.icon;
                        const active = manualKind === k.value;
                        return (
                          <button
                            key={k.value}
                            type="button"
                            onClick={() => setManualKind(k.value)}
                            className="inline-flex items-center justify-center gap-1 rounded-lg px-2 py-1.5 text-xs font-medium transition"
                            style={{
                              background: active ? "var(--bg-active)" : "var(--bg-surface)",
                              color: active ? "var(--text-on-active)" : "var(--text-primary)",
                              border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                            }}
                          >
                            <Icon className="h-3.5 w-3.5" />
                            <span>{k.label}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                )}
              </div>
            )}

            {/* Message text */}
            <div>
              <div className="mb-1 flex items-center justify-between text-xs">
                <label className="font-medium" style={{ color: "var(--text-secondary)" }}>
                  Nội dung tin nhắn
                </label>
                <span
                  className="font-mono text-[11px]"
                  style={{ color: text.length > 4000 ? "#f87171" : "var(--text-secondary)" }}
                >
                  {text.length}/4000
                </span>
              </div>
              <textarea
                rows={4}
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder="Nhập nội dung tin nhắn muốn tự động gửi..."
                className="w-full resize-y rounded-xl px-3 py-2 text-xs leading-relaxed outline-none"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>

            {/* Attachment picker (#31) */}
            <div className="space-y-1.5">
              <div className="flex items-center justify-between gap-2 text-xs">
                <span className="font-medium" style={{ color: "var(--text-secondary)" }}>
                  {t("schedAttachmentLabel")}
                </span>
                <button
                  type="button"
                  disabled={pickingAttachment || submitting}
                  onClick={() => void handlePickAttachment()}
                  className="hover-menu inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-[11.5px] font-medium transition disabled:opacity-50"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                    color: "var(--accent)",
                  }}
                >
                  {pickingAttachment ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Paperclip className="h-3.5 w-3.5" />
                  )}
                  <span>
                    {attachmentPath.trim()
                      ? t("schedChangeAttachmentBtn")
                      : t("schedAttachFileBtn")}
                  </span>
                </button>
              </div>
              <input
                ref={fallbackFileInputRef}
                type="file"
                className="hidden"
                onChange={handleFallbackFileChange}
              />
              {attachmentPath.trim() && (
                <div
                  className="flex items-center justify-between gap-2 rounded-xl px-2.5 py-2 text-xs"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                  title={attachmentPath}
                >
                  <div className="flex min-w-0 items-center gap-2">
                    {isImageAttachmentPath(attachmentPath) ? (
                      <ImageIcon className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                    ) : (
                      <FileText className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                    )}
                    <span className="truncate font-medium">
                      {selectedAttachmentBasename || t("attachmentFallbackLabel")}
                    </span>
                    {attachmentSizeBytes !== null && (
                      <span
                        className="shrink-0 font-mono text-[10.5px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        ({formatAttachmentBytes(attachmentSizeBytes)})
                      </span>
                    )}
                  </div>
                  <button
                    type="button"
                    onClick={() => {
                      setAttachmentPath("");
                      setAttachmentSizeBytes(null);
                    }}
                    className="hover-menu shrink-0 rounded-md p-1 transition"
                    style={{ color: "var(--text-secondary)" }}
                    title={t("schedRemoveAttachmentTitle")}
                    aria-label={t("schedRemoveAttachmentTitle")}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}
            </div>

            {/* Date & Time */}
            <div>
              <div className="mb-1.5 flex flex-wrap items-center justify-between gap-1">
                <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                  Thời gian gửi
                </label>
                <div className="flex flex-wrap gap-1">
                  {[
                    { id: "10m" as const, label: "+10 phút" },
                    { id: "1h" as const, label: "+1 giờ" },
                    { id: "today18" as const, label: "18:00" },
                    { id: "tomorrow9" as const, label: "9:00 mai" },
                  ].map((preset) => (
                    <button
                      key={preset.id}
                      type="button"
                      onClick={() => applyTimePreset(preset.id)}
                      className="hover-menu rounded-md px-2 py-0.5 text-[11px] font-medium"
                      style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
                    >
                      {preset.label}
                    </button>
                  ))}
                </div>
              </div>
              <input
                type="datetime-local"
                value={dateTimeStr}
                onChange={(e) => setDateTimeStr(e.target.value)}
                className="w-full rounded-xl px-3 py-2 text-xs outline-none"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              />
            </div>

            {/* Recurrence */}
            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Lặp lại
              </label>
              <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-4 md:grid-cols-2">
                {RECURRENCE_OPTIONS.map((opt) => {
                  const active = recurrence === opt.value;
                  return (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => setRecurrence(opt.value)}
                      className="rounded-lg px-2.5 py-1.5 text-xs font-medium transition"
                      style={{
                        background: active ? "var(--bg-active)" : "var(--bg-surface)",
                        color: active ? "var(--text-on-active)" : "var(--text-primary)",
                        border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                      }}
                    >
                      {opt.label}
                    </button>
                  );
                })}
              </div>
            </div>

            {/* Misfire Policy */}
            <div>
              <label
                className="mb-1 block text-xs font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                Nếu quá giờ (máy tắt / mất kết nối)
              </label>
              <select
                value={misfirePolicy}
                onChange={(e) => setMisfirePolicy(e.target.value as ScheduleMisfirePolicy)}
                className="w-full rounded-xl px-3 py-2 text-xs outline-none"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {MISFIRE_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
              <p className="mt-1 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                {MISFIRE_OPTIONS.find((o) => o.value === misfirePolicy)?.desc}
              </p>
            </div>

            {/* Feedback */}
            {feedback && (
              <div
                className="flex items-start gap-2 rounded-xl px-3 py-2 text-xs font-medium"
                style={{
                  background:
                    feedback.tone === "ok"
                      ? "rgba(34, 197, 94, 0.12)"
                      : feedback.tone === "warn"
                        ? "rgba(245, 158, 11, 0.14)"
                        : "var(--danger-bg)",
                  color:
                    feedback.tone === "ok"
                      ? "#4ade80"
                      : feedback.tone === "warn"
                        ? "#fbbf24"
                        : "var(--danger-text)",
                }}
              >
                {feedback.tone === "ok" ? (
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                ) : (
                  <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                )}
                <span className="wrap-anywhere">{feedback.text}</span>
              </div>
            )}

            <button
              type="submit"
              disabled={submitting || !hasDraftContent}
              className="tf-btn mt-auto inline-flex w-full items-center justify-center gap-2 rounded-xl px-4 py-2.5 text-xs font-semibold disabled:opacity-50"
            >
              {submitting ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <CalendarClock className="h-3.5 w-3.5" />
              )}
              <span>{editingItem ? "Lưu thay đổi lịch gửi" : "Lên lịch gửi tin"}</span>
            </button>
          </form>

          {/* Right Column: Scheduled Messages List */}
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            {/* Filter Tabs */}
            <div
              className="flex shrink-0 items-center gap-1.5 overflow-x-auto px-4 py-3"
              style={{ borderBottom: "1px solid var(--border-subtle)" }}
            >
              {(
                [
                  { id: "pending" as const, label: "Chờ gửi", count: tabCounts.pending },
                  { id: "sent" as const, label: "Đã gửi", count: tabCounts.sent },
                  { id: "attention" as const, label: "Lỗi & Bỏ lỡ", count: tabCounts.attention },
                  { id: "all" as const, label: "Tất cả", count: tabCounts.all },
                ]
              ).map((tab) => {
                const active = filterTab === tab.id;
                return (
                  <button
                    key={tab.id}
                    type="button"
                    onClick={() => setFilterTab(tab.id)}
                    className="inline-flex shrink-0 items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-medium transition"
                    style={{
                      background: active ? "var(--accent-soft)" : "var(--bg-surface)",
                      color: active ? "var(--accent)" : "var(--text-secondary)",
                      border: `1px solid ${active ? "var(--accent)" : "var(--border-subtle)"}`,
                    }}
                  >
                    <span>{tab.label}</span>
                    <span
                      className="rounded-full px-1.5 py-0.5 font-mono text-[10.5px]"
                      style={{ background: "var(--card-bg)" }}
                    >
                      {tab.count}
                    </span>
                  </button>
                );
              })}
            </div>

            {/* List Content */}
            <div className="min-h-0 flex-1 space-y-2.5 overflow-y-auto p-4">
              {loading && schedules.length === 0 ? (
                <div
                  className="flex items-center justify-center gap-2 py-12 text-xs"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
                  <span>Đang tải danh sách lịch gửi...</span>
                </div>
              ) : filteredSchedules.length === 0 ? (
                <div
                  className="flex flex-col items-center justify-center gap-2 rounded-2xl py-12 text-center"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px dashed var(--border-subtle)",
                    color: "var(--text-secondary)",
                  }}
                >
                  <Clock className="h-7 w-7 opacity-60" />
                  <div className="text-xs font-medium">Không có tin nhắn hẹn giờ nào trong mục này</div>
                  <div className="max-w-xs text-[11.5px] opacity-80">
                    Tạo lịch mới ở khung bên trái để Teza tự động gửi tin đúng thời điểm.
                  </div>
                </div>
              ) : (
                filteredSchedules.map((item) => {
                  const isBusy = busyRowId === item.id;
                  const canSendNow =
                    item.status === "pending" ||
                    item.status === "missed" ||
                    item.status === "failed";
                  const canEdit =
                    item.status === "pending" ||
                    item.status === "missed" ||
                    item.status === "failed";
                  const canCancel = item.status === "pending";
                  const canDelete = item.status !== "sending";
                  const isEditingThis = editingItem?.id === item.id;
                  const itemAttachmentPath = (item.attachment_path || "").trim();
                  const itemAttachmentName = extractAttachmentBasename(itemAttachmentPath);
                  const hasItemAttachment = Boolean(itemAttachmentPath);

                  return (
                    <div
                      key={item.id}
                      className="space-y-2.5 rounded-xl p-3.5 transition"
                      style={{
                        background: "var(--bg-surface)",
                        border: `1px solid ${
                          isEditingThis ? "var(--accent)" : "var(--border-subtle)"
                        }`,
                      }}
                    >
                      {/* Top row */}
                      <div className="flex flex-wrap items-start justify-between gap-2">
                        <div className="min-w-0 flex-1">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="truncate text-[13.5px] font-semibold">
                              {item.thread_title || item.thread_id}
                            </span>
                            <span
                              className="rounded-full px-2 py-0.5 text-[10.5px] font-medium"
                              style={{ background: "var(--chip-bg)", color: "var(--text-secondary)" }}
                            >
                              {formatThreadKindLabel(item.thread_kind)}
                            </span>
                            <span
                              className="font-mono text-[10.5px]"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              #{item.thread_id}
                            </span>
                          </div>
                        </div>

                        <span
                          className="shrink-0 rounded-full px-2.5 py-0.5 text-[11px] font-semibold"
                          style={badgeStyle(getScheduleStatusTone(item.status))}
                        >
                          {formatScheduleStatus(item.status)}
                        </span>
                      </div>

                      {/* Message & attachment preview */}
                      <div
                        className="space-y-1.5 rounded-lg px-3 py-2 text-xs leading-relaxed"
                        style={{
                          background: "var(--card-bg)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        {hasItemAttachment && (
                          <div
                            className="inline-flex max-w-full items-center gap-1.5 rounded-md px-2 py-1 text-[11.5px] font-medium"
                            style={{
                              background: "var(--accent-soft)",
                              color: "var(--accent)",
                            }}
                            title={itemAttachmentPath}
                          >
                            {isImageAttachmentPath(itemAttachmentPath) ? (
                              <ImageIcon className="h-3.5 w-3.5 shrink-0" />
                            ) : (
                              <FileText className="h-3.5 w-3.5 shrink-0" />
                            )}
                            <span className="truncate">
                              {itemAttachmentName || t("attachmentFallbackLabel")}
                            </span>
                          </div>
                        )}
                        {item.text.trim() ? (
                          <div className="selectable-text wrap-anywhere whitespace-pre-wrap">
                            {item.text}
                          </div>
                        ) : !hasItemAttachment ? (
                          <div style={{ color: "var(--text-secondary)" }}>—</div>
                        ) : null}
                      </div>

                      {/* Metadata row */}
                      <div
                        className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11.5px]"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <span className="inline-flex items-center gap-1">
                          <Clock className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
                          <span>Gửi lúc: {formatLocalDateTime(item.send_at_ts)}</span>
                        </span>
                        <span className="inline-flex items-center gap-1">
                          <Repeat className="h-3.5 w-3.5" />
                          <span>{formatScheduleRecurrence(item.recurrence)}</span>
                        </span>
                        <span>· {formatScheduleMisfirePolicy(item.misfire_policy)}</span>
                        {item.sent_at_ts > 0 && (
                          <span className="text-emerald-400">
                            · Đã gửi: {formatLocalDateTime(item.sent_at_ts)}
                          </span>
                        )}
                      </div>

                      {/* Error notice */}
                      {item.last_error && (
                        <div
                          className="selectable-text wrap-anywhere flex items-start gap-1.5 rounded-lg px-2.5 py-1.5 text-[11.5px]"
                          style={{
                            background: "var(--danger-bg)",
                            color: "var(--danger-text)",
                          }}
                        >
                          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                          <span>
                            {item.last_error}
                            {item.retry_count > 0 ? ` (đã thử lại ${item.retry_count} lần)` : ""}
                          </span>
                        </div>
                      )}

                      {/* Actions */}
                      <div className="flex flex-wrap items-center justify-end gap-1.5 pt-0.5">
                        {canSendNow && (
                          <button
                            type="button"
                            disabled={Boolean(busyRowId)}
                            onClick={() =>
                              void runRowAction(
                                item.id,
                                () => onSendNow(item.id),
                                "Đã đưa tin nhắn vào hàng đợi gửi ngay.",
                                {
                                  expectTruthy: true,
                                  onSuccess: () => {
                                    if (item.status !== "pending") {
                                      setFilterTab("pending");
                                    }
                                  },
                                },
                              )
                            }
                            className="tf-btn inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-[11.5px] font-semibold disabled:opacity-50"
                          >
                            {isBusy ? (
                              <Loader2 className="h-3 w-3 animate-spin" />
                            ) : (
                              <Send className="h-3 w-3" />
                            )}
                            <span>Gửi ngay</span>
                          </button>
                        )}

                        {canEdit && (
                          <button
                            type="button"
                            disabled={Boolean(busyRowId)}
                            onClick={() => startEditing(item)}
                            className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-50"
                            style={{
                              border: "1px solid var(--border-subtle)",
                              color: "var(--text-primary)",
                            }}
                          >
                            <Pencil className="h-3 w-3" />
                            <span>Sửa</span>
                          </button>
                        )}

                        {canCancel && (
                          <button
                            type="button"
                            disabled={Boolean(busyRowId)}
                            onClick={() =>
                              void runRowAction(
                                item.id,
                                () => onCancel(item.id),
                                "Đã hủy lịch gửi tin nhắn.",
                              )
                            }
                            className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-50"
                            style={{
                              border: "1px solid var(--border-subtle)",
                              color: "#fbbf24",
                            }}
                          >
                            <Ban className="h-3 w-3" />
                            <span>Hủy lịch</span>
                          </button>
                        )}

                        {canDelete && (
                          <button
                            type="button"
                            disabled={Boolean(busyRowId)}
                            onClick={() =>
                              void runRowAction(
                                item.id,
                                () => onDelete(item.id),
                                "Đã xóa lịch gửi tin nhắn.",
                              )
                            }
                            className="hover-menu inline-flex items-center gap-1 rounded-lg px-2.5 py-1 text-[11.5px] font-medium disabled:opacity-50"
                            style={{
                              background: "var(--danger-bg)",
                              color: "var(--danger-text)",
                            }}
                          >
                            <Trash2 className="h-3 w-3" />
                            <span>Xóa</span>
                          </button>
                        )}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default ScheduledMessagesModal;
