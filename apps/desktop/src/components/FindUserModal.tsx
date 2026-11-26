import { useEffect, useRef, useState } from "react";
import { Ban, Check, Loader2, MessageSquare, Phone, Search, User, UserPlus, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { UserProfileDto } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

export interface FindUserModalProps {
  open: boolean;
  initialPhone?: string;
  onClose: () => void;
  onOpenProfile: (profile: UserProfileDto) => void;
  onStartChat: (profile: UserProfileDto) => void;
  onRelationshipChanged?: () => void;
}

export function FindUserModal({
  open,
  initialPhone = "",
  onClose,
  onOpenProfile,
  onStartChat,
  onRelationshipChanged,
}: FindUserModalProps) {
  const { t } = useI18n();
  const [phone, setPhone] = useState("");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<UserProfileDto | null>(null);
  const [sendingRequest, setSendingRequest] = useState(false);
  const [requestSent, setRequestSent] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const runSearch = async (raw: string) => {
    const query = raw.trim();
    if (!query) {
      setError("Vui lòng nhập số điện thoại cần tìm");
      setResult(null);
      return;
    }
    setSearching(true);
    setError(null);
    setResult(null);
    setSendingRequest(false);
    setRequestSent(false);
    setActionError(null);
    setActionNotice(null);
    try {
      const found = await bridge.findUserByPhone(query);
      setResult(found);
    } catch (err) {
      setError(String(err).replace(/^Error:\s*/i, ""));
    } finally {
      setSearching(false);
    }
  };

  useEffect(() => {
    if (!open) {
      setPhone("");
      setError(null);
      setResult(null);
      setSearching(false);
      setSendingRequest(false);
      setRequestSent(false);
      setActionError(null);
      setActionNotice(null);
      return;
    }
    const init = initialPhone.trim();
    setPhone(init);
    setError(null);
    setResult(null);
    setSendingRequest(false);
    setRequestSent(false);
    setActionError(null);
    setActionNotice(null);
    setTimeout(() => inputRef.current?.focus(), 20);
    if (init) {
      void runSearch(init);
    }
  }, [open, initialPhone]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const handleQuickAddFriend = async () => {
    if (!result || result.is_self || result.is_friend || sendingRequest || requestSent) return;
    setSendingRequest(true);
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendSendRequest(result.uid, t("friendRequestDefaultGreeting"));
      if (!res.ok) {
        setActionError(res.message || t("friendAddBtn"));
        return;
      }
      if (res.already_friends) {
        setResult((prev) => (prev ? { ...prev, is_friend: true } : prev));
        setActionNotice(t("friendAlreadyFriends"));
      } else if (res.auto_accepted) {
        setResult((prev) => (prev ? { ...prev, is_friend: true } : prev));
        setActionNotice(t("friendRequestAutoAccepted"));
      } else {
        setRequestSent(true);
        setActionNotice(t("friendRequestSentSuccess"));
      }
      onRelationshipChanged?.();
    } catch (err) {
      setActionError(String(err).replace(/^Error:\s*/i, ""));
    } finally {
      setSendingRequest(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2 font-semibold">
            <Phone className="h-4 w-4" style={{ color: "var(--accent)" }} />
            <span>{t("findNewFriendMenu")}</span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-lg p-1"
            title={t("close")}
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <form
          onSubmit={(e) => {
            e.preventDefault();
            void runSearch(phone);
          }}
          className="space-y-3 p-4"
        >
          <div className="flex items-center gap-2">
            <div
              className="flex flex-1 items-center gap-2 rounded-xl px-3 py-2"
              style={{ background: "var(--bg-surface)" }}
            >
              <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
              <input
                ref={inputRef}
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
                placeholder={t("findUserPhonePlaceholder")}
                className="w-full bg-transparent text-[14.5px] outline-none"
              />
            </div>
            <button
              type="submit"
              disabled={searching}
              className="tf-btn flex shrink-0 items-center gap-1.5 rounded-xl px-4 py-2 text-[14px] font-medium disabled:opacity-50"
            >
              {searching ? <Loader2 className="h-4 w-4 animate-spin" /> : t("findShortBtn")}
            </button>
          </div>

          {error && (
            <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-[13px] text-rose-300">
              {error}
            </div>
          )}

          {result && (
            <div
              className="rounded-xl p-3.5"
              style={{ background: "var(--bg-surface)", border: "1px solid var(--border-subtle)" }}
            >
              <div className="flex items-center gap-3">
                <ThreadAvatar
                  avatar={result.avatar}
                  title={result.display_name}
                  kind="direct"
                  size="md"
                />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="truncate text-[15px] font-semibold">
                      {result.display_name}
                    </span>
                    <span
                      className="shrink-0 rounded-full px-2 py-0.5 text-[11px] font-medium"
                      style={{
                        background: result.is_friend ? "var(--accent-soft)" : "var(--bg-hover)",
                        color: result.is_friend ? "var(--accent)" : "var(--text-secondary)",
                      }}
                    >
                      {result.is_self
                        ? t("badgeSelfAccount")
                        : result.is_friend
                          ? t("badgeFriend")
                          : t("badgeNotFriend")}
                    </span>
                    {!result.is_self && result.is_blocked === true && (
                      <span
                        className="inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium"
                        style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                      >
                        <Ban className="h-3 w-3" />
                        <span>{t("friendBlockedBadge")}</span>
                      </span>
                    )}
                    {!result.is_self && !result.is_friend && requestSent && (
                      <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-emerald-500/15 px-2 py-0.5 text-[11px] font-medium text-emerald-400">
                        <Check className="h-3 w-3" />
                        <span>{t("friendRequestSentBadge")}</span>
                      </span>
                    )}
                  </div>
                  {result.phone_number && (
                    <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {result.phone_number}
                    </div>
                  )}
                </div>
              </div>

              {actionError && (
                <div
                  role="alert"
                  className="mt-2.5 rounded-xl px-3 py-2 text-xs"
                  style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                >
                  {actionError}
                </div>
              )}

              {actionNotice && (
                <div className="mt-2.5 rounded-xl bg-emerald-500/15 px-3 py-2 text-xs text-emerald-400">
                  {actionNotice}
                </div>
              )}

              <div className="mt-3 flex flex-wrap items-center gap-2">
                <button
                  type="button"
                  onClick={() => {
                    onOpenProfile(result);
                    onClose();
                  }}
                  className="hover-menu flex min-w-[100px] flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[13px] font-medium transition"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <User className="h-4 w-4" />
                  <span>{t("viewProfileBtn")}</span>
                </button>
                {!result.is_self && !result.is_friend && (
                  <button
                    type="button"
                    disabled={sendingRequest || requestSent}
                    onClick={() => void handleQuickAddFriend()}
                    className="hover-menu flex min-w-[100px] flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[13px] font-medium transition disabled:opacity-65"
                    style={{
                      background: requestSent ? "rgba(16, 185, 129, 0.15)" : "var(--accent-soft)",
                      border: "1px solid var(--border-subtle)",
                      color: requestSent ? "#34d399" : "var(--accent)",
                    }}
                  >
                    {sendingRequest ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : requestSent ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      <UserPlus className="h-4 w-4" />
                    )}
                    <span>{requestSent ? t("friendRequestSentBadge") : t("friendAddBtn")}</span>
                  </button>
                )}
                {!result.is_self && (
                  <button
                    type="button"
                    onClick={() => {
                      onStartChat(result);
                      onClose();
                    }}
                    className="tf-btn flex min-w-[100px] flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[13px] font-semibold"
                  >
                    <MessageSquare className="h-4 w-4" />
                    <span>{t("startChatBtn")}</span>
                  </button>
                )}
              </div>
            </div>
          )}
        </form>
      </div>
    </div>
  );
}
