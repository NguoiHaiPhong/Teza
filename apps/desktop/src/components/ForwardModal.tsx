import { useEffect, useMemo, useState } from "react";
import { Forward, Search, X } from "lucide-react";
import type { MessageDto, ThreadDto } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  message: MessageDto | null;
  threads: ThreadDto[];
  onClose: () => void;
  onForwardTo: (targetThreadId: string, msg: MessageDto) => Promise<void>;
}

export function ForwardModal({ message, threads, onClose, onForwardTo }: Props) {
  const [query, setQuery] = useState("");
  const [sendingId, setSendingId] = useState<string | null>(null);

  useEffect(() => {
    if (!message) {
      setQuery("");
      setSendingId(null);
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [message, onClose]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return threads
      .filter((t) => !q || t.title.toLowerCase().includes(q))
      .slice(0, 40);
  }, [threads, query]);

  if (!message) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2 text-[15px] font-semibold">
            <Forward className="h-4 w-4" style={{ color: "var(--accent)" }} />
            <span>Chuyển tiếp tin nhắn</span>
          </div>
          <button type="button" onClick={onClose} className="hover-menu rounded-lg p-1">
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="px-4 pt-3">
          <div
            className="mb-3 rounded-xl px-3 py-2 text-[13px]"
            style={{ background: "var(--quote-bg)", color: "var(--text-secondary)" }}
          >
            <span className="font-semibold" style={{ color: "var(--text-primary)" }}>
              Nội dung:{" "}
            </span>
            <span className="line-clamp-2">{message.text || message.attachment_name}</span>
          </div>

          <div
            className="flex items-center gap-2 rounded-xl px-3 py-2"
            style={{ background: "var(--bg-surface)" }}
          >
            <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Tìm cuộc hội thoại đích..."
              className="w-full bg-transparent text-[15px] outline-none"
            />
          </div>
        </div>

        <div className="mt-2 flex-1 overflow-y-auto px-2 pb-3">
          {filtered.length === 0 ? (
            <div className="py-8 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              Không tìm thấy hội thoại phù hợp
            </div>
          ) : (
            filtered.map((t) => (
              <button
                key={t.id}
                type="button"
                disabled={sendingId === t.id}
                onClick={() => {
                  setSendingId(t.id);
                  void onForwardTo(t.id, message).finally(() => {
                    setSendingId(null);
                    onClose();
                  });
                }}
                className="hover-surface flex w-full items-center justify-between gap-3 rounded-xl px-3 py-2 text-left transition disabled:opacity-60"
              >
                <div className="flex min-w-0 items-center gap-2.5">
                  <ThreadAvatar avatar={t.avatar} title={t.title} kind={t.kind} size="sm" />
                  <div className="min-w-0">
                    <div className="truncate text-[15px] font-medium">{t.title}</div>
                    <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {t.kind === "group" ? "Nhóm" : t.kind === "cloud" ? "Cloud" : "1-1"}
                    </div>
                  </div>
                </div>
                <span className="tf-btn shrink-0 rounded-lg px-2.5 py-1 text-[13px] font-semibold">
                  {sendingId === t.id ? "Đang gửi..." : "Gửi"}
                </span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
