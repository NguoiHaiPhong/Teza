import { useCallback, useEffect, useRef, useState } from "react";
import { Ban, Check, Loader2, MessageSquare, Phone, Search, User, UserPlus, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n, type UiLanguage } from "../i18n";
import type { UserProfileDto } from "../types";
import { isImeComposingEvent } from "../utils/searchNormalize";
import { ThreadAvatar } from "./ThreadAvatar";

const COUNTRY_CODE_OPTIONS = [
  { code: "84", label: "+84 VN" },
  { code: "82", label: "+82 KR" },
] as const;

function inferInitialCountryCode(raw: string, lang: UiLanguage): string {
  const cleaned = raw.trim().replace(/[\s\-().\t]/g, "");
  if (/^\+?82\d{7,}$/.test(cleaned)) return "82";
  if (/^\+?84\d{7,}$/.test(cleaned)) return "84";
  return lang === "ko" ? "82" : "84";
}

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
  const { lang, t } = useI18n();
  const defaultCountryCode = lang === "ko" ? "82" : "84";
  const [phone, setPhone] = useState("");
  const [countryCode, setCountryCode] = useState<string>(defaultCountryCode);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<UserProfileDto | null>(null);
  const [sendingRequest, setSendingRequest] = useState(false);
  const [requestSent, setRequestSent] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const searchSeqRef = useRef(0);
  const tRef = useRef(t);
  tRef.current = t;

  const resetState = useCallback(() => {
    searchSeqRef.current += 1;
    setPhone("");
    setCountryCode(defaultCountryCode);
    setError(null);
    setResult(null);
    setSearching(false);
    setSendingRequest(false);
    setRequestSent(false);
    setActionError(null);
    setActionNotice(null);
  }, [defaultCountryCode]);

  const handleClose = useCallback(() => {
    resetState();
    onClose();
  }, [onClose, resetState]);

  const runSearch = useCallback(async (raw: string, selectedCode: string) => {
    const query = raw.trim();
    const seq = ++searchSeqRef.current;
    if (!query) {
      setSearching(false);
      setError(tRef.current("findUserEmptyPhoneError"));
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
      const effectiveCountry = query.startsWith("+") ? undefined : selectedCode;
      const found = await bridge.findUserByPhone(query, effectiveCountry);
      if (searchSeqRef.current !== seq) return;
      setResult(found);
    } catch (err) {
      if (searchSeqRef.current !== seq) return;
      setError(String(err).replace(/^Error:\s*/i, ""));
    } finally {
      if (searchSeqRef.current === seq) {
        setSearching(false);
      }
    }
  }, []);

  useEffect(() => {
    if (!open) {
      resetState();
      return;
    }
    const init = initialPhone.trim();
    const initCode = inferInitialCountryCode(init, lang);
    searchSeqRef.current += 1;
    setPhone(init);
    setCountryCode(initCode);
    setError(null);
    setResult(null);
    setSearching(false);
    setSendingRequest(false);
    setRequestSent(false);
    setActionError(null);
    setActionNotice(null);
    const focusTimer = window.setTimeout(() => inputRef.current?.focus(), 20);
    if (init) {
      void runSearch(init, initCode);
    }
    return () => {
      window.clearTimeout(focusTimer);
      searchSeqRef.current += 1;
    };
  }, [open, initialPhone, lang, resetState, runSearch]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e)) return;
      if (e.key === "Escape") handleClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, handleClose]);

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
      onClick={handleClose}
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
            onClick={handleClose}
            className="hover-menu rounded-lg p-1"
            title={t("close")}
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <form
          onSubmit={(e) => {
            e.preventDefault();
            void runSearch(phone, countryCode);
          }}
          className="space-y-3 p-4"
        >
          <div className="flex items-center gap-2">
            <select
              value={countryCode}
              onChange={(e) => setCountryCode(e.target.value)}
              aria-label={t("findUserCountryCodeLabel")}
              title={t("findUserCountryOverrideHint")}
              className={`shrink-0 rounded-xl px-2.5 py-2 text-[13px] font-medium outline-none transition ${
                phone.trim().startsWith("+") ? "opacity-60" : ""
              }`}
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
                color: "var(--text-primary)",
              }}
            >
              {COUNTRY_CODE_OPTIONS.map((opt) => (
                <option key={opt.code} value={opt.code}>
                  {opt.label}
                </option>
              ))}
            </select>
            <div
              className="flex min-w-0 flex-1 items-center gap-2 rounded-xl px-3 py-2"
              style={{ background: "var(--bg-surface)" }}
            >
              <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
              <input
                ref={inputRef}
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
                onKeyDown={(e) => {
                  if (isImeComposingEvent(e)) {
                    if (e.key === "Enter") e.preventDefault();
                    return;
                  }
                }}
                placeholder={t("findUserPhonePlaceholder")}
                className="w-full min-w-0 bg-transparent text-[14.5px] outline-none"
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
                    const target = result;
                    resetState();
                    onOpenProfile(target);
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
                      const target = result;
                      resetState();
                      onStartChat(target);
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
