import { useEffect, useRef, useState } from "react";
import { Clock, Network, QrCode, RefreshCw } from "lucide-react";
import { useI18n } from "../i18n";
import type { RelayConfigSummary } from "../types";

interface Props {
  qrSvg: string;
  statusText: string;
  errorText: string | null;
  relayCfg?: RelayConfigSummary | null;
  qrTimeoutSec?: number | null;
  qrIssuedAtMs?: number | null;
  refreshing?: boolean;
  onOpenRelaySettings?: () => void;
  onRefresh: () => void;
  onEnterDemo: () => void;
}

function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.ceil(seconds));
  if (s >= 60) {
    const mins = Math.floor(s / 60);
    const rem = s % 60;
    return `${mins}:${String(rem).padStart(2, "0")}`;
  }
  return `${s}s`;
}

export function QrLoginModal({
  qrSvg,
  statusText,
  errorText,
  relayCfg,
  qrTimeoutSec = null,
  qrIssuedAtMs = null,
  refreshing = false,
  onOpenRelaySettings,
  onRefresh,
  onEnterDemo,
}: Props) {
  const { t } = useI18n();
  const [fallbackIssuedAtMs, setFallbackIssuedAtMs] = useState<number | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const onRefreshRef = useRef(onRefresh);
  onRefreshRef.current = onRefresh;
  const autoRefreshedKeyRef = useRef<string | null>(null);

  useEffect(() => {
    if (!qrSvg) {
      setFallbackIssuedAtMs(null);
      return;
    }
    setFallbackIssuedAtMs(Date.now());
    setNowMs(Date.now());
  }, [qrSvg]);

  const effectiveTimeoutSec =
    typeof qrTimeoutSec === "number" && Number.isFinite(qrTimeoutSec) && qrTimeoutSec > 0
      ? qrTimeoutSec
      : null;
  const effectiveIssuedAtMs =
    typeof qrIssuedAtMs === "number" && Number.isFinite(qrIssuedAtMs) && qrIssuedAtMs > 0
      ? qrIssuedAtMs
      : fallbackIssuedAtMs;

  const expiresAtMs =
    qrSvg && effectiveTimeoutSec !== null && effectiveIssuedAtMs !== null
      ? effectiveIssuedAtMs + effectiveTimeoutSec * 1000
      : null;

  const remainingSec =
    expiresAtMs !== null ? Math.max(0, Math.ceil((expiresAtMs - nowMs) / 1000)) : null;

  useEffect(() => {
    if (!qrSvg || expiresAtMs === null || refreshing) return;
    setNowMs(Date.now());
    const timer = window.setInterval(() => {
      setNowMs(Date.now());
    }, 500);
    return () => window.clearInterval(timer);
  }, [qrSvg, expiresAtMs, refreshing]);

  useEffect(() => {
    if (!qrSvg || expiresAtMs === null || refreshing) return;
    if (remainingSec === null || remainingSec > 0) return;
    const key = `${effectiveIssuedAtMs ?? 0}:${qrSvg.slice(0, 64)}`;
    if (autoRefreshedKeyRef.current === key) return;
    autoRefreshedKeyRef.current = key;
    onRefreshRef.current();
  }, [qrSvg, expiresAtMs, effectiveIssuedAtMs, remainingSec, refreshing]);

  return (
    <div className="flex h-full w-full items-center justify-center p-6" style={{ background: "var(--bg-stage)" }}>
      <div
        className="w-full max-w-md rounded-2xl p-8 text-center shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
      >
        <div
          className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full"
          style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
        >
          <QrCode className="h-6 w-6" />
        </div>
        <h1 className="text-xl font-semibold">{t("qrTitle")}</h1>
        <p className="mt-1 text-sm" style={{ color: "var(--text-secondary)" }}>
          {t("qrSubtitle")}
        </p>

        <div className="mx-auto my-6 flex h-56 w-56 items-center justify-center rounded-xl bg-white p-3">
          {qrSvg ? (
            <div className="h-full w-full [&>svg]:h-full [&>svg]:w-full" dangerouslySetInnerHTML={{ __html: qrSvg }} />
          ) : (
            <div className="text-xs text-slate-500">{t("qrLoading")}</div>
          )}
        </div>

        <p className="text-sm font-medium" style={{ color: "var(--accent)" }}>
          {statusText}
        </p>
        {remainingSec !== null && !refreshing && (
          <p
            className={`mt-1.5 inline-flex items-center justify-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium tabular-nums ${
              remainingSec <= 30 ? "bg-amber-500/15 text-amber-400" : ""
            }`}
            style={
              remainingSec > 30
                ? { background: "var(--bg-surface)", color: "var(--text-secondary)" }
                : undefined
            }
          >
            <Clock className="h-3.5 w-3.5" />
            <span>{t("qrExpiresIn", { time: formatCountdown(remainingSec) })}</span>
          </p>
        )}
        {relayCfg?.enabled && (
          <p className="mt-1.5 text-xs font-medium text-emerald-400">
            {t("qrRelayConnectedHint", {
              mode: relayCfg.mode.toUpperCase(),
              host: relayCfg.host,
              port: relayCfg.port,
            })}
          </p>
        )}
        {errorText && (
          <p
            className="mt-2 rounded-lg px-3 py-2 text-xs"
            style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
          >
            {errorText}
          </p>
        )}

        <div className="mt-6 flex flex-wrap items-center justify-center gap-2.5">
          <button
            type="button"
            disabled={refreshing}
            onClick={onRefresh}
            className="tf-btn inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-semibold disabled:cursor-not-allowed disabled:opacity-60"
          >
            <RefreshCw className={`h-4 w-4 ${refreshing ? "animate-spin" : ""}`} />
            <span>{refreshing ? t("qrRefreshing") : t("qrRegenerate")}</span>
          </button>
          {onOpenRelaySettings && (
            <button
              type="button"
              onClick={onOpenRelaySettings}
              className="inline-flex items-center gap-1.5 rounded-lg px-3.5 py-2 text-sm font-medium transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: relayCfg?.enabled ? "var(--accent)" : "var(--text-primary)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <Network className="h-4 w-4" />
              {t("qrRelayBtn")}
            </button>
          )}
          <button
            type="button"
            onClick={onEnterDemo}
            className="rounded-lg px-3.5 py-2 text-sm font-medium transition hover:opacity-80"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            {t("qrPreviewBtn")}
          </button>
        </div>
      </div>
    </div>
  );
}
