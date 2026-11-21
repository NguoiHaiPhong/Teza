import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { X } from "lucide-react";
import { bridge, isTauri } from "../bridge";
import { useI18n } from "../i18n";
import type { NotifyToastPayload } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

export function NotifyToastWindow() {
  const { t } = useI18n();
  const [payload, setPayload] = useState<NotifyToastPayload | null>(null);

  useEffect(() => {
    void bridge
      .getCurrentNotifyPopup()
      .then((cur) => {
        if (cur) {
          setPayload(cur);
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
        className="h-screen w-screen"
        style={{ background: "var(--bg-sidebar)" }}
      />
    );
  }

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
      className="relative flex h-screen w-screen cursor-pointer items-center gap-3 overflow-hidden px-3.5 py-2.5 shadow-xl transition hover:brightness-105"
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
      <div className="shrink-0 pl-1">
        <ThreadAvatar
          avatar={payload.avatar}
          title={payload.title}
          kind="direct"
          size="md"
        />
      </div>
      <div className="min-w-0 flex-1 pr-5">
        <div className="truncate text-[13.5px] font-semibold" style={{ color: "var(--text-primary)" }}>
          {payload.title}
        </div>
        <div
          className="mt-0.5 line-clamp-2 text-[12px] leading-snug"
          style={{ color: "var(--text-secondary)" }}
        >
          {payload.body}
        </div>
        <div className="mt-0.5 text-[10.5px] font-medium" style={{ color: "var(--accent)" }}>
          {t("notifyClickHint")}
        </div>
      </div>
      <button
        type="button"
        title={t("notifyCloseTitle")}
        onClick={(e) => {
          e.stopPropagation();
          void bridge.dismissNotifyPopup();
        }}
        className="hover-menu absolute right-2 top-2 rounded-lg p-1 transition"
        style={{ color: "var(--text-secondary)" }}
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}
