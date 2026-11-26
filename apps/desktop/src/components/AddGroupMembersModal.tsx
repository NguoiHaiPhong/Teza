import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Loader2, Search, UserPlus, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { AddGroupMembersResultDto, ThreadDto } from "../types";
import { isImeComposingEvent, matchesSearchQuery } from "../utils/searchNormalize";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  open: boolean;
  threadId: string;
  groupTitle: string;
  threads: ThreadDto[];
  existingMemberIds: string[];
  onClose: () => void;
  onAdded: (result: AddGroupMembersResultDto) => void;
}

export function AddGroupMembersModal({
  open,
  threadId,
  groupTitle,
  threads,
  existingMemberIds,
  onClose,
  onAdded,
}: Props) {
  const { lang, t } = useI18n();
  const [query, setQuery] = useState("");
  const [phoneQuery, setPhoneQuery] = useState("");
  const [pickedIds, setPickedIds] = useState<string[]>([]);
  const [extraUsers, setExtraUsers] = useState<
    { id: string; title: string; avatar: string }[]
  >([]);
  const [searchingPhone, setSearchingPhone] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<AddGroupMembersResultDto | null>(null);
  const closeTimerRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current !== null) {
        window.clearTimeout(closeTimerRef.current);
        closeTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (closeTimerRef.current !== null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
    if (!open) return;
    setQuery("");
    setPhoneQuery("");
    setPickedIds([]);
    setExtraUsers([]);
    setError(null);
    setResult(null);
  }, [open, threadId]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e)) return;
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  const existingSet = useMemo(() => {
    const s = new Set<string>();
    for (const id of existingMemberIds) {
      const clean = id.split("_")[0]?.trim();
      if (clean) s.add(clean);
    }
    return s;
  }, [existingMemberIds]);

  const candidates = useMemo(() => {
    const merged = new Map<string, { id: string; title: string; avatar: string }>();
    for (const ex of extraUsers) {
      if (!existingSet.has(ex.id)) merged.set(ex.id, ex);
    }
    for (const t of threads) {
      if (t.kind !== "direct") continue;
      if (existingSet.has(t.id)) continue;
      if (!merged.has(t.id)) {
        merged.set(t.id, { id: t.id, title: t.title, avatar: t.avatar });
      }
    }
    const q = query.trim();
    const list = Array.from(merged.values());
    return (q ? list.filter((u) => matchesSearchQuery(`${u.title}\n${u.id}`, q)) : list).slice(
      0,
      120,
    );
  }, [threads, extraUsers, existingSet, query]);

  if (!open) return null;

  const togglePick = (uid: string) => {
    setPickedIds((prev) =>
      prev.includes(uid) ? prev.filter((x) => x !== uid) : [...prev, uid],
    );
  };

  const handleFindPhone = async () => {
    const trimmed = phoneQuery.trim();
    if (!trimmed || searchingPhone) return;
    setSearchingPhone(true);
    setError(null);
    try {
      const countryCode = trimmed.startsWith("+") ? undefined : lang === "ko" ? "82" : "84";
      const profile = await bridge.findUserByPhone(trimmed, countryCode);
      if (existingSet.has(profile.uid)) {
        setError(`${profile.display_name} đã ở trong nhóm này.`);
      } else {
        setExtraUsers((prev) => {
          if (prev.some((x) => x.id === profile.uid)) return prev;
          return [
            {
              id: profile.uid,
              title: profile.display_name || profile.account_name || profile.uid,
              avatar: profile.avatar,
            },
            ...prev,
          ];
        });
        setPickedIds((prev) =>
          prev.includes(profile.uid) ? prev : [...prev, profile.uid],
        );
        setPhoneQuery("");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setSearchingPhone(false);
    }
  };

  const handleSubmit = async () => {
    if (pickedIds.length === 0 || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      const res = await bridge.addGroupMembers(threadId, pickedIds);
      setResult(res);
      onAdded(res);
      if (res.failed.length === 0) {
        if (closeTimerRef.current !== null) {
          window.clearTimeout(closeTimerRef.current);
        }
        closeTimerRef.current = window.setTimeout(() => {
          closeTimerRef.current = null;
          onClose();
        }, 900);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="flex max-h-[85vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2 font-semibold">
            <UserPlus className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
            <span className="truncate text-[15.5px]">
              {t("addMembersModalTitle", { title: groupTitle })}
            </span>
          </div>
          <button type="button" onClick={onClose} className="hover-menu rounded-lg p-1.5">
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="space-y-3 p-4">
          <div
            className="flex items-center gap-2 rounded-xl px-3 py-2"
            style={{ background: "var(--bg-surface)" }}
          >
            <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("filterFriendsPlaceholder")}
              className="w-full bg-transparent text-sm outline-none"
            />
          </div>

          <div className="flex items-center gap-2">
            <input
              value={phoneQuery}
              onChange={(e) => setPhoneQuery(e.target.value)}
              onKeyDown={(e) => {
                if (isImeComposingEvent(e)) return;
                if (e.key === "Enter") {
                  e.preventDefault();
                  void handleFindPhone();
                }
              }}
              placeholder={t("searchPhoneShortPlaceholder")}
              className="min-w-0 flex-1 rounded-xl px-3 py-1.5 text-xs outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            />
            <button
              type="button"
              disabled={!phoneQuery.trim() || searchingPhone}
              onClick={() => void handleFindPhone()}
              className="hover-menu shrink-0 rounded-xl px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
              style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
            >
              {searchingPhone ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : t("findPhoneBtn")}
            </button>
          </div>

          {error && (
            <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300">
              {error}
            </div>
          )}

          {result && (
            <div className="rounded-xl bg-emerald-500/15 px-3 py-2 text-xs text-emerald-300">
              {t("addMembersResultSummary", {
                added: result.added.length,
                pending: result.pending.length,
                failed: result.failed.length,
              })}
            </div>
          )}

          <div
            className="max-h-64 space-y-1 overflow-y-auto rounded-xl p-2"
            style={{ background: "var(--bg-stage)", border: "1px solid var(--border-subtle)" }}
          >
            {candidates.length === 0 ? (
              <div className="py-8 text-center text-xs" style={{ color: "var(--text-secondary)" }}>
                {t("noCandidatesLeft")}
              </div>
            ) : (
              candidates.map((u) => {
                const picked = pickedIds.includes(u.id);
                return (
                  <button
                    key={u.id}
                    type="button"
                    onClick={() => togglePick(u.id)}
                    className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left transition"
                    style={{ background: picked ? "var(--accent-soft)" : "transparent" }}
                  >
                    <span
                      className="flex h-4 w-4 shrink-0 items-center justify-center rounded border"
                      style={{
                        borderColor: picked ? "var(--accent)" : "var(--border-subtle)",
                        background: picked ? "var(--accent)" : "transparent",
                        color: "var(--text-on-accent)",
                      }}
                    >
                      {picked && <Check className="h-3 w-3" />}
                    </span>
                    <ThreadAvatar avatar={u.avatar} title={u.title} kind="direct" size="sm" />
                    <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">
                      {u.title}
                    </span>
                  </button>
                );
              })
            )}
          </div>
        </div>

        <div
          className="flex items-center justify-end gap-2 px-4 py-3"
          style={{ borderTop: "1px solid var(--border-subtle)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-xl px-4 py-2 text-xs font-medium"
            style={{ background: "var(--bg-surface)" }}
          >
            {t("close")}
          </button>
          <button
            type="button"
            disabled={pickedIds.length === 0 || submitting}
            onClick={() => void handleSubmit()}
            className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
          >
            {submitting && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
            <span>{t("addToGroupCountBtn", { count: pickedIds.length })}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
