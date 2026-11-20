import { useEffect, useMemo, useRef, useState } from "react";
import { CornerUpLeft, FileText, Image as ImageIcon, Loader2, Paperclip, Send, Smile, X, Zap } from "lucide-react";
import { bridge, isTauri, MAX_ATTACHMENT_BYTES } from "../bridge";
import type { MessageDto, PendingAttachment, StickerItem, ThreadMemberDto } from "../types";
import { RelayImage } from "./RelayImage";
import { ThreadAvatar } from "./ThreadAvatar";

const EMOJI_GROUPS: { label: string; items: string[] }[] = [
  {
    label: "Thường dùng",
    items: ["😀", "😂", "🥰", "😍", "😊", "😭", "😮", "🤔", "🙏", "👍", "❤️", "🔥", "🎉", "👏", "✨", "💯"],
  },
  {
    label: "Cảm xúc",
    items: ["😅", "🤣", "😉", "😎", "🥳", "😢", "😡", "🤯", "😴", "🤗", "🤭", "😇", "🥺", "😤", "😬", "🫠"],
  },
  {
    label: "Công việc",
    items: ["✅", "📌", "📎", "📝", "📅", "⏰", "💡", "🚀", "👀", "👌", "🤝", "💪", "📊", "📁", "⚠️", "❌"],
  },
];

