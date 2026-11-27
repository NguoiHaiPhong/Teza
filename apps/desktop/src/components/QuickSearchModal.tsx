import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2, Search, UserPlus, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { MessageDto, ThreadDto } from "../types";
import { isImeComposingEvent, matchesSearchQuery } from "../utils/searchNormalize";
import { looksLikePhoneQuery } from "../utils/threadState";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  open: boolean;
  isLocked?: boolean;
  threads: ThreadDto[];
  onClose: () => void;
  onSelectMessage: (msg: MessageDto) => void;
  onSelectThread?: (threadId: string) => void;
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

export function QuickSearchModal({
  open,
  isLocked = false,
  threads,
  onClose,
  onSelectMessage,
  onSelectThread,
  onFindPhone,
}: Props) {
  const { locale, t } = useI18n();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<MessageDto[]>([]);
  const [resultsQuery, setResultsQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const trimmedQuery = query.trim();
  const resultsAreFresh = Boolean(trimmedQuery) && resultsQuery === trimmedQuery;
  const freshResults = resultsAreFresh ? results : [];

  const threadTitleMap = useMemo(() => {
    const map = new Map<string, string>();
    for (const th of threads) {
      map.set(th.id, th.title);
    }
    return map;
  }, [threads]);

  const showPhoneLookup = Boolean(onFindPhone && looksLikePhoneQuery(trimmedQuery));

  const matchedThreads = useMemo(() => {
    if (!trimmedQuery || !onSelectThread) return [];
    return threads
      .filter((th) => matchesSearchQuery(`${th.title}\n${th.last_preview}\n${th.id}`, trimmedQuery))
      .slice(0, 6);
  }, [threads, trimmedQuery, onSelectThread]);

  const phoneOffset = showPhoneLookup ? 1 : 0;
  const msgOffset = phoneOffset + matchedThreads.length;
  const totalItems = msgOffset + freshResults.length;
  const safeActiveIndex = totalItems > 0 ? Math.min(activeIndex, totalItems - 1) : 0;

  const resetState = useCallback(() => {
    setQuery("");
    setResults([]);
    setResultsQuery("");
    setSearching(false);
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

  const handleSelectMatchedThread = useCallback(
    (threadId: string) => {
      if (!onSelectThread) return;
      resetState();
      onSelectThread(threadId);
      onClose();
    },
    [onClose, onSelectThread, resetState],
  );

  const handleSelectPhoneLookup = useCallback(() => {
    if (!onFindPhone || !trimmedQuery) return;
    const phone = trimmedQuery;
    resetState();
    onClose();
    onFindPhone(phone);
  }, [onFindPhone, trimmedQuery, resetState, onClose]);

  useEffect(() => {
    if (!open) {
      resetState();
      return;
    }
    let cancelled = false;
    setActiveIndex(0);
    if (!trimmedQuery) {
      setResults([]);
      setResultsQuery("");
      setSearching(false);
      return;
    }
    setSearching(true);
    const timer = window.setTimeout(() => {
      void bridge
        .searchGlobal(trimmedQuery)
        .then((hits) => {
          if (cancelled) return;
          setResults(hits);
          setResultsQuery(trimmedQuery);
          setSearching(false);
          setActiveIndex(0);
        })
        .catch(() => {
          if (cancelled) return;
          setResults([]);
          setResultsQuery(trimmedQuery);
          setSearching(false);
        });
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [open, trimmedQuery, resetState]);

  useEffect(() => {
    if (!open || totalItems === 0) return;
    const el = itemRefs.current[safeActiveIndex];
    el?.scrollIntoView({ block: "nearest" });
  }, [open, safeActiveIndex, totalItems]);

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked || isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        handleClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, handleClose]);

  if (!open) return null;

  itemRefs.current.length = totalItems;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/55 pt-20 backdrop-blur-xs"
      onClick={handleClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("globalSearchTitle")}
        className="w-full max-w-xl overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center gap-3 border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          {searching ? (
            <Loader2 className="h-4 w-4 shrink-0 animate-spin" style={{ color: "var(--accent)" }} />
          ) : (
            <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
          )}
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (isLocked || isImeComposingEvent(e)) return;
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                handleClose();
              } else if (e.key === "ArrowDown" && totalItems > 0) {
                e.preventDefault();
                setActiveIndex((i) => (i + 1) % totalItems);
              } else if (e.key === "ArrowUp" && totalItems > 0) {
                e.preventDefault();
                setActiveIndex((i) => (i - 1 + totalItems) % totalItems);
              } else if (e.key === "Enter" && totalItems > 0) {
                e.preventDefault();
                if (showPhoneLookup && safeActiveIndex === 0) {
                  handleSelectPhoneLookup();
                } else if (safeActiveIndex < msgOffset) {
                  const chosenThread = matchedThreads[safeActiveIndex - phoneOffset];
                  if (chosenThread) handleSelectMatchedThread(chosenThread.id);
                } else if (resultsAreFresh) {
                  const chosenMsg = freshResults[safeActiveIndex - msgOffset];
                  if (chosenMsg) handleSelectHit(chosenMsg);
                }
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
          {showPhoneLookup && (
            <button
              ref={(el) => {
                itemRefs.current[0] = el;
              }}
              type="button"
              onMouseEnter={() => setActiveIndex(0)}
              onClick={handleSelectPhoneLookup}
              className="hover-menu mb-1.5 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left text-[13.5px] font-medium transition"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
                outline: safeActiveIndex === 0 ? "1px solid var(--accent)" : "none",
              }}
            >
              <UserPlus className="h-4 w-4 shrink-0" />
              <span>{t("findFriendByPhonePrompt", { query: trimmedQuery })}</span>
            </button>
          )}
          {matchedThreads.length > 0 && (
            <div className="mb-1.5">
              <div
                className="px-3 py-1 text-[11px] font-semibold uppercase tracking-wider"
                style={{ color: "var(--text-secondary)" }}
              >
                {t("quickSearchThreadsSection")}
              </div>
              {matchedThreads.map((th, idx) => {
                const combinedIdx = phoneOffset + idx;
                const selected = combinedIdx === safeActiveIndex;
                return (
                  <button
                    key={th.id}
                    ref={(el) => {
                      itemRefs.current[combinedIdx] = el;
                    }}
                    type="button"
                    onMouseEnter={() => setActiveIndex(combinedIdx)}
                    onClick={() => handleSelectMatchedThread(th.id)}
                    className="hover-menu flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left transition"
                    style={{
                      background: selected ? "var(--bg-hover)" : "transparent",
                    }}
                  >
                    <ThreadAvatar avatar={th.avatar} title={th.title} kind={th.kind} size="sm" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-[14px] font-semibold">{th.title}</div>
                      {th.last_preview && (
                        <div
                          className="truncate text-[12px]"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {th.last_preview}
                        </div>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>
          )}
          {!trimmedQuery ? (
            <p className="py-8 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {t("quickSearchHint")}
            </p>
          ) : searching || !resultsAreFresh ? (
            matchedThreads.length === 0 && !showPhoneLookup ? (
              <div
                className="flex items-center justify-center gap-2 py-8 text-[13px]"
                style={{ color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
                <span>{t("loading")}</span>
              </div>
            ) : null
          ) : matchedThreads.length === 0 && freshResults.length === 0 && !showPhoneLookup ? (
            <p className="py-8 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {t("quickSearchNoResults")}
            </p>
          ) : freshResults.length > 0 ? (
            <div>
              {matchedThreads.length > 0 && (
                <div
                  className="px-3 py-1 text-[11px] font-semibold uppercase tracking-wider"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("quickSearchMessagesSection")}
                </div>
              )}
              {freshResults.map((m, idx) => {
                const combinedIdx = msgOffset + idx;
                const threadTitle = threadTitleMap.get(m.thread_id) || m.thread_id;
                const selected = combinedIdx === safeActiveIndex;
                return (
                  <button
                    key={`${m.thread_id}-${m.msg_id}`}
                    ref={(el) => {
                      itemRefs.current[combinedIdx] = el;
                    }}
                    type="button"
                    onMouseEnter={() => setActiveIndex(combinedIdx)}
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
              })}
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
