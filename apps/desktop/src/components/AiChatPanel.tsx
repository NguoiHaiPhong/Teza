import { useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  Bot,
  Check,
  Copy,
  CornerDownLeft,
  Lock,
  Send,
  SlidersHorizontal,
  Sparkles,
  Square,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import { copyTextReliable } from "../utils/clipboard";
import {
  formatLocalDateTime,
  parseInlineMarkdown,
  parseSimpleMarkdownBlocks,
  type AiConfigDto,
  type AiStoredMessageDto,
} from "../utils/automationHelpers";

export interface AiChatPanelProps {
  config: AiConfigDto | null;
  messages: AiStoredMessageDto[];
  streamingText: string;
  isStreaming: boolean;
  error: string | null;
  onSend: (prompt: string) => void;
  onCancelStream?: () => void;
  onClearHistory: () => void;
  onOpenSettings: () => void;
  onClose?: () => void;
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
  messages,
  streamingText,
  isStreaming,
  error,
  onSend,
  onCancelStream,
  onClearHistory,
  onOpenSettings,
  onClose,
}: AiChatPanelProps) {
  const [draft, setDraft] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [copiedCodeKey, setCopiedCodeKey] = useState<string | null>(null);

  const listRef = useRef<HTMLDivElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const copyTimerRef = useRef<number | null>(null);
  const codeCopyTimerRef = useRef<number | null>(null);

  const enabled = Boolean(config?.enabled);
  const isCli = config?.backend === "gemini_cli";
  const backendLabel = isCli ? "Gemini CLI" : "Gemini API";
  const activeModel = isCli
    ? config?.cli_model?.trim() || "Mặc định CLI"
    : config?.gemini_model?.trim() || "gemini-2.5-flash";

  useEffect(() => {
    return () => {
      if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
      if (codeCopyTimerRef.current !== null) window.clearTimeout(codeCopyTimerRef.current);
    };
  }, []);

  // Auto-scroll to bottom on new messages or streaming chunks
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const distanceToBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceToBottom < 220 || isStreaming) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages.length, streamingText, isStreaming]);

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

  const handleSubmit = () => {
    if (!enabled || isStreaming) return;
    const clean = draft.trim();
    if (!clean) return;
    setDraft("");
    setConfirmClear(false);
    onSend(clean);
  };

  const handleComposerKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      handleSubmit();
    }
  };

  const handleChipClick = (chip: StarterChip) => {
    if (!enabled || isStreaming) return;
    if (chip.fillOnly) {
      setDraft(chip.prompt);
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
      className="flex h-full min-h-0 flex-1 flex-col overflow-hidden"
      style={{ background: "var(--bg-stage)", color: "var(--text-primary)" }}
    >
      {/* Top Bar */}
      <header
        className="flex shrink-0 items-center justify-between gap-3 px-4 py-3"
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
                    {isCli ? <Terminal className="h-3 w-3" /> : <Bot className="h-3 w-3" />}
                    <span>
                      {backendLabel} · {activeModel}
                    </span>
                  </>
                ) : (
                  <span>Chưa bật</span>
                )}
              </span>
            </div>
            <p className="truncate text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
              Trợ lý tùy chọn · Lịch sử mã hóa cục bộ trên thiết bị
            </p>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
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
            Xác nhận xóa toàn bộ lịch sử trò chuyện với Trợ lý AI trên máy này?
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
      {!enabled ? (
        <div className="flex flex-1 items-center justify-center overflow-y-auto p-6">
          <div
            className="w-full max-w-md space-y-4 rounded-2xl p-6 text-center shadow-lg"
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
              <h3 className="text-[16px] font-semibold">Bật Trợ lý AI tùy chọn</h3>
              <p className="text-[13px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
                Trợ lý AI trong Teza được <strong>tắt mặc định</strong> để ưu tiên quyền riêng tư.
                Khi bật, bạn có thể trò chuyện trực tiếp bằng Gemini API (dùng chung API key ở mục
                Dịch) hoặc Gemini CLI cài trên máy, đồng thời tóm tắt nhanh tin nhắn chưa đọc.
              </p>
            </div>

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
                Dữ liệu chỉ được gửi khi bạn chủ động đặt câu hỏi hoặc bấm tóm tắt. Lịch sử hội
                thoại AI được mã hóa AES-256-GCM trong cơ sở dữ liệu cục bộ.
              </span>
            </div>

            <button
              type="button"
              onClick={onOpenSettings}
              className="tf-btn inline-flex items-center justify-center gap-2 rounded-xl px-5 py-2.5 text-[13.5px] font-semibold"
            >
              <SlidersHorizontal className="h-4 w-4" />
              <span>Mở Cài đặt AI</span>
            </button>
          </div>
        </div>
      ) : (
        <>
          {/* Conversation Scroll Area */}
          <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
            <div className="mx-auto flex w-full max-w-3xl flex-col gap-3">
              {messages.length === 0 && !isStreaming && (
                <div className="my-8 flex flex-col items-center text-center">
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
                      className={`group max-w-[85%] rounded-2xl px-3.5 py-2.5 text-[14px] shadow-xs ${
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
                    className="max-w-[85%] rounded-2xl rounded-bl-md px-3.5 py-2.5 text-[14px] shadow-xs"
                    style={{
                      background: "var(--bg-bubble-in)",
                      border: "1px solid var(--border-subtle)",
                      color: "var(--text-primary)",
                    }}
                  >
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
                          onClick={onCancelStream}
                          className="hover-menu inline-flex items-center gap-1 rounded-lg px-2 py-0.5 text-[11px] font-medium"
                          style={{
                            border: "1px solid var(--border-subtle)",
                            color: "var(--text-secondary)",
                          }}
                        >
                          <Square className="h-2.5 w-2.5 fill-current" />
                          <span>Dừng tạo</span>
                        </button>
                      </div>
                    )}
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
            </div>
          </div>

          {/* Composer */}
          <div
            className="shrink-0 px-4 py-3"
            style={{
              background: "var(--bg-sidebar)",
              borderTop: "1px solid var(--border-subtle)",
            }}
          >
            <div className="mx-auto flex w-full max-w-3xl flex-col gap-1.5">
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
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={handleComposerKeyDown}
                  placeholder="Nhập câu hỏi cho Trợ lý AI Teza... (Enter để gửi, Shift+Enter xuống dòng)"
                  className="max-h-40 min-h-[36px] flex-1 resize-none bg-transparent py-1 text-[14px] leading-relaxed outline-none"
                  style={{ color: "var(--text-primary)" }}
                />

                {isStreaming && onCancelStream ? (
                  <button
                    type="button"
                    onClick={onCancelStream}
                    className="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-xl px-3 text-xs font-semibold transition hover:opacity-90"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                    }}
                    title="Dừng phản hồi AI"
                  >
                    <Square className="h-3.5 w-3.5 fill-current" />
                    <span>Dừng</span>
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={isStreaming || !draft.trim()}
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
                  Enter gửi · Shift+Enter xuống dòng
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
