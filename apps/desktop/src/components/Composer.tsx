import { useEffect, useRef, useState } from "react";
import { CornerUpLeft, FileText, Image as ImageIcon, Paperclip, Send, Smile, X, Zap } from "lucide-react";
import { bridge, isTauri, MAX_ATTACHMENT_BYTES } from "../bridge";
import type { MessageDto, PendingAttachment } from "../types";

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
  pendingAttachment?: PendingAttachment | null;
  onPendingAttachmentChange?: (att: PendingAttachment | null) => void;
  onCancelReply: () => void;
  onSendText: (text: string) => Promise<void>;
  onSendAttachment?: (att: PendingAttachment, caption: string) => Promise<unknown>;
  onBuzz?: () => Promise<void>;
  buzzCooldownSec?: number;
  placeholder?: string;
  onError: (err: unknown) => void;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function Composer({
  replyTo,
  pendingAttachment = null,
  onPendingAttachmentChange,
  onCancelReply,
  onSendText,
  onSendAttachment,
  onBuzz,
  buzzCooldownSec = 0,
  placeholder,
  onError,
}: Props) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [emojiOpen, setEmojiOpen] = useState(false);
  const [emojiTab, setEmojiTab] = useState(0);
  const isComposingRef = useRef(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

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
    const fileName = file.name || `clipboard-${Date.now()}.png`;
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
      await onSendText(text);
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
        {emojiOpen && (
          <div
            className="absolute bottom-full left-4 z-30 mb-2 w-72 rounded-2xl p-3 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div className="mb-2 flex items-center justify-between gap-1">
              <div className="flex gap-1">
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
              <button
                type="button"
                onClick={() => setEmojiOpen(false)}
                className="hover-menu rounded p-1"
                style={{ color: "var(--text-secondary)" }}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
            <div className="grid grid-cols-8 gap-1">
              {EMOJI_GROUPS[emojiTab].items.map((emoji) => (
                <button
                  key={emoji}
                  type="button"
                  onClick={() => {
                    setDraft((prev) => `${prev}${emoji}`);
                    textareaRef.current?.focus();
                  }}
                  className="hover-menu flex h-8 w-8 items-center justify-center rounded-lg text-base transition hover:scale-110"
                >
                  {emoji}
                </button>
              ))}
            </div>
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
          title="Chèn biểu cảm"
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
          onChange={(e) => setDraft(e.target.value)}
          onPaste={(e) => {
            const items = e.clipboardData?.items;
            if (!items) return;
            for (const item of items) {
              if (item.kind === "file") {
                const file = item.getAsFile();
                if (file) {
                  e.preventDefault();
                  attachRawFile(file);
                  return;
                }
              }
            }
          }}
          onCompositionStart={() => {
            isComposingRef.current = true;
          }}
          onCompositionEnd={() => {
            isComposingRef.current = false;
          }}
          onKeyDown={(e) => {
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
