import { useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  Loader2,
  MessageSquare,
  RefreshCw,
  Undo2,
  UserCheck,
  UserPlus,
  X,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { FriendRequestItemDto } from "../types";
import { isImeComposingEvent } from "../utils/searchNormalize";
import { formatThreadTime } from "../utils/threadState";
import { RelayImage } from "./RelayImage";

export interface FriendRequestsModalProps {
  open: boolean;
  isLocked?: boolean;
  onClose: () => void;
  onOpenProfile: (uid: string) => void;
  onStartChat: (uid: string, name: string, avatar: string) => void;
  onChanged: (receivedCount: number) => void;
}

type RequestsTab = "received" | "sent";
type BusyAction = { uid: string; kind: "accept" | "reject" | "undo" } | null;

const AVATAR_PALETTE = ["#3b82f6", "#8b5cf6", "#10b981", "#f59e0b", "#ec4899", "#06b6d4"];

function pickAvatarColor(seed: string): string {
  let hash = 0;
  for (let i = 0; i < seed.length; i += 1) {
    hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  }
  return AVATAR_PALETTE[Math.abs(hash) % AVATAR_PALETTE.length];
}

function cleanError(err: unknown): string {
  return String(err).replace(/^Error:\s*/i, "").trim() || String(err);
}

function RequestAvatar({ avatar, name }: { avatar: string; name: string }) {
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
  }, [avatar]);

  if (!avatar || failed) {
    return (
      <div
        className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white"
        style={{ background: pickAvatarColor(name || "?") }}
      >
        {(name || "?").slice(0, 1).toUpperCase()}
      </div>
    );
  }

  return (
    <RelayImage
      src={avatar}
      alt={name}
      loading="lazy"
      decoding="async"
      referrerPolicy="no-referrer"
      onFallbackFail={() => setFailed(true)}
      onError={() => setFailed(true)}
      className="h-11 w-11 shrink-0 rounded-full object-cover"
    />
  );
}

