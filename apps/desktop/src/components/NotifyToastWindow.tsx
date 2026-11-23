import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { X, Zap } from "lucide-react";
import { bridge, isTauri } from "../bridge";
import { useI18n } from "../i18n";
import type { NotifyToastPayload } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

export function NotifyToastWindow() {
  const { t } = useI18n();
  const [payload, setPayload] = useState<NotifyToastPayload | null>(null);
  const [buzzPulseKey, setBuzzPulseKey] = useState(0);

  useEffect(() => {
    void bridge
      .getCurrentNotifyPopup()
      .then((cur) => {
        if (cur) {
          setPayload(cur);
          if (cur.is_buzz) setBuzzPulseKey((k) => k + 1);
          document.documentElement.setAttribute("data-theme", cur.theme || "triforce");
        }
      })
      .catch(() => undefined);

    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<NotifyToastPayload>("core://notify-payload", (ev) => {
      if (cancelled) return;
      setPayload(ev.payload);
      if (ev.payload.is_buzz) setBuzzPulseKey((k) => k + 1);
      document.documentElement.setAttribute("data-theme", ev.payload.theme || "triforce");
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  if (!payload) {
    return (
      <div
        className="h-full w-full"
        style={{ background: "var(--bg-sidebar)" }}
      />
    );
  }

  const isBuzz = Boolean(payload.is_buzz);
  const buzzStreak = payload.buzz_streak ?? 1;

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => void bridge.clickNotifyPopup(payload.thread_id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          void bridge.clickNotifyPopup(payload.thread_id);
        } else if (e.key === "Escape") {
          void bridge.dismissNotifyPopup();
        }
      }}
      className="relative flex h-full w-full cursor-pointer items-center overflow-hidden rounded-none transition hover:brightness-105"
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
          isBuzz ? "notify-buzz-shaking" : ""
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
              title={t("notifyCloseTitle")}
              onClick={(e) => {
                e.stopPropagation();
                void bridge.dismissNotifyPopup();
              }}
              className="hover-menu -mr-1 shrink-0 rounded-md p-0.5 transition"
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
