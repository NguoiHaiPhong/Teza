import { useState } from "react";
import {
  Copy,
  CornerUpLeft,
  Download,
  FileText,
  Forward,
  Languages,
  Loader2,
  MessagesSquare,
  RotateCcw,
  Smile,
  Trash2,
  Zap,
} from "lucide-react";
import type { MessageDto, ThreadKind } from "../types";
import { isBuzzMessage } from "../utils/buzz";

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
  translation?: MessageTranslationState;
  replaceWithTranslation?: boolean;
  replyCount?: number;
  hideQuote?: boolean;
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

function renderLinkedText(text: string, isOutgoing: boolean) {
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
      part
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
  translation,
  replaceWithTranslation = false,
  replyCount = 0,
  hideQuote = false,
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
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);
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

  const handleCopy = () => {
    void navigator.clipboard?.writeText(msg.text);
    setMenuOpen(false);
  };

  return (
    <div
      id={`msg-${msg.msg_id}`}
      className={`group relative flex ${msg.is_outgoing ? "justify-end" : "justify-start"} ${
        groupedWithPrev ? "mt-0.5" : "mt-2.5"
      }`}
      onMouseLeave={() => {
        setPickerOpen(false);
        setMenuOpen(false);
      }}
    >
      {/* Quick hover actions (left side for outgoing, right side for incoming) */}
      {!msg.is_recalled && (
        <div
          className={`flex items-center gap-1 self-center opacity-0 transition group-hover:opacity-100 ${
            msg.is_outgoing ? "order-1 mr-2" : "order-2 ml-2"
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
          setMenuOpen((v) => !v);
        }}
        className={`selectable-text wrap-anywhere relative min-w-0 max-w-[72%] rounded-2xl px-3.5 py-2 shadow-sm transition ${
          msg.is_outgoing ? "order-2 rounded-br-md" : "order-1 rounded-bl-md"
        } ${highlighted ? "ring-2 ring-sky-400" : ""}`}
        style={{
          background: msg.is_outgoing ? "var(--bg-bubble-out)" : "var(--bg-bubble-in)",
          border: msg.is_outgoing
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
          <img
            src={msg.attachment_url}
            alt="Nhãn dán"
            referrerPolicy="no-referrer"
            onError={() => setMediaError(true)}
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
            <img
              src={msg.attachment_url}
              alt={msg.attachment_name || "Hình ảnh"}
              referrerPolicy="no-referrer"
              onError={() => setMediaError(true)}
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
            {safeDownloadUrl && (
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
            )}
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
              className={`whitespace-pre-wrap wrap-anywhere text-[15.5px] leading-[1.55] ${
                msg.is_recalled ? "italic opacity-65" : ""
              } ${replaceWithTranslation && translation?.loading ? "opacity-75" : ""}`}
            >
              {renderLinkedText(
                isReplaced && translation?.text ? translation.text : msg.text,
                msg.is_outgoing,
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
          <span className="text-xs" style={{ color: "var(--text-secondary)" }}>
            {formatClock(msg.ts)}
          </span>
        </div>

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

        {/* Context menu */}
        {menuOpen && (
          <div
            className={`absolute top-full z-30 mt-1 w-48 rounded-xl py-1 shadow-2xl ${
              msg.is_outgoing ? "right-0" : "left-0"
            }`}
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            {!msg.is_recalled && (
              <button
                onClick={() => {
                  onReply(msg);
                  setMenuOpen(false);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <CornerUpLeft className="h-3.5 w-3.5" /> Trả lời
              </button>
            )}
            {!msg.is_recalled && onOpenThread && (
              <button
                onClick={() => {
                  onOpenThread(msg);
                  setMenuOpen(false);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <MessagesSquare className="h-3.5 w-3.5" />{" "}
                {replyCount > 0 ? `Xem luồng (${replyCount})` : "Mở luồng trả lời"}
              </button>
            )}
            {!msg.is_recalled &&
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
                    setMenuOpen(false);
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
            {!msg.is_recalled && onForward && (
              <button
                onClick={() => {
                  onForward(msg);
                  setMenuOpen(false);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Forward className="h-3.5 w-3.5" /> Chuyển tiếp
              </button>
            )}
            <button
              onClick={handleCopy}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
            >
              <Copy className="h-3.5 w-3.5" /> Sao chép
            </button>
            {msg.is_outgoing && !msg.is_recalled && (
              <button
                onClick={() => {
                  onRecall(msg);
                  setMenuOpen(false);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-amber-500"
              >
                <RotateCcw className="h-3.5 w-3.5" /> Thu hồi
              </button>
            )}
            <button
              onClick={() => {
                onDeleteLocal(msg);
                setMenuOpen(false);
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
