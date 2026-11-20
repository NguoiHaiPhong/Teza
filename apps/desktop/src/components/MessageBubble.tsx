import { useEffect, useState } from "react";
import {
  Check,
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
  RotateCcw,
  Smile,
  Trash2,
  Zap,
} from "lucide-react";
import type { MessageDto, ThreadKind } from "../types";
import { isBuzzMessage } from "../utils/buzz";
import { copyImageReliable, copyTextReliable } from "../utils/clipboard";
import { RelayImage } from "./RelayImage";
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
  highlighted: boolean;
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
}

function formatClock(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
}

function isStickerMessage(msg: MessageDto): boolean {
  return !!msg.attachment_url && (msg.msg_type.includes("sticker") || msg.msg_type === "5");
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
  const parts = text.split(/(https:\/\/[^\s<>"]+)/gi);
  return parts.map((part, idx) =>
    /^https:\/\//i.test(part) ? (
      <a
        key={idx}
        href={part}
        target="_blank"
        rel="noreferrer"
        className="underline underline-offset-2 hover:opacity-80"
        style={{ color: isOutgoing ? "var(--link-out)" : "var(--link-in)" }}
      >
        {part}
      </a>
    ) : (
      <span key={idx}>{renderMentionsAndSearch(part, isOutgoing, highlightQuery)}</span>
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

export function MessageBubble({
  msg,
  threadKind,
  groupedWithPrev,
  highlighted,
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
}: Props) {
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [mediaError, setMediaError] = useState(false);
  const [showOriginal, setShowOriginal] = useState(false);
  const isReplaced = Boolean(replaceWithTranslation && translation?.text && !showOriginal);
  const isBuzz = isBuzzMessage(msg);
  const isSticker = !msg.is_recalled && isStickerMessage(msg) && !mediaError;
  const isVoice = !msg.is_recalled && isVoiceAttachment(msg) && !mediaError;
  const isVideo = !msg.is_recalled && isVideoAttachment(msg) && !mediaError;
  const isImage = !msg.is_recalled && !isVoice && !isVideo && isImageAttachment(msg) && !mediaError;
  const isFileAttachment =
    !msg.is_recalled &&
    !isSticker &&
    !isVoice &&
    !isVideo &&
    !isImage &&
    (!!msg.attachment_url ||
      msg.msg_type.includes("file") ||
      msg.msg_type === "19");
  const safeDownloadUrl =
    msg.attachment_url && /^https:\/\//i.test(msg.attachment_url)
      ? msg.attachment_url
      : null;

  useEffect(() => {
    if (!menuPos && !pickerOpen) return;
    const close = () => {
      setMenuPos(null);
      setPickerOpen(false);
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
  }, [menuPos, pickerOpen]);

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

  return (
    <div
      id={`msg-${msg.msg_id}`}
      onClick={selectMode && !msg.send_status ? () => onToggleSelect?.(msg.msg_id) : undefined}
      className={`group relative flex ${msg.is_outgoing ? "justify-end" : "justify-start"} ${
        groupedWithPrev ? "mt-0.5" : "mt-2.5"
      } ${selectMode ? "cursor-pointer rounded-xl px-1.5 py-0.5 transition" : ""}`}
      style={selectMode && selected ? { background: "var(--accent-soft)" } : undefined}
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
        <div className="order-1 mr-2 mt-0.5 flex w-7 shrink-0 items-start justify-center self-start">
          {!groupedWithPrev && (
            <button
              type="button"
              disabled={!onOpenProfile || !msg.sender_id}
              onClick={() => msg.sender_id && onOpenProfile?.(msg.sender_id)}
              title="Xem thông tin tài khoản"
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
            title="Trả lời"
            className="hover-surface rounded-full p-1.5 transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            <CornerUpLeft className="h-3.5 w-3.5" />
          </button>
          {onOpenThread && (
            <button
              onClick={() => onOpenThread(msg)}
              title={replyCount > 0 ? `Xem luồng trả lời (${replyCount})` : "Mở luồng trả lời"}
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
              title={translation?.text ? "Ẩn bản dịch" : "Dịch tin nhắn"}
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
              title="Chuyển tiếp"
              className="hover-surface rounded-full p-1.5 transition"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Forward className="h-3.5 w-3.5" />
            </button>
          )}
          <div className="relative">
            <button
              onClick={() => setPickerOpen((v) => !v)}
              title="Thả cảm xúc"
              className="rounded-full p-1.5 hover:opacity-80"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Smile className="h-3.5 w-3.5" />
            </button>
            {pickerOpen && (
              <div
                className={`absolute bottom-full z-20 mb-1 flex items-center gap-1 rounded-full px-2 py-1 shadow-xl ${
                  msg.is_outgoing ? "right-0" : "left-0"
                }`}
                style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
              >
                {QUICK_EMOJIS.map((emoji) => {
                  const active = msg.reactions.some((r) => r.emoji === emoji && r.me);
                  return (
                    <button
                      key={emoji}
                      onClick={() => {
                        onReact(msg, emoji);
                        setPickerOpen(false);
                      }}
                      title={active ? "Bấm để bỏ cảm xúc" : `Thả ${emoji}`}
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
              </div>
            )}
          </div>
        </div>
      )}

      <div
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setPickerOpen(false);
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
        className={`selectable-text wrap-anywhere relative order-2 min-w-0 max-w-[72%] rounded-2xl px-3.5 shadow-sm transition ${
          msg.is_outgoing ? "rounded-br-md" : "rounded-bl-md"
        } ${highlighted ? "ring-2 ring-sky-400" : ""} ${
          msg.send_status === "sending" ? "opacity-80" : ""
        }`}
        style={{
          paddingBlock: "var(--bubble-py)",
          background: msg.is_outgoing ? "var(--bg-bubble-out)" : "var(--bg-bubble-in)",
          border:
            msg.send_status === "failed"
              ? "1px solid var(--danger-text)"
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
            className="mb-1 block text-left text-[13px] font-semibold hover:underline"
            style={{ color: "var(--accent)" }}
            title="Xem thông tin tài khoản"
          >
            {msg.sender_name}
          </button>
        )}

        {!hideQuote && msg.quote && !msg.is_recalled && (
          <button
            type="button"
            onClick={() => msg.quote?.msg_id && onJumpToMsg(msg.quote.msg_id)}
            className="mb-1.5 block w-full rounded-lg border-l-2 px-2.5 py-1.5 text-left text-[13px] transition hover:opacity-85"
            style={{
              borderColor: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
              background: "var(--quote-bg)",
            }}
          >
            <div
              className="font-semibold"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            >
              {msg.quote.sender_name || "Tin nhắn gốc"}
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
            className="mb-1 h-28 w-28 object-contain"
            loading="lazy"
          />
        )}

        {isVoice && msg.attachment_url && (
          <audio
            controls
            preload="none"
            src={msg.attachment_url}
            onError={() => setMediaError(true)}
            className="mb-1.5 max-w-full"
          />
        )}

        {isVideo && msg.attachment_url && (
          <video
            controls
            preload="metadata"
            src={msg.attachment_url}
            onError={() => setMediaError(true)}
            className="mb-1.5 max-h-72 w-full rounded-xl bg-black/40"
          />
        )}

        {isImage && msg.attachment_url && (
          <button
            type="button"
            onClick={() => onOpenImage(msg.attachment_url!, msg.attachment_name || "Hình ảnh")}
            className="mb-1.5 block overflow-hidden rounded-xl"
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

        {isFileAttachment && (
          <div
            className="mb-1.5 flex items-center gap-2.5 rounded-xl px-3 py-2"
            style={{ background: "var(--card-bg)" }}
          >
            <FileText
              className="h-5 w-5 shrink-0"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13.5px] font-medium">
                {msg.attachment_name || "Tập tin đính kèm"}
              </div>
              <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                Tệp đính kèm
              </div>
            </div>
            {safeDownloadUrl &&
              (onSaveFile ? (
                <button
                  type="button"
                  onClick={() => onSaveFile(safeDownloadUrl, msg.attachment_name || "download")}
                  title="Lưu tệp về máy..."
                  className="hover-menu rounded-lg p-1.5 transition"
                  style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                >
                  <Download className="h-4 w-4" />
                </button>
              ) : (
                <a
                  href={safeDownloadUrl}
                  target="_blank"
                  rel="noreferrer"
                  download={msg.attachment_name || undefined}
                  title="Tải xuống tệp"
                  className="hover-menu rounded-lg p-1.5 transition"
                  style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                >
                  <Download className="h-4 w-4" />
                </a>
              ))}
          </div>
        )}

        {isBuzz ? (
          <div className="my-0.5 inline-flex items-center gap-2 rounded-xl bg-amber-500/20 px-3 py-1.5 font-extrabold tracking-wider text-amber-400 ring-1 ring-amber-400/40">
            <Zap className="h-4 w-4 animate-pulse fill-amber-400 text-amber-300" />
            <span className="text-[16px]">BUZZ!</span>
          </div>
        ) : (
          !(isSticker && msg.text === "[Nhãn dán]") && (
            <div
              className={`msg-body-text whitespace-pre-wrap wrap-anywhere leading-[1.55] ${
                msg.is_recalled ? "italic opacity-65" : ""
              } ${replaceWithTranslation && translation?.loading ? "opacity-75" : ""}`}
            >
              {renderLinkedText(
                isReplaced && translation?.text ? translation.text : msg.text,
                msg.is_outgoing,
                highlightQuery,
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
                  ? "Đang dịch..."
                  : translation.error
                    ? "Lỗi dịch"
                    : `Dịch${translation.src ? ` · ${translation.src}` : ""}${
                        translation.targetLang ? ` → ${translation.targetLang}` : ""
                      }`}
              </span>
              {replaceWithTranslation && translation.text ? (
                <button
                  type="button"
                  onClick={() => setShowOriginal(false)}
                  className="hover:underline"
                >
                  Xem bản dịch
                </button>
              ) : (
                onHideTranslation &&
                !translation.loading && (
                  <button
                    type="button"
                    onClick={() => onHideTranslation(msg.msg_id)}
                    className="hover:underline"
                  >
                    Ẩn
                  </button>
                )
              )}
            </div>
            {translation.loading ? (
              <div className="flex items-center gap-1.5 text-[13.5px] opacity-80">
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                <span>Đang xử lý bản dịch...</span>
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

        <div className="mt-1 flex items-center justify-end gap-2">
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
              Đang dịch...
            </span>
          )}
          {isReplaced && (
            <button
              type="button"
              onClick={() => setShowOriginal(true)}
              title="Nhấn để xem văn bản gốc"
              className="flex items-center gap-1 text-[11px] opacity-75 hover:opacity-100 hover:underline"
              style={{ color: "var(--text-secondary)" }}
            >
              <Languages className="h-3 w-3" />
              <span>Đã dịch · Gốc</span>
            </button>
          )}
          <span className="flex items-center gap-1 text-xs" style={{ color: "var(--text-secondary)" }}>
            {msg.is_pinned && (
              <span title="Tin nhắn đã ghim" className="inline-flex items-center">
                <Pin
                  className="h-3 w-3 rotate-45"
                  style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                />
              </span>
            )}
            <span>{formatClock(msg.ts)}</span>
            {msg.is_outgoing && !msg.is_recalled && msg.send_status === "sending" && (
              <span title="Đang gửi...">
                <Loader2 className="h-3 w-3 animate-spin" />
              </span>
            )}
            {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
              <span title="Đã gửi">
                <Check className="h-3 w-3 opacity-80" />
              </span>
            )}
          </span>
        </div>

        {msg.send_status === "failed" && (
          <div className="mt-1.5 flex items-center justify-end gap-2 text-xs">
            <span style={{ color: "var(--danger-text)" }}>
              {msg.send_error || "Gửi thất bại"}
            </span>
            {onRetrySend && (
              <button
                type="button"
                onClick={() => onRetrySend(msg)}
                className="font-semibold underline hover:opacity-80"
                style={{ color: "var(--accent)" }}
              >
                Thử lại
              </button>
            )}
            {onDiscardFailed && (
              <button
                type="button"
                onClick={() => onDiscardFailed(msg.msg_id)}
                className="opacity-75 underline hover:opacity-100"
              >
                Xóa
              </button>
            )}
          </div>
        )}

        {replyCount > 0 && onOpenThread && (
          <button
            type="button"
            onClick={() => onOpenThread(msg)}
            className="mt-2 -mx-1 flex w-[calc(100%+0.5rem)] items-center justify-between gap-3 rounded-xl border-t px-2 py-1.5 text-left text-[13px] font-medium transition hover:bg-white/5"
            style={{
              borderColor: msg.is_outgoing ? "rgba(255,255,255,0.16)" : "var(--border-subtle)",
              color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
            }}
          >
            <span className="flex items-center gap-1.5">
              <MessagesSquare className="h-3.5 w-3.5 shrink-0" />
              <span>{replyCount} phản hồi</span>
            </span>
            <span className="text-xs opacity-85">Xem luồng ›</span>
          </button>
        )}

        {/* Fixed viewport-clamped Context menu */}
        {menuPos && (
          <div
            onClick={(e) => e.stopPropagation()}
            className="fixed z-50 w-52 rounded-xl py-1 shadow-2xl"
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
                <CornerUpLeft className="h-3.5 w-3.5" /> Trả lời
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
                {replyCount > 0 ? `Xem luồng (${replyCount})` : "Mở luồng trả lời"}
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
                      ? "Chỉ hiện bản dịch"
                      : "Xem văn bản gốc"
                    : translation?.text
                      ? "Ẩn bản dịch"
                      : "Dịch tin nhắn"}
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
                {msg.is_pinned ? "Bỏ ghim tin nhắn" : "Ghim tin nhắn"}
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
                <Forward className="h-3.5 w-3.5" /> Chuyển tiếp
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
                <CheckSquare className="h-3.5 w-3.5" /> Chọn nhiều tin
              </button>
            )}
            {isImage && msg.attachment_url && (
              <button
                onClick={handleCopyImage}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <ImageIcon className="h-3.5 w-3.5" /> Sao chép hình ảnh
              </button>
            )}
            <button
              onClick={handleCopy}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
            >
              <Copy className="h-3.5 w-3.5" /> Sao chép
            </button>
            {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
              <button
                onClick={() => {
                  onRecall(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-amber-500"
              >
                <RotateCcw className="h-3.5 w-3.5" /> Thu hồi
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
              <Trash2 className="h-3.5 w-3.5" /> Xóa trên máy
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
