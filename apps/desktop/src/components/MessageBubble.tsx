import { memo, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import {
  BarChart3,
  CalendarClock,
  Check,
  CheckCheck,
  CheckSquare,
  Copy,
  CornerUpLeft,
  Download,
  FileText,
  Forward,
  Image as ImageIcon,
  Languages,
  Loader2,
  MessagesSquare,
  Pin,
  RefreshCw,
  Repeat,
  RotateCcw,
  Smile,
  Trash2,
  Zap,
} from "lucide-react";
import type { MessageDto, ThreadKind } from "../types";
import { useI18n } from "../i18n";
import { isBuzzMessage } from "../utils/buzz";
import { copyImageReliable, copyTextReliable } from "../utils/clipboard";
import {
  formatReminderCountdown,
  parsePollPayload,
  parseReminderPayload,
} from "../utils/richMessage";
import { getDisplayBodyText, splitTextLinks } from "../utils/threadState";
import { RelayAudio, RelayImage, RelayVideo } from "./RelayImage";
import { ThreadAvatar } from "./ThreadAvatar";

export interface MessageTranslationState {
  text?: string;
  src?: string | null;
  targetLang?: string;
  loading?: boolean;
  error?: string;
}

const QUICK_EMOJIS = ["👍", "❤️", "😆", "😮", "😢", "😠"];

interface Props {
  msg: MessageDto;
  threadKind: ThreadKind;
  groupedWithPrev: boolean;
  groupedWithNext?: boolean;
  highlighted: boolean;
  isSeen?: boolean;
  senderAvatar?: string;
  translation?: MessageTranslationState;
  replaceWithTranslation?: boolean;
  replyCount?: number;
  hideQuote?: boolean;
  highlightQuery?: string;
  selectMode?: boolean;
  selected?: boolean;
  onToggleSelect?: (msgId: string) => void;
  onStartSelect?: (msgId: string) => void;
  onTogglePin?: (msg: MessageDto) => void;
  onReply: (msg: MessageDto) => void;
  onReact: (msg: MessageDto, emoji: string) => void;
  onRecall: (msg: MessageDto) => void;
  onDeleteLocal: (msg: MessageDto) => void;
  onJumpToMsg: (msgId: string) => void;
  onOpenImage: (url: string, title: string) => void;
  onOpenProfile?: (userId: string) => void;
  onForward?: (msg: MessageDto) => void;
  onTranslate?: (msg: MessageDto) => void;
  onHideTranslation?: (msgId: string) => void;
  onOpenThread?: (msg: MessageDto) => void;
  onRetrySend?: (msg: MessageDto) => void;
  onDiscardFailed?: (msgId: string) => void;
  onSaveFile?: (url: string, name: string) => void;
  onCopyError?: (err: unknown) => void;
  onVotePoll?: (msg: MessageDto, optionIds: number[]) => Promise<void> | void;
  onRefreshPoll?: (msg: MessageDto) => Promise<void> | void;
}

function formatClock(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
}

function isStickerMessage(msg: MessageDto): boolean {
  const t = msg.msg_type.toLowerCase();
  return (
    !!msg.attachment_url &&
    (t.includes("sticker") || t === "5" || t === "35" || t === "36")
  );
}

function isVoiceAttachment(msg: MessageDto): boolean {
  return (
    !!msg.attachment_url &&
    /^https:\/\//i.test(msg.attachment_url) &&
    (msg.msg_type.includes("voice") || msg.msg_type === "3")
  );
}

function isVideoAttachment(msg: MessageDto): boolean {
  return (
    !!msg.attachment_url &&
    /^https:\/\//i.test(msg.attachment_url) &&
    (msg.msg_type.includes("video") || msg.msg_type === "4")
  );
}

function renderHighlightedPlain(text: string, query?: string) {
  const q = query?.trim();
  if (!q) return text;
  const escaped = q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`(${escaped})`, "gi");
  const segments = text.split(re);
  if (segments.length <= 1) return text;
  const qLower = q.toLowerCase();
  return segments.map((seg, i) =>
    seg.toLowerCase() === qLower ? (
      <mark key={i} className="rounded bg-amber-400/35 px-0.5 text-inherit">
        {seg}
      </mark>
    ) : (
      seg
    ),
  );
}

function renderMentionsAndSearch(text: string, isOutgoing: boolean, highlightQuery?: string) {
  const parts = text.split(/((?:^|\s)@[^\s@<>]+)/g);
  return parts.map((part, idx) => {
    const m = part.match(/^(\s*)(@[^\s@<>]+)$/);
    if (m) {
      return (
        <span key={idx}>
          {m[1]}
          <span
            className="font-semibold"
            style={{ color: isOutgoing ? "var(--link-out)" : "var(--accent)" }}
          >
            {renderHighlightedPlain(m[2], highlightQuery)}
          </span>
        </span>
      );
    }
    return <span key={idx}>{renderHighlightedPlain(part, highlightQuery)}</span>;
  });
}

