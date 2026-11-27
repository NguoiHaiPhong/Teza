import { useEffect, useRef, useState } from "react";
import { Eye, EyeOff, Lock, LogOut, Mic, MicOff, Phone, PhoneOff, Video } from "lucide-react";
import { useI18n } from "../i18n";
import {
  PASSCODE_LOCKOUT_THRESHOLD,
  type PasscodeConfigWithLockout,
  getPasscodeLockoutRemainingMs,
  loadPasscodeConfig,
  recordPasscodeFailure,
  resetPasscodeLockout,
} from "../utils/passcode";
import { isImeComposingEvent } from "../utils/searchNormalize";

export interface PasscodeActiveCallBanner {
  peerName: string;
  isVideo: boolean;
  muted: boolean;
  onToggleMute: () => void;
  onHangup: () => void;
}

interface Props {
  open: boolean;
  myName?: string;
  activeCallBanner?: PasscodeActiveCallBanner | null;
  onUnlock: (pin: string) => Promise<boolean>;
  onLogout: () => void;
  onLockoutChange?: (cfg: PasscodeConfigWithLockout) => void;
}

const FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function getVisibleFocusables(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) => el.getClientRects().length > 0,
  );
}

export function PasscodeLockScreen({
  open,
  myName,
  activeCallBanner = null,
  onUnlock,
  onLogout,
  onLockoutChange,
}: Props) {
  const { t } = useI18n();
  const [pin, setPin] = useState("");
  const [showPin, setShowPin] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const [lockoutUntilMs, setLockoutUntilMs] = useState(0);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const mountedRef = useRef(true);
  const wasLockedOutRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const lockoutRemainingMs = getPasscodeLockoutRemainingMs({ lockoutUntilMs }, nowMs);
  const lockoutSeconds = Math.ceil(lockoutRemainingMs / 1000);
  const isLockedOut = lockoutRemainingMs > 0;

  useEffect(() => {
    if (!open) {
      setPin("");
      setError(null);
      setShowPin(false);
      setConfirmLogout(false);
      return;
    }
    const cfg = loadPasscodeConfig();
    const now = Date.now();
    const nextUntil = cfg.lockoutUntilMs ?? 0;
    setLockoutUntilMs(nextUntil);
    setNowMs(now);
    const currentlyLockedOut = getPasscodeLockoutRemainingMs({ lockoutUntilMs: nextUntil }, now) > 0;
    wasLockedOutRef.current = currentlyLockedOut;

    const focusInitial = () => {
      if (!currentlyLockedOut && inputRef.current && !inputRef.current.disabled) {
        inputRef.current.focus();
      } else {
        dialogRef.current?.focus();
      }
    };
    focusInitial();
    const timer = window.setTimeout(focusInitial, 30);
    return () => window.clearTimeout(timer);
  }, [open]);

  // Live countdown ticker while locked out
  useEffect(() => {
    if (!open || !isLockedOut) return;
    const interval = window.setInterval(() => {
      setNowMs(Date.now());
    }, 250);
    return () => window.clearInterval(interval);
  }, [open, isLockedOut]);

  // Re-focus PIN input automatically as soon as lockout cooldown expires
  useEffect(() => {
    if (!open) return;
    if (wasLockedOutRef.current && !isLockedOut) {
      setError(null);
      const timer = window.setTimeout(() => {
        inputRef.current?.focus();
      }, 20);
      wasLockedOutRef.current = false;
      return () => window.clearTimeout(timer);
    }
    wasLockedOutRef.current = isLockedOut;
  }, [open, isLockedOut]);

  // Strict keyboard focus trap & Escape isolation while locked
  useEffect(() => {
    if (!open) return;

    const onKeyDown = (e: KeyboardEvent) => {
      const root = dialogRef.current;
      if (!root) return;

      if (e.key === "Tab") {
        const focusables = getVisibleFocusables(root);
        if (focusables.length === 0) {
          e.preventDefault();
          e.stopPropagation();
          root.focus();
          return;
        }
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        const active = document.activeElement as HTMLElement | null;

        if (!active || !root.contains(active)) {
          e.preventDefault();
          e.stopPropagation();
          (e.shiftKey ? last : first).focus();
          return;
        }
        if (e.shiftKey && active === first) {
          e.preventDefault();
          e.stopPropagation();
          last.focus();
          return;
        }
        if (!e.shiftKey && active === last) {
          e.preventDefault();
          e.stopPropagation();
          first.focus();
          return;
        }
        return;
      }

      if (e.key === "Escape") {
        if (isImeComposingEvent(e)) return;
        e.preventDefault();
        e.stopPropagation();
        if (confirmLogout) {
          setConfirmLogout(false);
        }
      }
    };

    const onFocusIn = (e: FocusEvent) => {
      const root = dialogRef.current;
      if (!root) return;
      const target = e.target;
      if (target instanceof Node && !root.contains(target)) {
        if (inputRef.current && !inputRef.current.disabled) {
          inputRef.current.focus();
        } else {
          const focusables = getVisibleFocusables(root);
          (focusables[0] ?? root).focus();
        }
      }
    };

    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn, true);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn, true);
    };
  }, [open, confirmLogout]);

  if (!open) return null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!pin || checking) return;
    const latestCfg = loadPasscodeConfig();
    const now = Date.now();
    if (getPasscodeLockoutRemainingMs(latestCfg, now) > 0) {
      setLockoutUntilMs(latestCfg.lockoutUntilMs ?? 0);
      setNowMs(now);
      setError(null);
      return;
    }

    setChecking(true);
    setError(null);
    try {
      const ok = await onUnlock(pin);
      if (!mountedRef.current) return;
      if (!ok) {
        const updated = recordPasscodeFailure();
        const afterNow = Date.now();
        setLockoutUntilMs(updated.lockoutUntilMs ?? 0);
        setNowMs(afterNow);
        onLockoutChange?.(updated);

        if (getPasscodeLockoutRemainingMs(updated, afterNow) > 0) {
          setPin("");
          setError(null);
        } else {
          const attempts = updated.failedAttempts ?? 0;
          const remaining = Math.max(0, PASSCODE_LOCKOUT_THRESHOLD - attempts);
          setError(
            remaining > 0
              ? t("lockScreenAttemptsHint", { remaining })
              : t("lockScreenError"),
          );
          inputRef.current?.select();
        }
      } else {
        const updated = resetPasscodeLockout();
        setLockoutUntilMs(0);
        setPin("");
        setError(null);
        onLockoutChange?.(updated);
      }
    } finally {
      if (mountedRef.current) {
        setChecking(false);
      }
    }
  };

  return (
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby="passcode-lock-title"
      tabIndex={-1}
      className="fixed inset-0 z-[90] flex flex-col items-center justify-center gap-4 p-4 outline-none"
      style={{ background: "var(--bg-stage)", color: "var(--text-primary)" }}
    >
      {activeCallBanner && (
        <div
          className="flex w-full max-w-sm items-center justify-between gap-3 rounded-2xl px-4 py-2.5 shadow-xl"
          style={{
            background: "var(--bg-elevated)",
            border: "1px solid rgba(16, 185, 129, 0.45)",
          }}
        >
          <div className="flex min-w-0 items-center gap-2.5">
            <span className="relative flex h-2.5 w-2.5 shrink-0">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
              <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
            </span>
            <span className="shrink-0 text-emerald-400">
              {activeCallBanner.isVideo ? (
                <Video className="h-4 w-4" />
              ) : (
                <Phone className="h-4 w-4" />
              )}
            </span>
            <div className="min-w-0">
              <div className="truncate text-xs font-semibold">{activeCallBanner.peerName}</div>
              <div className="truncate text-[11px]" style={{ color: "var(--text-secondary)" }}>
                {activeCallBanner.isVideo ? t("callCardVideoTitle") : t("callCardVoiceTitle")}
              </div>
            </div>
          </div>

          <div className="flex shrink-0 items-center gap-1.5">
            <button
              type="button"
              onClick={activeCallBanner.onToggleMute}
              title={activeCallBanner.muted ? t("callUnmuteBtn") : t("callMuteBtn")}
              aria-label={activeCallBanner.muted ? t("callUnmuteBtn") : t("callMuteBtn")}
              className="flex items-center gap-1 rounded-xl px-2.5 py-1.5 text-xs font-medium transition"
              style={{
                background: activeCallBanner.muted
                  ? "rgba(245, 158, 11, 0.2)"
                  : "var(--bg-surface)",
                color: activeCallBanner.muted ? "#fbbf24" : "var(--text-primary)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              {activeCallBanner.muted ? (
                <MicOff className="h-3.5 w-3.5" />
              ) : (
                <Mic className="h-3.5 w-3.5" />
              )}
              <span>{activeCallBanner.muted ? t("callUnmuteBtn") : t("callMuteBtn")}</span>
            </button>
            <button
              type="button"
              onClick={activeCallBanner.onHangup}
              title={t("callHangupBtn")}
              aria-label={t("callHangupBtn")}
              className="flex items-center gap-1 rounded-xl bg-rose-600 px-2.5 py-1.5 text-xs font-semibold text-white transition hover:bg-rose-500"
            >
              <PhoneOff className="h-3.5 w-3.5" />
              <span>{t("callHangupBtn")}</span>
            </button>
          </div>
        </div>
      )}

      <div
        className="w-full max-w-sm rounded-3xl p-6 shadow-2xl"
        style={{
          background: "var(--bg-elevated)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <div className="mb-5 flex flex-col items-center text-center">
          <div
            className="mb-3 flex h-14 w-14 items-center justify-center rounded-2xl"
            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
          >
            <Lock className="h-7 w-7" />
          </div>
          <h2 id="passcode-lock-title" className="text-lg font-semibold">
            {t("lockScreenTitle")}
          </h2>
          <p className="mt-1 text-xs" style={{ color: "var(--text-secondary)" }}>
            {myName ? t("lockScreenAccount", { name: myName }) : t("lockScreenPrompt")}
          </p>
        </div>

        <form onSubmit={(e) => void submit(e)} className="space-y-3">
          <div
            className="flex items-center gap-2 rounded-xl px-3 py-2"
            style={{
              background: "var(--bg-surface)",
              border: `1px solid ${error || isLockedOut ? "rgba(244,63,94,0.6)" : "var(--border-subtle)"}`,
              opacity: isLockedOut ? 0.65 : 1,
            }}
          >
            <input
              ref={inputRef}
              type={showPin ? "text" : "password"}
              value={pin}
              disabled={checking || isLockedOut}
              onChange={(e) => {
                setPin(e.target.value);
                if (error) setError(null);
              }}
              placeholder={t("lockScreenPlaceholder")}
              autoComplete="off"
              className="min-w-0 flex-1 bg-transparent text-sm tracking-wider outline-none disabled:cursor-not-allowed"
            />
            <button
              type="button"
              disabled={isLockedOut}
              onClick={() => setShowPin((v) => !v)}
              aria-label={showPin ? t("webappAccountsHidePassword") : t("webappAccountsShowPassword")}
              className="rounded-lg p-1 opacity-70 transition hover:opacity-100 disabled:opacity-40"
            >
              {showPin ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
            </button>
          </div>

          {isLockedOut ? (
            <div
              role="alert"
              className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300 tabular-nums"
            >
              {t("lockScreenLockoutError", { seconds: lockoutSeconds })}
            </div>
          ) : error ? (
            <div role="alert" className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300">
              {error}
            </div>
          ) : null}

          <button
            type="submit"
            disabled={!pin || checking || isLockedOut}
            className="w-full rounded-xl py-2.5 text-sm font-semibold transition disabled:opacity-50"
            style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
          >
            {isLockedOut
              ? t("lockScreenLockedBtn", { seconds: lockoutSeconds })
              : checking
                ? t("lockScreenChecking")
                : t("lockScreenUnlock")}
          </button>
        </form>

        <div className="mt-5 border-t pt-3 text-center" style={{ borderColor: "var(--border-subtle)" }}>
          {!confirmLogout ? (
            <button
              type="button"
              onClick={() => setConfirmLogout(true)}
              className="inline-flex items-center gap-1.5 text-xs opacity-70 transition hover:opacity-100"
              style={{ color: "var(--text-secondary)" }}
            >
              <LogOut className="h-3.5 w-3.5" />
              <span>{t("lockScreenForgot")}</span>
            </button>
          ) : (
            <div className="flex items-center justify-center gap-2 text-xs">
              <span style={{ color: "var(--text-secondary)" }}>{t("lockScreenConfirmLogout")}</span>
              <button
                type="button"
                onClick={onLogout}
                className="rounded-lg bg-rose-500/20 px-2.5 py-1 font-medium text-rose-300 hover:bg-rose-500/30"
              >
                {t("logoutMenu")}
              </button>
              <button
                type="button"
                onClick={() => setConfirmLogout(false)}
                className="rounded-lg px-2 py-1 opacity-80 hover:opacity-100"
              >
                {t("cancel")}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