export function FriendRequestsModal({
  open,
  isLocked = false,
  onClose,
  onOpenProfile,
  onStartChat,
  onChanged,
}: FriendRequestsModalProps) {
  const { lang, t } = useI18n();
  const [tab, setTab] = useState<RequestsTab>("received");
  const [received, setReceived] = useState<FriendRequestItemDto[]>([]);
  const [sent, setSent] = useState<FriendRequestItemDto[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyAction, setBusyAction] = useState<BusyAction>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [lastAccepted, setLastAccepted] = useState<{
    uid: string;
    name: string;
    avatar: string;
  } | null>(null);

  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;
  const fetchSeqRef = useRef(0);
  const receivedRef = useRef<FriendRequestItemDto[]>([]);
  const mountedRef = useRef(true);
  const openRef = useRef(open);
  openRef.current = open;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const loadRequests = useCallback(async (notifyCount = true) => {
    const seq = ++fetchSeqRef.current;
    setLoading(true);
    setError(null);
    try {
      const res = await bridge.friendListRequests();
      if (!mountedRef.current || seq !== fetchSeqRef.current) return;
      const nextReceived = Array.isArray(res.received) ? res.received : [];
      const nextSent = Array.isArray(res.sent) ? res.sent : [];
      receivedRef.current = nextReceived;
      setReceived(nextReceived);
      setSent(nextSent);
      if (notifyCount) {
        onChangedRef.current(nextReceived.length);
      }
    } catch (err) {
      if (!mountedRef.current || seq !== fetchSeqRef.current) return;
      setError(cleanError(err));
    } finally {
      if (mountedRef.current && seq === fetchSeqRef.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    if (!open) {
      fetchSeqRef.current += 1;
      setBusyAction(null);
      setError(null);
      setNotice(null);
      setLastAccepted(null);
      return;
    }
    setNotice(null);
    setLastAccepted(null);
    void loadRequests(true);
  }, [open, loadRequests]);

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked || isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, onClose]);

  if (!open) return null;

  const getDisplayName = (item: FriendRequestItemDto) =>
    item.display_name?.trim() || item.account_name?.trim() || item.user_id;

  const handleAccept = async (item: FriendRequestItemDto) => {
    if (busyAction) return;
    const displayName = getDisplayName(item);
    setBusyAction({ uid: item.user_id, kind: "accept" });
    setError(null);
    setNotice(null);
    setLastAccepted(null);
    try {
      const res = await bridge.friendAcceptRequest(item.user_id);
      if (!mountedRef.current || !openRef.current) return;
      if (!res.ok) {
        setError(res.message || t("friendRequestAcceptBtn"));
        return;
      }
      const nextReceived = receivedRef.current.filter((r) => r.user_id !== item.user_id);
      receivedRef.current = nextReceived;
      setReceived((prev) => prev.filter((r) => r.user_id !== item.user_id));
      setSent((prev) => prev.filter((r) => r.user_id !== item.user_id));
      onChangedRef.current(nextReceived.length);
      setNotice(t("friendAcceptedSuccess", { name: displayName }));
      setLastAccepted({ uid: item.user_id, name: displayName, avatar: item.avatar || "" });
    } catch (err) {
      if (!mountedRef.current || !openRef.current) return;
      setError(cleanError(err));
    } finally {
      if (mountedRef.current) {
        setBusyAction(null);
      }
    }
  };

  const handleReject = async (item: FriendRequestItemDto) => {
    if (busyAction) return;
    setBusyAction({ uid: item.user_id, kind: "reject" });
    setError(null);
    setNotice(null);
    setLastAccepted(null);
    try {
      const res = await bridge.friendRejectRequest(item.user_id);
      if (!mountedRef.current || !openRef.current) return;
      if (!res.ok) {
        setError(res.message || t("friendRequestRejectBtn"));
        return;
      }
      const nextReceived = receivedRef.current.filter((r) => r.user_id !== item.user_id);
      receivedRef.current = nextReceived;
      setReceived((prev) => prev.filter((r) => r.user_id !== item.user_id));
      onChangedRef.current(nextReceived.length);
      setNotice(t("friendRejectedSuccess"));
    } catch (err) {
      if (!mountedRef.current || !openRef.current) return;
      setError(cleanError(err));
    } finally {
      if (mountedRef.current) {
        setBusyAction(null);
      }
    }
  };

  const handleUndo = async (item: FriendRequestItemDto) => {
    if (busyAction) return;
    setBusyAction({ uid: item.user_id, kind: "undo" });
    setError(null);
    setNotice(null);
    setLastAccepted(null);
    try {
      const res = await bridge.friendUndoRequest(item.user_id);
      if (!mountedRef.current || !openRef.current) return;
      if (!res.ok) {
        setError(res.message || t("friendRequestUndoBtn"));
        return;
      }
      setSent((prev) => prev.filter((r) => r.user_id !== item.user_id));
      onChangedRef.current(receivedRef.current.length);
      setNotice(t("friendUndoneSuccess"));
    } catch (err) {
      if (!mountedRef.current || !openRef.current) return;
      setError(cleanError(err));
    } finally {
      if (mountedRef.current) {
        setBusyAction(null);
      }
    }
  };

  const activeItems = tab === "received" ? received : sent;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2 font-semibold">
            <UserCheck className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
            <span className="truncate text-[15.5px]">{t("friendRequestsTitle")}</span>
          </div>
          <div className="flex items-center gap-1">
            <button
              type="button"
              disabled={loading}
              onClick={() => void loadRequests(true)}
              title={t("friendRequestsRefresh")}
              className="hover-menu flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition disabled:opacity-50"
              style={{ color: "var(--text-secondary)" }}
            >
              <RefreshCw className={`h-3.5 w-3.5 ${loading ? "animate-spin" : ""}`} />
              <span>{t("friendRequestsRefresh")}</span>
            </button>
            <button
              type="button"
              onClick={onClose}
              title={t("close")}
              className="hover-menu rounded-lg p-1.5"
            >
              <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
            </button>
          </div>
        </div>

        {/* Tabs */}
        <div
          className="grid grid-cols-2 gap-2 px-4 pt-3 pb-2"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <button
            type="button"
            onClick={() => setTab("received")}
            className="flex items-center justify-center gap-2 rounded-xl px-3 py-2 text-[13.5px] font-semibold transition"
            style={{
              background: tab === "received" ? "var(--accent-soft)" : "var(--bg-surface)",
              color: tab === "received" ? "var(--accent)" : "var(--text-secondary)",
              border:
                tab === "received"
                  ? "1px solid var(--accent)"
                  : "1px solid var(--border-subtle)",
            }}
          >
            <span>{t("friendRequestsReceivedTab")}</span>
            <span
              className="rounded-full px-1.5 py-0.5 text-[11px] font-bold leading-none"
              style={{
                background: tab === "received" ? "var(--accent)" : "var(--bg-hover)",
                color: tab === "received" ? "var(--text-on-accent)" : "var(--text-secondary)",
              }}
            >
              {received.length}
            </span>
          </button>

          <button
            type="button"
            onClick={() => setTab("sent")}
            className="flex items-center justify-center gap-2 rounded-xl px-3 py-2 text-[13.5px] font-semibold transition"
            style={{
              background: tab === "sent" ? "var(--accent-soft)" : "var(--bg-surface)",
              color: tab === "sent" ? "var(--accent)" : "var(--text-secondary)",
              border:
                tab === "sent"
                  ? "1px solid var(--accent)"
                  : "1px solid var(--border-subtle)",
            }}
          >
            <span>{t("friendRequestsSentTab")}</span>
            <span
              className="rounded-full px-1.5 py-0.5 text-[11px] font-bold leading-none"
              style={{
                background: tab === "sent" ? "var(--accent)" : "var(--bg-hover)",
                color: tab === "sent" ? "var(--text-on-accent)" : "var(--text-secondary)",
              }}
            >
              {sent.length}
            </span>
          </button>
        </div>

        {/* Status / Error Banners */}
        {(error || notice) && (
          <div className="space-y-2 px-4 pt-3">
            {error && (
              <div
                role="alert"
                className="flex items-center justify-between gap-2 rounded-xl px-3 py-2 text-xs"
                style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
              >
                <span className="wrap-anywhere">{error}</span>
                <button
                  type="button"
                  onClick={() => setError(null)}
                  className="shrink-0 rounded p-0.5 opacity-75 hover:opacity-100"
                  title={t("close")}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            )}
            {notice && (
              <div className="flex items-center justify-between gap-2 rounded-xl bg-emerald-500/15 px-3 py-2 text-xs text-emerald-400">
                <span className="wrap-anywhere">{notice}</span>
                <div className="flex shrink-0 items-center gap-1.5">
                  {lastAccepted && (
                    <button
                      type="button"
                      onClick={() => {
                        onStartChat(lastAccepted.uid, lastAccepted.name, lastAccepted.avatar);
                        onClose();
                      }}
                      className="tf-btn flex items-center gap-1 rounded-lg px-2 py-1 text-[11.5px] font-semibold"
                    >
                      <MessageSquare className="h-3 w-3" />
                      <span>{t("startChatBtn")}</span>
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => {
                      setNotice(null);
                      setLastAccepted(null);
                    }}
                    className="rounded p-0.5 opacity-75 hover:opacity-100"
                    title={t("close")}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            )}
          </div>
        )}

        {/* List Body */}
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {loading && activeItems.length === 0 ? (
            <div
              className="flex items-center justify-center gap-2 py-12 text-[13px]"
              style={{ color: "var(--text-secondary)" }}
            >
              <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
              <span>{t("friendRequestsRefresh")}...</span>
            </div>
          ) : activeItems.length === 0 ? (
            <div
              className="flex flex-col items-center justify-center gap-2 py-12 text-center text-[13px]"
              style={{ color: "var(--text-secondary)" }}
            >
              <UserPlus className="h-8 w-8 opacity-40" />
              <span>
                {tab === "received"
                  ? t("friendRequestsEmptyReceived")
                  : t("friendRequestsEmptySent")}
              </span>
            </div>
          ) : (
            <div className="space-y-2.5">
              {activeItems.map((item) => {
                const displayName = getDisplayName(item);
                const isAccepting =
                  busyAction?.uid === item.user_id && busyAction.kind === "accept";
                const isRejecting =
                  busyAction?.uid === item.user_id && busyAction.kind === "reject";
                const isUndoing =
                  busyAction?.uid === item.user_id && busyAction.kind === "undo";
                const anyBusy = Boolean(busyAction);
                const timeText = item.created_ts > 0 ? formatThreadTime(item.created_ts, lang) : "";

                return (
                  <div
                    key={`${tab}-${item.user_id}`}
                    className="rounded-xl p-3 transition"
                    style={{
                      background: "var(--bg-surface)",
                      border: "1px solid var(--border-subtle)",
                    }}
                  >
                    <div className="flex items-start gap-3">
                      <button
                        type="button"
                        onClick={() => {
                          onOpenProfile(item.user_id);
                          onClose();
                        }}
                        title={t("viewProfileBtn")}
                        className="shrink-0 rounded-full transition hover:opacity-90"
                      >
                        <RequestAvatar avatar={item.avatar} name={displayName} />
                      </button>

                      <div className="min-w-0 flex-1">
                        <div className="flex items-center justify-between gap-2">
                          <button
                            type="button"
                            onClick={() => {
                              onOpenProfile(item.user_id);
                              onClose();
                            }}
                            title={t("viewProfileBtn")}
                            className="min-w-0 truncate text-left text-[14.5px] font-semibold hover:underline"
                          >
                            {displayName}
                          </button>
                          {timeText && (
                            <span
                              className="shrink-0 text-[11px]"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {timeText}
                            </span>
                          )}
                        </div>

                        {item.message?.trim() && (
                          <p
                            className="selectable-text wrap-anywhere mt-1 rounded-lg px-2.5 py-1.5 text-[12.5px]"
                            style={{
                              background: "var(--bg-sidebar)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            {item.message.trim()}
                          </p>
                        )}

                        <div className="mt-2.5 flex flex-wrap items-center gap-2">
                          {tab === "received" ? (
                            <>
                              <button
                                type="button"
                                disabled={anyBusy}
                                onClick={() => void handleAccept(item)}
                                className="tf-btn flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
                              >
                                {isAccepting ? (
                                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <Check className="h-3.5 w-3.5" />
                                )}
                                <span>{t("friendRequestAcceptBtn")}</span>
                              </button>
                              <button
                                type="button"
                                disabled={anyBusy}
                                onClick={() => void handleReject(item)}
                                className="hover-menu flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-medium transition disabled:opacity-50"
                                style={{
                                  background: "var(--bg-sidebar)",
                                  border: "1px solid var(--border-subtle)",
                                  color: "var(--danger-text)",
                                }}
                              >
                                {isRejecting ? (
                                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <X className="h-3.5 w-3.5" />
                                )}
                                <span>{t("friendRequestRejectBtn")}</span>
                              </button>
                            </>
                          ) : (
                            <button
                              type="button"
                              disabled={anyBusy}
                              onClick={() => void handleUndo(item)}
                              className="hover-menu flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-medium transition disabled:opacity-50"
                              style={{
                                background: "var(--bg-sidebar)",
                                border: "1px solid var(--border-subtle)",
                                color: "var(--text-primary)",
                              }}
                            >
                              {isUndoing ? (
                                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                              ) : (
                                <Undo2 className="h-3.5 w-3.5" />
                              )}
                              <span>{t("friendRequestUndoBtn")}</span>
                            </button>
                          )}

                          <button
                            type="button"
                            onClick={() => {
                              onStartChat(item.user_id, displayName, item.avatar || "");
                              onClose();
                            }}
                            title={t("startChatBtn")}
                            className="hover-menu ml-auto flex items-center gap-1 rounded-xl px-2.5 py-1.5 text-xs font-medium transition"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            <MessageSquare className="h-3.5 w-3.5" />
                            <span>{t("startChatBtn")}</span>
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Footer */}
        <div
          className="flex items-center justify-end px-4 py-3"
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
        </div>
      </div>
    </div>
  );
}

export default FriendRequestsModal;