function renderLinkedText(text: string, isOutgoing: boolean, highlightQuery?: string) {
  const parts = splitTextLinks(text);
  return parts.map((part, idx) =>
    part.url ? (
      <a
        key={idx}
        href={part.url}
        target="_blank"
        rel="noreferrer"
        className="underline underline-offset-2 hover:opacity-80"
        style={{ color: isOutgoing ? "var(--link-out)" : "var(--link-in)" }}
      >
        {part.text}
      </a>
    ) : (
      <span key={idx}>{renderMentionsAndSearch(part.text, isOutgoing, highlightQuery)}</span>
    ),
  );
}

function isImageAttachment(msg: MessageDto): boolean {
  if (!msg.attachment_url) return false;
  if (isStickerMessage(msg)) return false;
  if (msg.msg_type.includes("photo") || msg.msg_type === "2") return true;
  if (msg.attachment_url.startsWith("data:image/")) return true;
  return /\.(png|jpe?g|webp|gif|bmp)(\?|$)/i.test(msg.attachment_url);
}

export const MessageBubble = memo(function MessageBubble({
  msg,
  threadKind,
  groupedWithPrev,
  groupedWithNext = false,
  highlighted,
  isSeen = false,
  senderAvatar,
  translation,
  replaceWithTranslation = false,
  replyCount = 0,
  hideQuote = false,
  highlightQuery = "",
  selectMode = false,
  selected = false,
  onToggleSelect,
  onStartSelect,
  onTogglePin,
  onReply,
  onReact,
  onRecall,
  onDeleteLocal,
  onJumpToMsg,
  onOpenImage,
  onOpenProfile,
  onForward,
  onTranslate,
  onHideTranslation,
  onOpenThread,
  onRetrySend,
  onDiscardFailed,
  onSaveFile,
  onCopyError,
  onVotePoll,
  onRefreshPoll,
}: Props) {
  const { lang, locale, t } = useI18n();
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [pickerPos, setPickerPos] = useState<{ left: number; top: number } | null>(null);
  const [mediaError, setMediaError] = useState(false);
  const [showOriginal, setShowOriginal] = useState(false);
  const [pollBusy, setPollBusy] = useState(false);

  useEffect(() => {
    setMediaError(false);
  }, [msg.attachment_url, msg.msg_type]);
  const isReplaced = Boolean(replaceWithTranslation && translation?.text && !showOriginal);
  const reminderData = !msg.is_recalled ? parseReminderPayload(msg) : null;
  const pollData = !msg.is_recalled && !reminderData ? parsePollPayload(msg) : null;
  const isRichCard = Boolean(reminderData || pollData);
  const isBuzz = !isRichCard && isBuzzMessage(msg);
  const isSticker = !msg.is_recalled && !isRichCard && isStickerMessage(msg) && !mediaError;
  const isVoice = !msg.is_recalled && !isRichCard && isVoiceAttachment(msg) && !mediaError;
  const isVideo = !msg.is_recalled && !isRichCard && isVideoAttachment(msg) && !mediaError;
  const isImage =
    !msg.is_recalled && !isRichCard && !isVoice && !isVideo && isImageAttachment(msg) && !mediaError;
  const isFileAttachment =
    !msg.is_recalled &&
    !isRichCard &&
    !isSticker &&
    !isVoice &&
    !isVideo &&
    !isImage &&
    (!!msg.attachment_url ||
      msg.msg_type.includes("file") ||
      msg.msg_type === "19");
  const displayText = msg.is_recalled
    ? msg.text
    : isReplaced && translation?.text
      ? translation.text
      : getDisplayBodyText(msg);
  const isPlainSticker =
    isSticker &&
    !msg.quote &&
    !msg.is_recalled &&
    !displayText &&
    !isBuzz &&
    !(threadKind === "group" && !msg.is_outgoing && !groupedWithPrev);
  const safeDownloadUrl =
    msg.attachment_url && /^https:\/\//i.test(msg.attachment_url)
      ? msg.attachment_url
      : null;

  useEffect(() => {
    if (!menuPos && !pickerPos) return;
    const close = () => {
      setMenuPos(null);
      setPickerPos(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [menuPos, pickerPos]);

  const handleCopy = () => {
    const textToCopy = isReplaced && translation?.text ? translation.text : msg.text;
    void copyTextReliable(textToCopy).catch((err) => onCopyError?.(err));
    setMenuPos(null);
  };

  const handleCopyImage = () => {
    if (!msg.attachment_url) return;
    const url = msg.attachment_url;
    setMenuPos(null);
    void copyImageReliable(url).catch((err) => onCopyError?.(err));
  };

  const bubbleCornerClass = msg.is_outgoing
    ? groupedWithPrev && groupedWithNext
      ? "rounded-2xl rounded-r-[6px]"
      : groupedWithPrev && !groupedWithNext
        ? "rounded-2xl rounded-tr-[6px] rounded-br-[4px]"
        : !groupedWithPrev && groupedWithNext
          ? "rounded-2xl rounded-br-[6px]"
          : "rounded-2xl rounded-br-[4px]"
    : groupedWithPrev && groupedWithNext
      ? "rounded-2xl rounded-l-[6px]"
      : groupedWithPrev && !groupedWithNext
        ? "rounded-2xl rounded-tl-[6px] rounded-bl-[4px]"
        : !groupedWithPrev && groupedWithNext
          ? "rounded-2xl rounded-bl-[6px]"
          : "rounded-2xl rounded-bl-[4px]";

  const hasTranslationPanel = Boolean(
    translation && !isReplaced && (!replaceWithTranslation || showOriginal || translation.error),
  );
  const useInlineMeta = Boolean(
    displayText &&
      !isBuzz &&
      !isRichCard &&
      !isPlainSticker &&
      !hasTranslationPanel &&
      msg.reactions.length === 0 &&
      !isReplaced &&
      !(replaceWithTranslation && translation?.loading) &&
      msg.send_status !== "failed" &&
      !(replyCount > 0 && onOpenThread),
  );
  const inlineMetaWidthPx =
    48 +
    (msg.is_outgoing && !msg.is_recalled ? (isSeen ? 18 : 16) : 0) +
    (msg.is_pinned ? 16 : 0);

  const timestampEl = (
    <span
      className={`inline-flex select-none items-center gap-1 text-[11px] leading-none tabular-nums ${
        isPlainSticker ? "rounded-full px-2 py-0.5" : ""
      }`}
      style={{
        color: isPlainSticker ? "var(--pill-text)" : "var(--text-secondary)",
        background: isPlainSticker ? "var(--pill-bg)" : undefined,
      }}
    >
      {msg.is_pinned && (
        <span title={t("pinnedBadgeTitle")} className="inline-flex items-center">
          <Pin
            className="h-3 w-3 rotate-45"
            style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
          />
        </span>
      )}
      <span>{formatClock(msg.ts)}</span>
      {msg.is_outgoing && !msg.is_recalled && msg.send_status === "sending" && (
        <span title={t("sendingStatus")}>
          <Loader2 className="h-3 w-3 animate-spin" />
        </span>
      )}
      {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
        isSeen ? (
          <span title={t("youSeenStatus")}>
            <CheckCheck className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
          </span>
        ) : (
          <span title={t("youSentStatus")}>
            <Check className="h-3 w-3 opacity-80" />
          </span>
        )
      )}
    </span>
  );

  return (
    <div
      id={`msg-${msg.msg_id}`}
      onClick={selectMode && !msg.send_status ? () => onToggleSelect?.(msg.msg_id) : undefined}
      className={`group relative flex ${msg.is_outgoing ? "justify-end" : "justify-start"} ${
        selectMode ? "cursor-pointer rounded-xl px-1.5 py-0.5 transition" : ""
      }`}
      style={{
        marginTop: groupedWithPrev ? "var(--msg-gap-grouped)" : "var(--msg-gap-group)",
        ...(selectMode && selected ? { background: "var(--accent-soft)" } : undefined),
      }}
    >
      {selectMode && (
        <div className="order-0 mr-2.5 flex items-center self-center">
          <div
            className={`flex h-5 w-5 items-center justify-center rounded-full border transition ${
              msg.send_status ? "opacity-35" : ""
            }`}
            style={{
              background: selected ? "var(--accent)" : "transparent",
              borderColor: selected ? "var(--accent)" : "var(--text-secondary)",
            }}
          >
            {selected && <Check className="h-3 w-3 text-white" />}
          </div>
        </div>
      )}

      {!msg.is_outgoing && threadKind === "group" && (
        <div className="order-1 mb-0.5 mr-2 flex w-7 shrink-0 items-end justify-center self-end">
          {!groupedWithNext && (
            <button
              type="button"
              disabled={!onOpenProfile || !msg.sender_id}
              onClick={() => msg.sender_id && onOpenProfile?.(msg.sender_id)}
              title={t("viewProfileTitle")}
              className="rounded-full transition hover:opacity-85"
            >
              <ThreadAvatar
                avatar={senderAvatar || ""}
                title={msg.sender_name}
                kind="direct"
                size="xs"
              />
            </button>
          )}
        </div>
      )}

      {/* Quick hover actions (left side for outgoing, right side for incoming) */}
      {!selectMode && !msg.is_recalled && !msg.send_status && (
        <div
          onClick={(e) => e.stopPropagation()}
          className={`flex items-center gap-1 self-center opacity-0 transition group-hover:opacity-100 ${
            msg.is_outgoing ? "order-1 mr-2" : "order-3 ml-2"
          }`}
        >
          <button
            onClick={() => onReply(msg)}
            title={t("replyTitle")}
            className="hover-surface rounded-full p-1.5 transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            <CornerUpLeft className="h-3.5 w-3.5" />
          </button>
          {onOpenThread && (
            <button
              onClick={() => onOpenThread(msg)}
              title={replyCount > 0 ? t("viewThreadReplies", { count: replyCount }) : t("openThreadReplies")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{
                background: "var(--bg-surface)",
                color: replyCount > 0 ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              <MessagesSquare className="h-3.5 w-3.5" />
            </button>
          )}
          {onTranslate && msg.text.trim() && !(isSticker && msg.text === "[Nhãn dán]") && (
            <button
              onClick={() =>
                translation?.text ? onHideTranslation?.(msg.msg_id) : onTranslate(msg)
              }
              title={translation?.text ? t("hideTranslation") : t("translateMessage")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{
                background: "var(--bg-surface)",
                color: translation?.text ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              <Languages className="h-3.5 w-3.5" />
            </button>
          )}
          {onForward && (
            <button
              onClick={() => onForward(msg)}
              title={t("forwardMessage")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Forward className="h-3.5 w-3.5" />
            </button>
          )}
          <div className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                if (pickerPos) {
                  setPickerPos(null);
                  return;
                }
                const rect = e.currentTarget.getBoundingClientRect();
                const pickerW = 240;
                const left = Math.max(
                  12,
                  Math.min(
                    rect.left + rect.width / 2 - pickerW / 2,
                    window.innerWidth - pickerW - 12,
                  ),
                );
                const top =
                  rect.top >= 58
                    ? rect.top - 48
                    : Math.min(window.innerHeight - 52, rect.bottom + 8);
                setMenuPos(null);
                setPickerPos({ left, top });
              }}
              title={t("reactTitle")}
              className="rounded-full p-1.5 hover:opacity-80"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Smile className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      )}

      {pickerPos &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            onClick={(e) => e.stopPropagation()}
            className="fixed z-[80] flex items-center gap-1 rounded-full px-2 py-1 shadow-xl"
            style={{
              left: `${pickerPos.left}px`,
              top: `${pickerPos.top}px`,
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            {QUICK_EMOJIS.map((emoji) => {
              const active = msg.reactions.some((r) => r.emoji === emoji && r.me);
              return (
                <button
                  key={emoji}
                  onClick={() => {
                    onReact(msg, emoji);
                    setPickerPos(null);
                  }}
                  title={active ? t("reactToggleRemoveTitle") : t("reactWithTitle", { emoji })}
                  className="rounded-full p-1 text-sm transition hover:scale-125"
                  style={
                    active
                      ? {
                          background: "rgba(56, 189, 248, 0.24)",
                          outline: "1px solid var(--accent)",
                        }
                      : undefined
                  }
                >
                  {emoji}
                </button>
              );
            })}
          </div>,
          document.body,
        )}

      <div
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setPickerPos(null);
          setMenuPos({
            x: Math.min(e.clientX, window.innerWidth - 210),
            y: Math.min(e.clientY, window.innerHeight - 390),
          });
        }}
        onDoubleClick={(e) => {
          if (selectMode || msg.is_recalled || msg.send_status) return;
          if ((window.getSelection()?.toString().trim().length ?? 0) > 0) return;
          const el = e.target instanceof Element ? e.target : null;
          if (el?.closest("a, button, input, textarea, audio, video")) return;
          onReply(msg);
        }}
        className={`selectable-text wrap-anywhere relative order-2 min-w-0 max-w-[72%] transition ${
          isPlainSticker
            ? "rounded-2xl px-1 py-0.5"
            : `px-3 shadow-sm ${bubbleCornerClass}`
        } ${highlighted ? "ring-2 ring-sky-400" : ""} ${
          msg.send_status === "sending" ? "opacity-80" : ""
        }`}
        style={{
          paddingBlock: isPlainSticker ? undefined : "var(--bubble-py)",
          background: isPlainSticker
            ? "transparent"
            : msg.is_outgoing
              ? "var(--bg-bubble-out)"
              : "var(--bg-bubble-in)",
          border:
            msg.send_status === "failed"
              ? "1px solid var(--danger-text)"
              : isPlainSticker
                ? "none"
                : msg.is_outgoing
                  ? "1px solid var(--bubble-out-border)"
                  : "1px solid var(--bubble-in-border)",
        }}
      >
        {!msg.is_outgoing && threadKind === "group" && !groupedWithPrev && (
          <button
            type="button"
            disabled={!onOpenProfile || !msg.sender_id}
            onClick={() => msg.sender_id && onOpenProfile?.(msg.sender_id)}
            className="mb-0.5 block text-left text-[12.5px] font-semibold leading-tight hover:underline"
            style={{ color: "var(--accent)" }}
            title={t("viewProfileTitle")}
          >
            {msg.sender_name}
          </button>
        )}

        {!hideQuote && msg.quote && !msg.is_recalled && (
          <button
            type="button"
            onClick={() => msg.quote?.msg_id && onJumpToMsg(msg.quote.msg_id)}
            className="mb-1 block w-full rounded-lg border-l-2 px-2.5 py-1 text-left text-[12.5px] transition hover:opacity-85"
            style={{
              borderColor: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
              background: "var(--quote-bg)",
            }}
          >
            <div
              className="font-semibold"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            >
              {msg.quote.sender_name || t("originalMessageFallback")}
            </div>
            <div className="truncate" style={{ color: "var(--text-secondary)" }}>
              {msg.quote.snippet}
            </div>
          </button>
        )}

        {isSticker && msg.attachment_url && (
          <RelayImage
            src={msg.attachment_url}
            alt="Nhãn dán"
            referrerPolicy="no-referrer"
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 h-32 w-32 object-contain drop-shadow-sm"
            loading="lazy"
          />
        )}

        {isVoice && msg.attachment_url && (
          <RelayAudio
            controls
            preload="none"
            src={msg.attachment_url}
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 max-w-full"
          />
        )}

        {isVideo && msg.attachment_url && (
          <RelayVideo
            controls
            preload="metadata"
            src={msg.attachment_url}
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 max-h-72 w-full rounded-xl bg-black/40"
          />
        )}

        {isImage && msg.attachment_url && (
          <button
            type="button"
            onClick={() => onOpenImage(msg.attachment_url!, msg.attachment_name || "Hình ảnh")}
            className="mb-1 block overflow-hidden rounded-xl"
          >
            <RelayImage
              src={msg.attachment_url}
              alt={msg.attachment_name || "Hình ảnh"}
              referrerPolicy="no-referrer"
              onFallbackFail={() => setMediaError(true)}
              className="max-h-72 w-auto rounded-xl object-cover transition hover:brightness-95"
              loading="lazy"
            />
          </button>
        )}

        {isFileAttachment &&
          (safeDownloadUrl && onSaveFile ? (
            <button
              type="button"
              onClick={() => onSaveFile(safeDownloadUrl, msg.attachment_name || "download")}
              title={t("clickToDownloadFile")}
              className="mb-1 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left transition hover:brightness-95"
              style={{ background: "var(--card-bg)" }}
            >
              <FileText
                className="h-5 w-5 shrink-0"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px] font-medium">
                  {msg.attachment_name || t("attachedFileTitle")}
                </div>
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  {t("attachedFileHint")}
                </div>
              </div>
              <span
                className="rounded-lg p-1.5"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              >
                <Download className="h-4 w-4" />
              </span>
            </button>
          ) : (
            <div
              className="mb-1 flex items-center gap-2.5 rounded-xl px-3 py-2"
              style={{ background: "var(--card-bg)" }}
            >
              <FileText
                className="h-5 w-5 shrink-0"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px] font-medium">
                  {msg.attachment_name || t("attachedFileTitle")}
                </div>
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  {t("attachedFileSub")}
                </div>
              </div>
              {safeDownloadUrl && (
                <a
                  href={safeDownloadUrl}
                  target="_blank"
                  rel="noreferrer"
                  download={msg.attachment_name || undefined}
                  title={t("downloadFileTitle")}
                  className="hover-menu rounded-lg p-1.5 transition"
                  style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                >
                  <Download className="h-4 w-4" />
                </a>
              )}
            </div>
          ))}

        {reminderData ? (
          <div
            className="my-0.5 flex min-w-[240px] max-w-[330px] items-stretch gap-3 rounded-xl p-2.5"
            style={{
              background: msg.is_outgoing ? "rgba(0,0,0,0.16)" : "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div
              className="flex w-12 shrink-0 flex-col items-center justify-center rounded-xl py-1.5 text-center"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
            >
              {reminderData.startTime > 0 ? (
                <>
                  <span className="text-[10px] font-bold uppercase tracking-wider">
                    T{new Date(reminderData.startTime).getMonth() + 1}
                  </span>
                  <span className="text-lg font-extrabold leading-tight tabular-nums">
                    {String(new Date(reminderData.startTime).getDate()).padStart(2, "0")}
                  </span>
                </>
              ) : (
                <CalendarClock className="h-5 w-5" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div
                className="flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wider"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              >
                <CalendarClock className="h-3.5 w-3.5 shrink-0" />
                <span>{t("reminderTitleLabel")}</span>
              </div>
              <div className="mt-0.5 text-[14px] font-semibold leading-snug">
                {reminderData.title}
              </div>
              <div
                className="mt-1 flex flex-wrap items-center gap-1.5 text-[11.5px]"
                style={{ color: "var(--text-secondary)" }}
              >
                {(reminderData.scheduleText || reminderData.startTime > 0) && (
                  <span>
                    {reminderData.scheduleText ||
                      new Date(reminderData.startTime).toLocaleString(locale, {
                        hour: "2-digit",
                        minute: "2-digit",
                        day: "2-digit",
                        month: "2-digit",
                        year: "numeric",
                      })}
                  </span>
                )}
                {reminderData.repeat > 0 && (
                  <span
                    className="inline-flex items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[10.5px] font-medium"
                    style={{ background: "var(--bg-sidebar)" }}
                  >
                    <Repeat className="h-3 w-3" />
                    {reminderData.repeat === 1
                      ? t("repeatDaily")
                      : reminderData.repeat === 2
                        ? t("repeatWeekly")
                        : t("repeatMonthly")}
                  </span>
                )}
              </div>
              {reminderData.startTime > 0 && (
                <div
                  className="mt-1 inline-block rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  {formatReminderCountdown(reminderData.startTime, Date.now(), lang)}
                </div>
              )}
            </div>
          </div>
        ) : pollData ? (
          <div
            className="my-0.5 min-w-[250px] max-w-[340px] rounded-xl p-2.5"
            style={{
              background: msg.is_outgoing ? "rgba(0,0,0,0.16)" : "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div className="flex items-start justify-between gap-2">
              <div
                className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              >
                <BarChart3 className="h-3.5 w-3.5 shrink-0" />
                <span>{t("pollTitleLabel")}</span>
              </div>
              {onRefreshPoll && pollData.poll_id > 0 && (
                <button
                  type="button"
                  disabled={pollBusy}
                  onClick={() => {
                    if (pollBusy) return;
                    setPollBusy(true);
                    void Promise.resolve(onRefreshPoll(msg)).finally(() => setPollBusy(false));
                  }}
                  title={t("pollRefreshTitle")}
                  className="hover-menu rounded p-1 opacity-75 hover:opacity-100 disabled:opacity-40"
                >
                  <RefreshCw className={`h-3 w-3 ${pollBusy ? "animate-spin" : ""}`} />
                </button>
              )}
            </div>
            <div className="mt-1 text-[14px] font-semibold leading-snug">{pollData.question}</div>
            <div
              className="mt-1 flex flex-wrap gap-1 text-[10.5px]"
              style={{ color: "var(--text-secondary)" }}
            >
              <span>{pollData.allow_multi ? t("pollMultiChoice") : t("pollSingleChoice")}</span>
              {pollData.is_anonymous && <span>{t("pollAnonymous")}</span>}
              {pollData.closed && <span className="text-rose-400">{t("pollClosed")}</span>}
            </div>
            <div className="mt-2 space-y-1.5">
              {pollData.options.length === 0 ? (
                <div className="text-xs italic opacity-70">
                  {t("pollNoOptionsHint")}
                </div>
              ) : (
                (() => {
                  const totalVotes = pollData.options.reduce((s, o) => s + (o.votes || 0), 0);
                  return pollData.options.map((opt) => {
                    const pct = totalVotes > 0 ? Math.round((opt.votes / totalVotes) * 100) : 0;
                    const canVote = Boolean(onVotePoll && pollData.poll_id > 0 && !pollData.closed);
                    return (
                      <button
                        key={opt.id}
                        type="button"
                        disabled={!canVote || pollBusy}
                        onClick={() => {
                          if (!canVote || !onVotePoll || pollBusy) return;
                          const nextIds = pollData.allow_multi
                            ? opt.voted
                              ? pollData.options.filter((o) => o.voted && o.id !== opt.id).map((o) => o.id)
                              : [...pollData.options.filter((o) => o.voted).map((o) => o.id), opt.id]
                            : opt.voted
                              ? []
                              : [opt.id];
                          setPollBusy(true);
                          void Promise.resolve(onVotePoll(msg, nextIds)).finally(() =>
                            setPollBusy(false),
                          );
                        }}
                        className="relative block w-full overflow-hidden rounded-lg px-2.5 py-1.5 text-left text-xs transition hover:brightness-95 disabled:cursor-default"
                        style={{
                          background: "var(--bg-sidebar)",
                          border: opt.voted
                            ? "1px solid var(--accent)"
                            : "1px solid var(--border-subtle)",
                        }}
                      >
                        <div
                          aria-hidden="true"
                          className="pointer-events-none absolute inset-y-0 left-0 opacity-20 transition-all"
                          style={{
                            width: `${pct}%`,
                            background: "var(--accent)",
                          }}
                        />
                        <div className="relative flex items-center justify-between gap-2">
                          <span className="flex min-w-0 items-center gap-1.5 font-medium">
                            {opt.voted && (
                              <Check
                                className="h-3.5 w-3.5 shrink-0"
                                style={{ color: "var(--accent)" }}
                              />
                            )}
                            <span className="truncate">{opt.content}</span>
                          </span>
                          <span
                            className="shrink-0 text-[11px] font-semibold tabular-nums"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            {opt.votes}
                            {totalVotes > 0 ? ` (${pct}%)` : ""}
                          </span>
                        </div>
                      </button>
                    );
                  });
                })()
              )}
            </div>
          </div>
        ) : isBuzz ? (
          <div className="my-0.5 inline-flex items-center gap-2 rounded-xl bg-amber-500/20 px-3 py-1.5 font-extrabold tracking-wider text-amber-400 ring-1 ring-amber-400/40">
            <Zap className="h-4 w-4 animate-pulse fill-amber-400 text-amber-300" />
            <span className="text-[16px]">BUZZ!</span>
          </div>
        ) : (
          displayText && (
            <div
              className={`msg-body-text whitespace-pre-wrap wrap-anywhere leading-[1.38] ${
                msg.is_recalled ? "italic opacity-65" : ""
              } ${replaceWithTranslation && translation?.loading ? "opacity-75" : ""}`}
            >
              {renderLinkedText(displayText, msg.is_outgoing, highlightQuery)}
              {useInlineMeta && (
                <span
                  aria-hidden="true"
                  className="pointer-events-none inline-block h-[11px] select-none align-baseline"
                  style={{ width: `${inlineMetaWidthPx}px` }}
                />
              )}
            </div>
          )
        )}

        {translation && !isReplaced && (!replaceWithTranslation || showOriginal || translation.error) && (
          <div
            className="mt-2 border-t pt-1.5"
            style={{
              borderColor: msg.is_outgoing
                ? "rgba(255,255,255,0.22)"
                : "var(--border)",
            }}
          >
            <div className="mb-0.5 flex items-center justify-between gap-2 text-xs opacity-85">
              <span className="flex items-center gap-1 font-medium">
                <Languages className="h-3.5 w-3.5" />
                {translation.loading
                  ? t("translatingDots")
                  : translation.error
                    ? t("translateError")
                    : `${t("translateHeader")}${translation.src ? ` · ${translation.src}` : ""}${
                        translation.targetLang ? ` → ${translation.targetLang}` : ""
                      }`}
              </span>
              {replaceWithTranslation && translation.text ? (
                <button
                  type="button"
                  onClick={() => setShowOriginal(false)}
                  className="hover:underline"
                >
                  {t("viewTranslation")}
                </button>
              ) : (
                onHideTranslation &&
                !translation.loading && (
                  <button
                    type="button"
                    onClick={() => onHideTranslation(msg.msg_id)}
                    className="hover:underline"
                  >
                    {t("hideTranslation")}
                  </button>
                )
              )}
            </div>
            {translation.loading ? (
              <div className="flex items-center gap-1.5 text-[13.5px] opacity-80">
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                <span>{t("translatingBody")}</span>
              </div>
            ) : translation.error ? (
              <div
                className="text-[13px]"
                style={{ color: msg.is_outgoing ? "#fecaca" : "var(--danger-text)" }}
              >
                {translation.error}
              </div>
            ) : (
              <div className="whitespace-pre-wrap wrap-anywhere text-[15px] leading-[1.5] opacity-95">
                {translation.text}
              </div>
            )}
          </div>
        )}

        {useInlineMeta ? (
          <span
            className="absolute right-2.5 select-none"
            style={{ bottom: "var(--bubble-py)" }}
          >
            {timestampEl}
          </span>
        ) : (
          <div className="mt-0.5 flex items-center justify-end gap-2">
            {msg.reactions.length > 0 && (
              <div className="mr-auto flex flex-wrap gap-1">
                {msg.reactions.map((r) => (
                  <button
                    key={r.emoji}
                    onClick={() => onReact(msg, r.emoji)}
                    className="rounded-full px-2 py-0.5 text-[13px] transition hover:opacity-85"
                    style={{
                      background: r.me ? "var(--accent-soft)" : "var(--chip-bg)",
                      border: r.me ? "1px solid var(--accent)" : "1px solid var(--chip-border)",
                    }}
                  >
                    {r.emoji} {r.count}
                  </button>
                ))}
              </div>
            )}
            {replaceWithTranslation && translation?.loading && (
              <span
                className="flex items-center gap-1 text-[11px] opacity-80"
                style={{ color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-3 w-3 animate-spin" />
                {t("translatingDots")}
              </span>
            )}
            {isReplaced && (
              <button
                type="button"
                onClick={() => setShowOriginal(true)}
                title={t("viewOrigTextTitle")}
                className="flex items-center gap-1 text-[11px] opacity-75 hover:opacity-100 hover:underline"
                style={{ color: "var(--text-secondary)" }}
              >
                <Languages className="h-3 w-3" />
                <span>{t("translatedOrigLabel")}</span>
              </button>
            )}
            {timestampEl}
          </div>
        )}

        {msg.send_status === "failed" && (
          <div className="mt-1.5 flex items-center justify-end gap-2 text-xs">
            <span style={{ color: "var(--danger-text)" }}>
              {msg.send_error || t("sendErrorFallback")}
            </span>
            {onRetrySend && (
              <button
                type="button"
                onClick={() => onRetrySend(msg)}
                className="font-semibold underline hover:opacity-80"
                style={{ color: "var(--accent)" }}
              >
                {t("retryShort")}
              </button>
            )}
            {onDiscardFailed && (
              <button
                type="button"
                onClick={() => onDiscardFailed(msg.msg_id)}
                className="opacity-75 underline hover:opacity-100"
              >
                {t("delete")}
              </button>
            )}
          </div>
        )}

        {replyCount > 0 && onOpenThread && (
          <button
            type="button"
            onClick={() => onOpenThread(msg)}
            className="mt-1.5 -mx-1 flex w-[calc(100%+0.5rem)] items-center justify-between gap-3 rounded-xl border-t px-2 py-1.5 text-left text-[13px] font-medium transition hover:bg-white/5"
            style={{
              borderColor: msg.is_outgoing ? "rgba(255,255,255,0.16)" : "var(--border-subtle)",
              color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
            }}
          >
            <span className="flex items-center gap-1.5">
              <MessagesSquare className="h-3.5 w-3.5 shrink-0" />
              <span>{t("replyCountLabel", { count: replyCount })}</span>
            </span>
            <span className="text-xs opacity-85">{t("viewThreadLink")}</span>
          </button>
        )}

        {/* Fixed viewport-clamped Context menu */}
        {menuPos &&
          typeof document !== "undefined" &&
          createPortal(
            <div
              onClick={(e) => e.stopPropagation()}
              className="fixed z-[80] w-52 rounded-xl py-1 shadow-2xl"
              style={{
                left: `${menuPos.x}px`,
                top: `${menuPos.y}px`,
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
              }}
            >
            {!msg.is_recalled && !msg.send_status && (
              <button
                onClick={() => {
                  onReply(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <CornerUpLeft className="h-3.5 w-3.5" /> {t("replyTitle")}
              </button>
            )}
            {!msg.is_recalled && !msg.send_status && onOpenThread && (
              <button
                onClick={() => {
                  onOpenThread(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <MessagesSquare className="h-3.5 w-3.5" />{" "}
                {replyCount > 0 ? t("viewThreadReplies", { count: replyCount }) : t("openThreadReplies")}
              </button>
            )}
            {!msg.is_recalled &&
              !msg.send_status &&
              onTranslate &&
              msg.text.trim() &&
              !(isSticker && msg.text === "[Nhãn dán]") && (
                <button
                  onClick={() => {
                    if (replaceWithTranslation && translation?.text) {
                      setShowOriginal((v) => !v);
                    } else if (translation?.text) {
                      onHideTranslation?.(msg.msg_id);
                    } else {
                      onTranslate(msg);
                    }
                    setMenuPos(null);
                  }}
                  className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                >
                  <Languages className="h-3.5 w-3.5" />{" "}
                  {replaceWithTranslation && translation?.text
                    ? showOriginal
                      ? t("showTranslationOnly")
                      : t("viewOriginalTextMenu")
                    : translation?.text
                      ? t("hideTranslation")
                      : t("translateMessage")}
                </button>
              )}
            {!msg.is_recalled && !msg.send_status && onTogglePin && (
              <button
                onClick={() => {
                  onTogglePin(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Pin className="h-3.5 w-3.5" />{" "}
                {msg.is_pinned ? t("unpinMessage") : t("pinMessage")}
              </button>
            )}
            {!msg.is_recalled && !msg.send_status && onForward && (
              <button
                onClick={() => {
                  onForward(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Forward className="h-3.5 w-3.5" /> {t("forwardMessage")}
              </button>
            )}
            {!msg.send_status && onStartSelect && (
              <button
                onClick={() => {
                  onStartSelect(msg.msg_id);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <CheckSquare className="h-3.5 w-3.5" /> {t("selectMultiple")}
              </button>
            )}
            {isImage && msg.attachment_url && (
              <button
                onClick={handleCopyImage}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <ImageIcon className="h-3.5 w-3.5" /> {t("copyImage")}
              </button>
            )}
            <button
              onClick={handleCopy}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
            >
              <Copy className="h-3.5 w-3.5" /> {t("copyText")}
            </button>
            {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
              <button
                onClick={() => {
                  onRecall(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-amber-500"
              >
                <RotateCcw className="h-3.5 w-3.5" /> {t("recallMessage")}
              </button>
            )}
            <button
              onClick={() => {
                if (msg.send_status && onDiscardFailed) {
                  onDiscardFailed(msg.msg_id);
                } else {
                  onDeleteLocal(msg);
                }
                setMenuPos(null);
              }}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-rose-500"
            >
              <Trash2 className="h-3.5 w-3.5" /> {t("deleteOnDevice")}
            </button>
          </div>,
            document.body,
          )}
      </div>
    </div>
  );
});
