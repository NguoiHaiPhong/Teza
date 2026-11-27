import { type MouseEvent, useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { X, Zap } from "lucide-react";
import { bridge, isTauri } from "../bridge";
import { useI18n } from "../i18n";
import type { NotifyToastPayload } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";
import {
  classifyNotifyPopupKey,
  isExplicitPointerClick,
  isPrimaryMouseDown,
} from "./notifyPopupInteraction";

const FALLBACK_RETRY_DELAYS_MS = [150, 450] as const;
const FALLBACK_AUTO_DISMISS_MS = 2500;
const CLIENT_FAILSAFE_GRACE_MS = 1500;
const POINTER_DISMISS_DEDUPE_MS = 300;

export function NotifyToastWindow() {
  const { t } = useI18n();
  const [payload, setPayload] = useState<NotifyToastPayload | null>(null);
  const [payloadSeq, setPayloadSeq] = useState(0);
  const [buzzPulseKey, setBuzzPulseKey] = useState(0);
  const [isHovered, setIsHovered] = useState(false);
  const [shakeSuppressed, setShakeSuppressed] = useState(false);

  const payloadSeqRef = useRef(0);
  const hiddenAtSeqRef = useRef(-1);
  const hasEventPayloadRef = useRef(false);
  const suppressOpenForSeqRef = useRef(-1);
  const lastPointerDismissAtRef = useRef(0);
  const failsafeTimerRef = useRef<number | null>(null);

  const clearFailsafeTimer = useCallback(() => {
    if (failsafeTimerRef.current !== null) {
      window.clearTimeout(failsafeTimerRef.current);
      failsafeTimerRef.current = null;
    }
  }, []);

  const applyIncomingPayload = useCallback(
    (next: NotifyToastPayload) => {
      clearFailsafeTimer();
      const nextSeq = payloadSeqRef.current + 1;
      payloadSeqRef.current = nextSeq;
      hiddenAtSeqRef.current = -1;
      suppressOpenForSeqRef.current = -1;
      setPayload(next);
      setPayloadSeq(nextSeq);
      setIsHovered(false);
      setShakeSuppressed(false);
      if (next.is_buzz) {
        setBuzzPulseKey((k) => k + 1);
      }
      document.documentElement.setAttribute("data-theme", next.theme || "triforce");
    },
    [clearFailsafeTimer],
  );

  const dismissNow = useCallback(() => {
    clearFailsafeTimer();
    hiddenAtSeqRef.current = payloadSeqRef.current;
    suppressOpenForSeqRef.current = payloadSeqRef.current;
    void bridge.dismissNotifyPopup().catch(() => undefined);
  }, [clearFailsafeTimer]);

  const openThreadNow = useCallback(
    (threadId: string) => {
      if (suppressOpenForSeqRef.current === payloadSeqRef.current) {
        return;
      }
      clearFailsafeTimer();
      hiddenAtSeqRef.current = payloadSeqRef.current;
      suppressOpenForSeqRef.current = payloadSeqRef.current;
      void bridge.clickNotifyPopup(threadId).catch(() => undefined);
    },
    [clearFailsafeTimer],
  );

  const handleDismissMouseDown = useCallback(
    (e: MouseEvent) => {
      if (!isPrimaryMouseDown(e)) return;
      e.preventDefault();
      e.stopPropagation();
      lastPointerDismissAtRef.current = Date.now();
      dismissNow();
    },
    [dismissNow],
  );

  const handleDismissClick = useCallback(
    (e: MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (!isExplicitPointerClick(e)) {
        return;
      }
      if (Date.now() - lastPointerDismissAtRef.current < POINTER_DISMISS_DEDUPE_MS) {
        return;
      }
      dismissNow();
    },
    [dismissNow],
  );

  // Initial fetch, live event subscription, theme bootstrap, and keyboard/focus defense.
  useEffect(() => {
    try {
      const savedTheme = localStorage.getItem("litechat.theme");
      if (savedTheme && !document.documentElement.getAttribute("data-theme")) {
        document.documentElement.setAttribute("data-theme", savedTheme);
      }
    } catch {
      // Ignore storage access restrictions
    }

    let cancelled = false;
    let unlistenPayload: (() => void) | undefined;
    let unlistenHidden: (() => void) | undefined;

    void bridge
      .getCurrentNotifyPopup()
      .then((cur) => {
        if (cancelled || hasEventPayloadRef.current || !cur) return;
        applyIncomingPayload(cur);
      })
      .catch(() => undefined);

    if (isTauri()) {
      void listen<NotifyToastPayload>("core://notify-payload", (ev) => {
        if (cancelled) return;
        hasEventPayloadRef.current = true;
        applyIncomingPayload(ev.payload);
      }).then((fn) => {
        if (cancelled) fn();
        else unlistenPayload = fn;
      });
      void listen("core://notify-hidden", () => {
        if (cancelled) return;
        hiddenAtSeqRef.current = payloadSeqRef.current;
        clearFailsafeTimer();
      }).then((fn) => {
        if (cancelled) fn();
        else unlistenHidden = fn;
      });
    }

    const onWindowKeyDown = (e: KeyboardEvent) => {
      const action = classifyNotifyPopupKey(e);
      if (action === "dismiss") {
        e.preventDefault();
        dismissNow();
      } else if (action === "block") {
        // Never allow Enter/Space in the floating popup to activate or switch chats.
        e.preventDefault();
        e.stopPropagation();
      }
    };
    const onWindowFocus = () => {
      if (document.activeElement instanceof HTMLElement) {
        document.activeElement.blur();
      }
    };
    const onVisibilityChange = () => {
      if (document.visibilityState !== "visible") {
        hiddenAtSeqRef.current = payloadSeqRef.current;
        clearFailsafeTimer();
      }
    };
    window.addEventListener("keydown", onWindowKeyDown, true);
    window.addEventListener("focus", onWindowFocus, true);
    document.addEventListener("visibilitychange", onVisibilityChange);

    return () => {
      cancelled = true;
      unlistenPayload?.();
      unlistenHidden?.();
      window.removeEventListener("keydown", onWindowKeyDown, true);
      window.removeEventListener("focus", onWindowFocus, true);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [applyIncomingPayload, clearFailsafeTimer, dismissNow]);

  // If mounted before payload arrives (or if initial event was missed), retry
  // fetching current payload at 150ms & 450ms and auto-dismiss after 2500ms.
  useEffect(() => {
    if (payload) return;
    let cancelled = false;

    const retryFetch = () => {
      void bridge
        .getCurrentNotifyPopup()
        .then((cur) => {
          if (cancelled || hasEventPayloadRef.current || !cur) return;
          applyIncomingPayload(cur);
        })
        .catch(() => undefined);
    };

    const retryTimers = FALLBACK_RETRY_DELAYS_MS.map((delayMs) =>
      window.setTimeout(retryFetch, delayMs),
    );
    const autoDismissTimer = window.setTimeout(() => {
      if (!cancelled && document.visibilityState === "visible") {
        dismissNow();
      }
    }, FALLBACK_AUTO_DISMISS_MS);

    return () => {
      cancelled = true;
      retryTimers.forEach((id) => window.clearTimeout(id));
      window.clearTimeout(autoDismissTimer);
    };
  }, [payload, applyIncomingPayload, dismissNow]);

  // Client-side failsafe auto-hide timer keyed by monotonic payload sequence.
  useEffect(() => {
    clearFailsafeTimer();
    if (!payload || payloadSeq === 0) return;
    const armedSeq = payloadSeq;
    const clampedMs = Math.min(Math.max(payload.duration_ms || 4500, 2500), 15000);
    failsafeTimerRef.current = window.setTimeout(() => {
      failsafeTimerRef.current = null;
      if (
        document.visibilityState !== "visible" ||
        payloadSeqRef.current !== armedSeq ||
        hiddenAtSeqRef.current === armedSeq
      ) {
        return;
      }
      dismissNow();
    }, clampedMs + CLIENT_FAILSAFE_GRACE_MS);

    return () => {
      clearFailsafeTimer();
    };
  }, [payload, payloadSeq, clearFailsafeTimer, dismissNow]);

  // Never render an unclickable dead rectangle when payload is missing.
  if (!payload) {
    return (
      <div
        role="button"
        tabIndex={-1}
        onMouseDown={handleDismissMouseDown}
        onClick={handleDismissClick}
        className="relative flex h-full w-full cursor-pointer select-none items-center justify-between gap-2 overflow-hidden rounded-none px-3 py-2"
        style={{
          background: "var(--bg-sidebar)",
          color: "var(--text-primary)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <span
          aria-hidden="true"
          className="absolute inset-y-0 left-0 w-[3px]"
          style={{ background: "var(--accent)" }}
        />
        <div className="min-w-0 flex-1 pl-1">
          <div
            className="truncate text-[13px] font-semibold leading-tight"
            style={{ color: "var(--text-primary)" }}
          >
            Teza
          </div>
          <div
            className="mt-0.5 truncate text-[11px] leading-tight"
            style={{ color: "var(--text-secondary)" }}
          >
            {t("notifyCloseTitle")}
          </div>
        </div>
        <button
          type="button"
          tabIndex={-1}
          data-notify-close="true"
          aria-label={t("notifyCloseTitle")}
          title={t("notifyCloseTitle")}
          onMouseDown={handleDismissMouseDown}
          onClick={handleDismissClick}
          className="hover-menu -mr-1 shrink-0 rounded-md p-1 transition"
          style={{ color: "var(--text-secondary)" }}
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  const isBuzz = Boolean(payload.is_buzz);
  const buzzStreak = payload.buzz_streak ?? 1;
  const shouldShake = isBuzz && !isHovered && !shakeSuppressed;

  return (
    <div
      role="button"
      tabIndex={-1}
      onMouseEnter={() => {
        setIsHovered(true);
        setShakeSuppressed(true);
      }}
      onMouseMove={() => {
        if (!isHovered) {
          setIsHovered(true);
          setShakeSuppressed(true);
        }
      }}
      onMouseLeave={() => setIsHovered(false)}
      onMouseDown={(e) => {
        if (!isPrimaryMouseDown(e)) return;
        if (e.target instanceof Element && e.target.closest("[data-notify-close]")) {
          return;
        }
        e.preventDefault();
        openThreadNow(payload.thread_id);
      }}
      onClick={(e) => {
        if (!isExplicitPointerClick(e)) return;
        if (e.target instanceof Element && e.target.closest("[data-notify-close]")) {
          return;
        }
        openThreadNow(payload.thread_id);
      }}
      className="relative flex h-full w-full cursor-pointer select-none items-center overflow-hidden rounded-none transition hover:brightness-105"
      style={{
        background: "var(--bg-sidebar)",
        color: "var(--text-primary)",
        border: isBuzz ? "1px solid rgba(245, 158, 11, 0.55)" : "1px solid var(--border-subtle)",
      }}
    >
      <span
        aria-hidden="true"
        className="absolute inset-y-0 left-0 w-[3px]"
        style={{ background: isBuzz ? "#f59e0b" : "var(--accent)" }}
      />
      <div
        key={isBuzz ? `buzz-${buzzPulseKey}` : "normal"}
        className={`flex h-full w-full items-center gap-2.5 px-3 py-2 ${
          shouldShake ? "notify-buzz-shaking" : ""
        }`}
      >
        <div className="shrink-0 pl-0.5">
          <ThreadAvatar
            avatar={payload.avatar}
            title={payload.title}
            kind="direct"
            size="sm"
          />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-1.5">
            <div className="flex min-w-0 items-center gap-1.5">
              <div
                className="min-w-0 truncate text-[13px] font-semibold leading-tight"
                style={{ color: "var(--text-primary)" }}
              >
                {payload.title}
              </div>
              {isBuzz && (
                <span className="inline-flex shrink-0 items-center gap-0.5 rounded bg-amber-500/20 px-1.5 py-0.5 text-[10px] font-bold leading-none text-amber-400">
                  <Zap className="h-2.5 w-2.5 fill-amber-400" />
                  BUZZ{buzzStreak >= 2 ? ` ×${buzzStreak}` : "!"}
                </span>
              )}
            </div>
            <button
              type="button"
              tabIndex={-1}
              data-notify-close="true"
              aria-label={t("notifyCloseTitle")}
              title={t("notifyCloseTitle")}
              onMouseDown={handleDismissMouseDown}
              onClick={handleDismissClick}
              className="hover-menu -mr-1 shrink-0 rounded-md p-1 transition"
              style={{ color: "var(--text-secondary)" }}
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
          <div
            className="mt-0.5 line-clamp-2 break-words text-[12px] leading-[1.35]"
            style={{ color: "var(--text-secondary)" }}
          >
            {payload.body}
          </div>
          <div
            className="mt-0.5 truncate text-[10.5px] font-medium leading-tight"
            style={{ color: isBuzz ? "#f59e0b" : "var(--accent)" }}
          >
            {t("notifyClickHint")}
          </div>
        </div>
      </div>
    </div>
  );
}
