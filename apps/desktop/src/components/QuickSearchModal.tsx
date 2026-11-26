import { useCallback, useEffect, useMemo, useState } from "react";
import { Search, UserPlus, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { MessageDto, ThreadDto } from "../types";
import { looksLikePhoneQuery } from "../utils/threadState";

interface Props {
  open: boolean;
  threads: ThreadDto[];
  onClose: () => void;
  onSelectMessage: (msg: MessageDto) => void;
  onFindPhone?: (phone: string) => void;
}

function formatResultTime(ts: number, locale: string): string {
  if (!ts) return "";
  const ms = ts > 1e12 ? ts : ts * 1000;
  return new Date(ms).toLocaleString(locale, {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function QuickSearchModal({ open, threads, onClose, onSelectMessage, onFindPhone }: Props) {
  const { locale, t } = useI18n();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<MessageDto[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);

  const threadTitleMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const t of threads) {
      map.set(t.id, t.title);
    }
    return map;
  }, [threads]);

  const resetState = useCallback(() => {
    setQuery("");
    setResults([]);
    setActiveIndex(0);
  }, []);

  const handleClose = useCallback(() => {
    resetState();
    onClose();
  }, [onClose, resetState]);

  const handleSelectHit = useCallback(
    (hit: MessageDto) => {
      resetState();
      onSelectMessage(hit);
      onClose();
    },
    [onClose, onSelectMessage, resetState],
  );

  useEffect(() => {
    if (!open) {
      resetState();
      return;
    }
    let cancelled = false;
    const trimmed = query.trim();
    if (!trimmed) {
      setResults([]);
      setActiveIndex(0);
      return;
    }
    const timer = window.setTimeout(() => {
      void bridge
        .searchGlobal(trimmed)
        .then((hits) => {
          if (cancelled) return;
          setResults(hits);
          setActiveIndex(0);
        })
        .catch(() => {
          if (cancelled) return;
          setResults([]);
        });
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [open, query, resetState]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") handleClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, handleClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/55 pt-20 backdrop-blur-xs"
      onClick={handleClose}
    >
      <div
        className="w-full max-w-xl overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center gap-3 border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                handleClose();
              } else if (e.key === "ArrowDown" && results.length > 0) {
                e.preventDefault();
                setActiveIndex((i) => (i + 1) % results.length);
              } else if (e.key === "ArrowUp" && results.length > 0) {
                e.preventDefault();
                setActiveIndex((i) => (i - 1 + results.length) % results.length);
              } else if (e.key === "Enter" && results[activeIndex]) {
                e.preventDefault();
                handleSelectHit(results[activeIndex]);
              }
            }}
            placeholder={t("quickSearchPlaceholder")}
            className="w-full bg-transparent text-[15px] outline-none"
          />
          <button
            type="button"
            onClick={handleClose}
            className="hover-menu rounded p-1"
            title={t("close")}
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="max-h-96 overflow-y-auto p-2">
          {onFindPhone && looksLikePhoneQuery(query) && (
            <button
              type="button"
              onClick={() => {
                const phone = query.trim();
                resetState();
                onClose();
                onFindPhone(phone);
              }}
              className="hover-menu mb-1.5 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left text-[13.5px] font-medium transition"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
              }}
            >
              <UserPlus className="h-4 w-4 shrink-0" />
              <span>{t("findFriendByPhonePrompt", { query: query.trim() })}</span>
            </button>
          )}
          {results.length === 0 ? (
            <p className="py-8 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {query.trim()
                ? t("quickSearchNoResults")
                : t("quickSearchHint")}
            </p>
          ) : (
            results.map((m, idx) => {
              const threadTitle = threadTitleMap.get(m.thread_id) || m.thread_id;
              const selected = idx === activeIndex;
              return (
                <button
                  key={`${m.thread_id}-${m.msg_id}`}
                  type="button"
                  onMouseEnter={() => setActiveIndex(idx)}
                  onClick={() => handleSelectHit(m)}
                  className="hover-menu flex w-full flex-col gap-0.5 rounded-xl px-3 py-2 text-left transition"
                  style={{
                    background: selected ? "var(--bg-hover)" : "transparent",
                  }}
                >
                  <div className="flex items-center justify-between gap-2 text-[13px]">
                    <span className="truncate font-semibold" style={{ color: "var(--accent)" }}>
                      {threadTitle}
                      <span className="font-normal" style={{ color: "var(--text-secondary)" }}>
                        {" · "}
                        {m.sender_name}
                      </span>
                    </span>
                    <span className="shrink-0 text-xs" style={{ color: "var(--text-secondary)" }}>
                      {formatResultTime(m.ts, locale)}
                    </span>
                  </div>
                  <span className="truncate text-[15px]">{m.text}</span>
                </button>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
