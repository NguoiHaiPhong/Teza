import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ArrowDownToLine, CheckCircle2, RefreshCw, Sparkles, X } from "lucide-react";
import { relaunch } from "@tauri-apps/plugin-process";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { AppUpdateInfo } from "../types";
import { isImeComposingEvent } from "../utils/searchNormalize";

export interface UpdateInstallProgress {
  downloadedBytes: number;
  totalBytes: number | null;
  stageText?: string;
  percent?: number | null;
}

interface Props {
  open: boolean;
  isLocked?: boolean;
  currentVersion: string;
  availableUpdate: AppUpdateInfo | null;
  checking: boolean;
  errorText: string | null;
  lastCheckedAt: number | null;
  installing?: boolean;
  progress?: UpdateInstallProgress | null;
  error?: string | null;
  onInstall?: () => Promise<void> | void;
  onCheckNow: () => Promise<void>;
  onClose: () => void;
}

export function UpdateModal({
  open,
  isLocked = false,
  currentVersion,
  availableUpdate,
  checking,
  errorText,
  lastCheckedAt,
  installing: installingProp,
  progress: progressProp,
  error: errorProp,
  onInstall,
  onCheckNow,
  onClose,
}: Props) {
  const { t } = useI18n();
  const [localInstalling, setLocalInstalling] = useState(false);
  const [localInstallError, setLocalInstallError] = useState<string | null>(null);
  const [localDownloadedBytes, setLocalDownloadedBytes] = useState(0);
  const [localTotalBytes, setLocalTotalBytes] = useState<number | null>(null);
  const [localStageText, setLocalStageText] = useState("");
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const effectiveInstalling = installingProp ?? localInstalling;
  const effectiveDownloadedBytes = progressProp ? progressProp.downloadedBytes : localDownloadedBytes;
  const effectiveTotalBytes = progressProp ? progressProp.totalBytes : localTotalBytes;
  const effectiveStageText =
    progressProp?.stageText !== undefined
      ? progressProp.stageText
      : localStageText || (effectiveInstalling ? t("updateDownloading") : "");
  const effectiveInstallError = errorProp !== undefined ? errorProp : localInstallError;

  const percent =
    progressProp?.percent !== undefined
      ? progressProp.percent
      : effectiveTotalBytes && effectiveTotalBytes > 0
        ? Math.min(100, Math.round((effectiveDownloadedBytes / effectiveTotalBytes) * 100))
        : null;

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (!effectiveInstalling) {
          onClose();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, effectiveInstalling, onClose]);

  if (!open) return null;

  const handleRequestClose = () => {
    if (effectiveInstalling) return;
    onClose();
  };

  const handleInstall = async () => {
    if (!availableUpdate || effectiveInstalling) return;
    if (onInstall) {
      await onInstall();
      return;
    }
    setLocalInstalling(true);
    setLocalInstallError(null);
    setLocalDownloadedBytes(0);
    setLocalTotalBytes(null);
    setLocalStageText(t("updateDownloading"));
    let unlisten: UnlistenFn | null = null;
    try {
      unlisten = await listen<{ event: string; total?: number | null; chunk?: number }>(
        "core://update-progress",
        (ev) => {
          if (!mountedRef.current) return;
          const p = ev.payload;
          if (p.event === "started") {
            setLocalTotalBytes(p.total ?? null);
            setLocalStageText(t("updateDownloadingPkg"));
          } else if (p.event === "progress" && p.chunk) {
            setLocalDownloadedBytes((prev) => prev + (p.chunk ?? 0));
          } else if (p.event === "finished") {
            setLocalStageText(t("updateFinishingRestart"));
          }
        },
      );
      await bridge.installAppUpdate();
      await relaunch();
    } catch (err) {
      if (mountedRef.current) {
        setLocalInstallError(String(err));
        setLocalInstalling(false);
      }
    } finally {
      unlisten?.();
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
      onClick={handleRequestClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("updateModalTitle")}
        className="w-full max-w-md overflow-hidden rounded-2xl shadow-2xl"
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
          <div className="flex items-center gap-2">
            <ArrowDownToLine className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <h3 className="text-[16px] font-semibold">{t("updateModalTitle")}</h3>
          </div>
          <button
            type="button"
            disabled={effectiveInstalling}
            onClick={handleRequestClose}
            className="rounded-lg p-1.5 hover:bg-black/10 disabled:cursor-not-allowed disabled:opacity-40"
            title={t("close")}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-4 p-5">
          <div
            className="flex items-center justify-between rounded-xl px-4 py-3 text-[13.5px]"
            style={{ background: "var(--bg-surface)" }}
          >
            <span style={{ color: "var(--text-secondary)" }}>{t("currentVersionLabel")}</span>
            <span className="font-mono font-semibold">v{currentVersion || "0.1.0"}</span>
          </div>

          {availableUpdate ? (
            <div
              className="space-y-2 rounded-xl p-4"
              style={{
                background: "var(--accent-soft)",
                border: "1px solid var(--accent)",
              }}
            >
              <div className="flex items-center gap-2 text-[15px] font-semibold" style={{ color: "var(--accent)" }}>
                <Sparkles className="h-4 w-4" />
                <span>{t("newVersionReady", { version: availableUpdate.version })}</span>
              </div>
              {availableUpdate.body && (
                <p
                  className="max-h-36 overflow-y-auto whitespace-pre-wrap text-[13px] leading-relaxed"
                  style={{ color: "var(--text-primary)" }}
                >
                  {availableUpdate.body}
                </p>
              )}
            </div>
          ) : (
            <div
              className="flex items-center gap-2.5 rounded-xl px-4 py-3 text-[13.5px]"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <CheckCircle2 className="h-4 w-4 shrink-0 text-emerald-400" />
              <span>
                {checking
                  ? t("updateCheckingGithub")
                  : lastCheckedAt
                    ? t("updateUpToDate")
                    : t("updateClickCheckHint")}
              </span>
            </div>
          )}

          {effectiveInstalling && (
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs" style={{ color: "var(--text-secondary)" }}>
                <span>{effectiveStageText}</span>
                {percent !== null && <span>{percent}%</span>}
              </div>
              <div
                className="h-2 w-full overflow-hidden rounded-full"
                style={{ background: "var(--bg-surface)" }}
              >
                <div
                  className="h-full transition-all"
                  style={{
                    width: `${percent ?? 35}%`,
                    background: "var(--accent)",
                  }}
                />
              </div>
            </div>
          )}

          {(errorText || effectiveInstallError) && (
            <div
              role="alert"
              className="rounded-xl px-3 py-2 text-[13px]"
              style={{
                background: "var(--danger-bg)",
                color: "var(--danger-text)",
              }}
            >
              {effectiveInstallError || errorText}
            </div>
          )}

          <div className="flex justify-end gap-2 pt-1">
            <button
              type="button"
              disabled={checking || effectiveInstalling}
              onClick={() => void onCheckNow()}
              className="flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-[14px] font-medium disabled:opacity-55"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <RefreshCw className={`h-4 w-4 ${checking ? "animate-spin" : ""}`} />
              <span>{t("checkBtn")}</span>
            </button>
            {availableUpdate && (
              <button
                type="button"
                disabled={effectiveInstalling}
                onClick={() => void handleInstall()}
                className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-[14px] font-semibold disabled:opacity-60"
              >
                <ArrowDownToLine className="h-4 w-4" />
                <span>{effectiveInstalling ? t("updateInstallingBtn") : t("updateInstallRestartBtn")}</span>
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