interface Props {
  replyTo: MessageDto | null;
  draft?: string;
  onDraftChange?: (val: string) => void;
  pendingAttachment?: PendingAttachment | null;
  onPendingAttachmentChange?: (att: PendingAttachment | null) => void;
  mentionMembers?: ThreadMemberDto[];
  onSendSticker?: (sticker: StickerItem) => Promise<unknown>;
  onCancelReply: () => void;
  onSendText: (text: string) => Promise<boolean | void>;
  onSendAttachment?: (att: PendingAttachment, caption: string) => Promise<unknown>;
  onBuzz?: () => Promise<void>;
  buzzCooldownSec?: number;
  placeholder?: string;
  globalPasteFallback?: boolean;
  onError: (err: unknown) => void;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function extFromMime(mime: string): string {
  switch (mime.toLowerCase()) {
    case "image/jpeg":
    case "image/jpg":
      return "jpg";
    case "image/webp":
      return "webp";
    case "image/gif":
      return "gif";
    case "image/bmp":
      return "bmp";
    default:
      return "png";
  }
}

function extractAttachmentFromClipboard(cd: DataTransfer | null): File | null {
  if (!cd) return null;
  const fromItems: File[] = [];
  if (cd.items) {
    for (const item of Array.from(cd.items)) {
      if (item.kind === "file") {
        const f = item.getAsFile();
        if (f) fromItems.push(f);
      }
    }
  }
  const candidates = fromItems.length > 0 ? fromItems : Array.from(cd.files ?? []);
  if (candidates.length === 0) return null;

  const plainText = cd.getData("text/plain");
  const uriList = cd.getData("text/uri-list");
  // Word/Excel/browsers often place both text AND an auto-rendered snapshot image on the clipboard;
  // prefer pasting text unless the clipboard actually holds non-image files or no text.
  if (
    plainText.trim().length > 0 &&
    !uriList &&
    candidates.every(
      (f) =>
        f.type.startsWith("image/") &&
        (!f.name || /^image\.(png|jpe?g|bmp|webp|gif)$/i.test(f.name)),
    )
  ) {
    return null;
  }
  return candidates[0];
}

export function Composer({
  replyTo,
  draft: controlledDraft,
  onDraftChange,
  pendingAttachment = null,
  onPendingAttachmentChange,
  mentionMembers = [],
  onSendSticker,
  onCancelReply,
  onSendText,
  onSendAttachment,
  onBuzz,
  buzzCooldownSec = 0,
  placeholder,
  globalPasteFallback = false,
  onError,
}: Props) {
  const [localDraft, setLocalDraft] = useState("");
  const draft = controlledDraft !== undefined ? controlledDraft : localDraft;
  const setDraft = (next: string | ((prev: string) => string)) => {
    const val = typeof next === "function" ? next(draft) : next;
    if (onDraftChange) onDraftChange(val);
    else setLocalDraft(val);
  };
  const [sending, setSending] = useState(false);
  const [emojiOpen, setEmojiOpen] = useState(false);
  const [pickerMode, setPickerMode] = useState<"emoji" | "sticker">("emoji");
  const [emojiTab, setEmojiTab] = useState(0);
  const [stickers, setStickers] = useState<StickerItem[] | null>(null);
  const [stickersLoading, setStickersLoading] = useState(false);
  const [cursorPos, setCursorPos] = useState(0);
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionDismissedAt, setMentionDismissedAt] = useState<number | null>(null);
  const isComposingRef = useRef(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    if (!emojiOpen || pickerMode !== "sticker" || stickers !== null || !onSendSticker) return;
    let cancelled = false;
    setStickersLoading(true);
    void bridge
      .listStickers()
      .then((list) => {
        if (!cancelled) setStickers(list);
      })
      .catch((err) => {
        if (!cancelled) {
          setStickers([]);
          onError(err);
        }
      })
      .finally(() => {
        if (!cancelled) setStickersLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [emojiOpen, pickerMode, stickers, onSendSticker, onError]);

  const mentionMatch = useMemo(() => {
    if (mentionMembers.length === 0) return null;
    const bounded = Math.min(cursorPos, draft.length);
    const before = draft.slice(0, bounded);
    const m = before.match(/(?:^|\s)(@([^\s@]*))$/);
    if (!m) return null;
    const start = before.length - m[1].length;
    if (mentionDismissedAt === start) return null;
    return { start, query: m[2] };
  }, [draft, cursorPos, mentionMembers.length, mentionDismissedAt]);

  const mentionMatches = useMemo(() => {
    if (!mentionMatch) return [];
    const q = mentionMatch.query.trim().toLowerCase();
    return mentionMembers
      .filter((m) => !q || m.name.toLowerCase().includes(q))
      .slice(0, 8);
  }, [mentionMatch, mentionMembers]);

  useEffect(() => {
    if (!mentionMatch && mentionDismissedAt !== null) {
      setMentionDismissedAt(null);
    }
  }, [mentionMatch, mentionDismissedAt]);

  useEffect(() => {
    setMentionIndex(0);
  }, [mentionMatch?.query, mentionMatches.length]);

  const insertMention = (member: ThreadMemberDto) => {
    if (!mentionMatch) return;
    const end = Math.min(cursorPos, draft.length);
    const inserted = `@${member.name} `;
    const nextText = `${draft.slice(0, mentionMatch.start)}${inserted}${draft.slice(end)}`;
    const nextCursor = mentionMatch.start + inserted.length;
    setDraft(nextText);
    setCursorPos(nextCursor);
    setMentionDismissedAt(null);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(nextCursor, nextCursor);
      }
    });
  };

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 140)}px`;
  }, [draft]);

  useEffect(() => {
    const previewUrl =
      pendingAttachment?.kind === "raw" ? pendingAttachment.previewUrl : null;
    return () => {
      if (previewUrl) URL.revokeObjectURL(previewUrl);
    };
  }, [pendingAttachment]);

  const attachRawFile = (file: File) => {
    if (!onPendingAttachmentChange || !onSendAttachment) return;
    if (file.size === 0) {
      onError(new Error("Tệp đính kèm rỗng (0 byte)"));
      return;
    }
    if (file.size > MAX_ATTACHMENT_BYTES) {
      onError(
        new Error(
          `Tệp "${file.name}" quá lớn (${formatBytes(file.size)}, tối đa ${
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
          } MB)`,
        ),
      );
      return;
    }
    const fileName = file.name || `clipboard-${Date.now()}.${extFromMime(file.type)}`;
    const isImage =
      file.type.startsWith("image/") || /\.(png|jpe?g|webp|gif|bmp)$/i.test(fileName);
    const previewUrl = isImage ? URL.createObjectURL(file) : null;
    onPendingAttachmentChange({
      kind: "raw",
      file,
      fileName,
      sizeBytes: file.size,
      isImage,
      previewUrl,
    });
  };

  const attachRawFileRef = useRef(attachRawFile);
  attachRawFileRef.current = attachRawFile;

  useEffect(() => {
    if (!globalPasteFallback) return;
    const onWindowPaste = (e: ClipboardEvent) => {
      if (e.defaultPrevented) return;
      const active = document.activeElement;
      if (
        active instanceof HTMLElement &&
        (active.tagName === "INPUT" ||
          active.tagName === "TEXTAREA" ||
          active.tagName === "SELECT" ||
          active.isContentEditable)
      ) {
        return;
      }
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest("input, textarea, select, [contenteditable]")) return;

      const file = extractAttachmentFromClipboard(e.clipboardData);
      if (file) {
        e.preventDefault();
        attachRawFileRef.current(file);
      }
    };
    window.addEventListener("paste", onWindowPaste);
    return () => window.removeEventListener("paste", onWindowPaste);
  }, [globalPasteFallback]);

  const handleAttachClick = async () => {
    if (!onPendingAttachmentChange || !onSendAttachment) return;
    if (isTauri()) {
      try {
        const meta = await bridge.pickAttachmentFile();
        if (!meta) return;
        onPendingAttachmentChange({
          kind: "path",
          path: meta.path,
          fileName: meta.file_name,
          sizeBytes: meta.size_bytes,
          isImage: meta.is_image,
        });
      } catch (err) {
        onError(err);
      }
      return;
    }
    fileInputRef.current?.click();
  };

  const handleSubmit = async () => {
    if (sending) return;
    if (pendingAttachment && onSendAttachment) {
      setSending(true);
      const att = pendingAttachment;
      const cap = draft;
      onPendingAttachmentChange?.(null);
      setDraft("");
      try {
        await onSendAttachment(att, cap);
      } finally {
        setSending(false);
      }
      return;
    }

    if (!draft.trim()) return;
    setSending(true);
    const text = draft;
    setDraft("");
    try {
      const ok = await onSendText(text);
      if (ok === false) {
        setDraft((cur) => (cur ? cur : text));
      }
    } catch {
      setDraft((cur) => (cur ? cur : text));
    } finally {
      setSending(false);
    }
  };

  return (
    <footer
      className="shrink-0"
      style={{ background: "var(--bg-sidebar)", borderTop: "1px solid var(--border-subtle)" }}
    >
      {replyTo && (
        <div
          className="flex items-center justify-between gap-2 px-4 py-2 text-[13px]"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2">
            <CornerUpLeft className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <div className="min-w-0 border-l-2 pl-2" style={{ borderColor: "var(--accent)" }}>
              <div className="font-semibold" style={{ color: "var(--accent)" }}>
                Trả lời {replyTo.sender_name}
              </div>
              <div className="truncate" style={{ color: "var(--text-secondary)" }}>
                {replyTo.text}
              </div>
            </div>
          </div>
          <button onClick={onCancelReply} className="rounded-lg p-1 hover:opacity-80">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {pendingAttachment && (
        <div
          className="flex items-center justify-between gap-3 px-4 py-2"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-3">
            {pendingAttachment.kind === "raw" && pendingAttachment.previewUrl ? (
              <img
                src={pendingAttachment.previewUrl}
                alt={pendingAttachment.fileName}
                className="h-12 w-12 shrink-0 rounded-lg object-cover"
              />
            ) : (
              <div
                className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg"
                style={{ background: "var(--bg-surface)" }}
              >
                {pendingAttachment.isImage ? (
                  <ImageIcon className="h-5 w-5" style={{ color: "var(--accent)" }} />
                ) : (
                  <FileText className="h-5 w-5" style={{ color: "var(--accent)" }} />
                )}
              </div>
            )}
            <div className="min-w-0 text-xs">
              <div className="flex items-center gap-1.5 font-medium">
                {pendingAttachment.isImage ? (
                  <ImageIcon className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                ) : (
                  <FileText className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                )}
                <span className="truncate">{pendingAttachment.fileName}</span>
                <span className="shrink-0" style={{ color: "var(--text-secondary)" }}>
                  ({formatBytes(pendingAttachment.sizeBytes)})
                </span>
              </div>
              <div style={{ color: "var(--text-secondary)" }}>
                Nhập chú thích bên dưới rồi nhấn Enter để gửi
              </div>
            </div>
          </div>
          <button
            onClick={() => onPendingAttachmentChange?.(null)}
            className="rounded-lg p-1 hover:opacity-80"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      <div className="relative flex items-end gap-2 px-4 py-3">
        {mentionMatches.length > 0 && mentionMatch && (
          <div
            className="absolute bottom-full left-12 z-30 mb-2 max-h-56 w-72 overflow-y-auto rounded-2xl p-1.5 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div
              className="px-2 py-1 text-[11px] font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Nhắc tên thành viên
            </div>
            {mentionMatches.map((m, idx) => (
              <button
                key={m.id || `${m.name}-${idx}`}
                type="button"
                onMouseDown={(e) => {
                  e.preventDefault();
                  insertMention(m);
                }}
                className="flex w-full items-center gap-2.5 rounded-xl px-2.5 py-1.5 text-left transition"
                style={{
                  background: idx === mentionIndex ? "var(--bg-hover)" : "transparent",
                }}
              >
                <ThreadAvatar avatar={m.avatar} title={m.name} kind="direct" size="xs" />
                <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">
                  {m.name}
                </span>
                {m.role && (
                  <span
                    className="shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold"
                    style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                  >
                    {m.role}
                  </span>
                )}
              </button>
            ))}
          </div>
        )}

        {emojiOpen && (
          <div
            className="absolute bottom-full left-4 z-30 mb-2 w-76 rounded-2xl p-3 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div
              className="mb-2 flex items-center justify-between gap-2 pb-2"
              style={{ borderBottom: "1px solid var(--border-subtle)" }}
            >
              <div className="flex gap-1">
                <button
                  type="button"
                  onClick={() => setPickerMode("emoji")}
                  className="rounded-lg px-2.5 py-1 text-xs font-semibold transition"
                  style={{
                    background: pickerMode === "emoji" ? "var(--accent-soft)" : "transparent",
                    color: pickerMode === "emoji" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  Emoji
                </button>
                {onSendSticker && (
                  <button
                    type="button"
                    onClick={() => setPickerMode("sticker")}
                    className="rounded-lg px-2.5 py-1 text-xs font-semibold transition"
                    style={{
                      background: pickerMode === "sticker" ? "var(--accent-soft)" : "transparent",
                      color: pickerMode === "sticker" ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    Nhãn dán
                  </button>
                )}
              </div>
              <button
                type="button"
                onClick={() => setEmojiOpen(false)}
                className="hover-menu rounded p-1"
                style={{ color: "var(--text-secondary)" }}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>

            {pickerMode === "emoji" ? (
              <>
                <div className="mb-2 flex gap-1">
                  {EMOJI_GROUPS.map((g, idx) => (
                    <button
                      key={g.label}
                      type="button"
                      onClick={() => setEmojiTab(idx)}
                      className="rounded-lg px-2 py-1 text-[11px] font-medium transition"
                      style={{
                        background: emojiTab === idx ? "var(--bg-hover)" : "transparent",
                        color: emojiTab === idx ? "var(--accent)" : "var(--text-secondary)",
                      }}
                    >
                      {g.label}
                    </button>
                  ))}
                </div>
                <div className="grid grid-cols-8 gap-1">
                  {EMOJI_GROUPS[emojiTab].items.map((emoji) => (
                    <button
                      key={emoji}
                      type="button"
                      onClick={() => {
                        const end = Math.min(cursorPos, draft.length);
                        const nextText = `${draft.slice(0, end)}${emoji}${draft.slice(end)}`;
                        const nextCursor = end + emoji.length;
                        setDraft(nextText);
                        setCursorPos(nextCursor);
                        requestAnimationFrame(() => {
                          const el = textareaRef.current;
                          if (el) {
                            el.focus();
                            el.setSelectionRange(nextCursor, nextCursor);
                          }
                        });
                      }}
                      className="hover-menu flex h-8 w-8 items-center justify-center rounded-lg text-base transition hover:scale-110"
                    >
                      {emoji}
                    </button>
                  ))}
                </div>
              </>
            ) : stickersLoading ? (
              <div
                className="flex h-44 items-center justify-center gap-2 text-xs"
                style={{ color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-4 w-4 animate-spin" />
                <span>Đang tải nhãn dán...</span>
              </div>
            ) : !stickers || stickers.length === 0 ? (
              <div
                className="flex h-44 items-center justify-center text-xs"
                style={{ color: "var(--text-secondary)" }}
              >
                Chưa có nhãn dán khả dụng
              </div>
            ) : (
              <div className="grid max-h-56 grid-cols-4 gap-2 overflow-y-auto pr-1">
                {stickers.map((s) => (
                  <button
                    key={`${s.cate_id}-${s.id}`}
                    type="button"
                    onClick={() => {
                      setEmojiOpen(false);
                      void onSendSticker?.(s);
                    }}
                    className="hover-surface flex items-center justify-center rounded-xl p-1.5 transition hover:scale-105"
                  >
                    <RelayImage
                      src={s.url}
                      alt={`Sticker ${s.id}`}
                      loading="lazy"
                      decoding="async"
                      referrerPolicy="no-referrer"
                      className="h-14 w-14 object-contain"
                    />
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={async (e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            if (files.length === 1) {
              attachRawFile(files[0]);
            } else if (files.length > 1) {
              for (const f of files) {
                if (f.size === 0 || f.size > MAX_ATTACHMENT_BYTES) {
                  attachRawFile(f);
                  continue;
                }
                const isImage =
                  f.type.startsWith("image/") || /\.(png|jpe?g|webp|gif|bmp)$/i.test(f.name);
                await onSendAttachment?.(
                  {
                    kind: "raw",
                    file: f,
                    fileName: f.name,
                    sizeBytes: f.size,
                    isImage,
                    previewUrl: null,
                  },
                  "",
                );
              }
            }
          }}
        />
        {onSendAttachment && onPendingAttachmentChange && (
          <button
            type="button"
            onClick={() => void handleAttachClick()}
            title="Đính kèm tệp hoặc hình ảnh (tối đa 100 MB)"
            className="hover-surface rounded-lg p-2 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <Paperclip className="h-5 w-5" />
          </button>
        )}

        <button
          type="button"
          onClick={() => setEmojiOpen((v) => !v)}
          title="Chèn biểu cảm & nhãn dán"
          className="hover-surface rounded-lg p-2 transition"
          style={{ color: emojiOpen ? "var(--accent)" : "var(--text-secondary)" }}
        >
          <Smile className="h-5 w-5" />
        </button>

        {onBuzz && (
          <button
            type="button"
            disabled={sending || buzzCooldownSec > 0}
            onClick={() => {
              setEmojiOpen(false);
              void onBuzz();
            }}
            title={
              buzzCooldownSec > 0
                ? `Chờ ${buzzCooldownSec}s để gửi BUZZ tiếp`
                : "Gửi BUZZ! gây chú ý (giới hạn 5s/lần)"
            }
            className="hover-surface relative rounded-lg p-2 transition disabled:cursor-not-allowed disabled:opacity-45"
            style={{ color: buzzCooldownSec > 0 ? "var(--text-secondary)" : "#f59e0b" }}
          >
            <Zap className="h-5 w-5" />
            {buzzCooldownSec > 0 && (
              <span className="absolute -right-1 -top-1 rounded-full bg-amber-500/90 px-1 text-[10px] font-bold text-slate-950">
                {buzzCooldownSec}
              </span>
            )}
          </button>
        )}

        <textarea
          ref={textareaRef}
          rows={1}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            setCursorPos(e.currentTarget.selectionStart ?? e.target.value.length);
          }}
          onClick={(e) => {
            setCursorPos(e.currentTarget.selectionStart ?? e.currentTarget.value.length);
          }}
          onKeyUp={(e) => {
            setCursorPos(e.currentTarget.selectionStart ?? e.currentTarget.value.length);
          }}
          onPaste={(e) => {
            const pastedFile = extractAttachmentFromClipboard(e.clipboardData);
            if (pastedFile) {
              e.preventDefault();
              attachRawFile(pastedFile);
            }
          }}
          onCompositionStart={() => {
            isComposingRef.current = true;
          }}
          onCompositionEnd={() => {
            isComposingRef.current = false;
          }}
          onKeyDown={(e) => {
            if (mentionMatches.length > 0 && mentionMatch) {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setMentionIndex((i) => (i + 1) % mentionMatches.length);
                return;
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setMentionIndex((i) => (i - 1 + mentionMatches.length) % mentionMatches.length);
                return;
              }
              if (
                e.key === "Tab" ||
                (e.key === "Enter" &&
                  !e.shiftKey &&
                  !isComposingRef.current &&
                  !e.nativeEvent.isComposing)
              ) {
                e.preventDefault();
                insertMention(mentionMatches[mentionIndex] ?? mentionMatches[0]);
                return;
              }
              if (e.key === "Escape") {
                e.preventDefault();
                setMentionDismissedAt(mentionMatch.start);
                return;
              }
            }
            if (
              e.key === "Enter" &&
              !e.shiftKey &&
              !isComposingRef.current &&
              !e.nativeEvent.isComposing
            ) {
              e.preventDefault();
              setEmojiOpen(false);
              void handleSubmit();
            }
          }}
          placeholder={
            pendingAttachment
              ? "Thêm chú thích cho tệp đính kèm..."
              : placeholder || "Nhập tin nhắn... (Enter gửi, Shift+Enter xuống dòng)"
          }
          className="min-h-[42px] max-h-[140px] flex-1 resize-none overflow-y-auto rounded-xl px-3.5 py-2 text-[15px] leading-6 outline-none"
          style={{ background: "var(--bg-surface)", color: "var(--text-primary)" }}
        />

        <button
          type="button"
          onClick={() => {
            setEmojiOpen(false);
            void handleSubmit();
          }}
          disabled={sending || (!draft.trim() && !pendingAttachment)}
          title="Gửi tin nhắn"
          className="tf-btn rounded-xl p-2.5 disabled:cursor-not-allowed disabled:opacity-40"
        >
          <Send className="h-4 w-4" />
        </button>
      </div>
    </footer>
  );
}
