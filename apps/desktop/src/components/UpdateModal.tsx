import { useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ArrowDownToLine, CheckCircle2, RefreshCw, Sparkles, X } from "lucide-react";
import { relaunch } from "@tauri-apps/plugin-process";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { AppUpdateInfo } from "../types";

interface Props {
  open: boolean;
  currentVersion: string;
  availableUpdate: AppUpdateInfo | null;
  checking: boolean;
  errorText: string | null;
  lastCheckedAt: number | null;
  onCheckNow: () => Promise<void>;
  onClose: () => void;
}

export function UpdateModal({
  open,
  currentVersion,
  availableUpdate,
  checking,
  errorText,
  lastCheckedAt,
  onCheckNow,
  onClose,
}: Props) {
  const { t } = useI18n();
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [downloadedBytes, setDownloadedBytes] = useState(0);
  const [totalBytes, setTotalBytes] = useState<number | null>(null);
  const [stageText, setStageText] = useState("");

  if (!open) return null;

  const percent =
    totalBytes && totalBytes > 0
      ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100))
      : null;

  const handleInstall = async () => {
    if (!availableUpdate || installing) return;
    setInstalling(true);
    setInstallError(null);
    setDownloadedBytes(0);
    setTotalBytes(null);
    setStageText(t("updateDownloading"));
    let unlisten: UnlistenFn | null = null;
    try {
      unlisten = await listen<{ event: string; total?: number | null; chunk?: number }>(
        "core://update-progress",
        (ev) => {
          const p = ev.payload;
          if (p.event === "started") {
            setTotalBytes(p.total ?? null);
            setStageText(t("updateDownloadingPkg"));
          } else if (p.event === "progress" && p.chunk) {
            setDownloadedBytes((prev) => prev + (p.chunk ?? 0));
          } else if (p.event === "finished") {
            setStageText(t("updateFinishingRestart"));
          }
        },
      );
      await bridge.installAppUpdate();
      await relaunch();
    } catch (err) {
      setInstallError(String(err));
      setInstalling(false);
    } finally {
      unlisten?.();
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
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
          className="flex items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2">
            <ArrowDownToLine className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <h3 className="text-[16px] font-semibold">{t("updateModalTitle")}</h3>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg p-1.5 hover:bg-black/10"
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

          {installing && (
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs" style={{ color: "var(--text-secondary)" }}>
                <span>{stageText}</span>
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

          {(errorText || installError) && (
            <div
              className="rounded-xl px-3 py-2 text-[13px]"
              style={{
                background: "var(--danger-bg)",
                color: "var(--danger-text)",
              }}
            >
              {installError || errorText}
            </div>
          )}

          <div className="flex justify-end gap-2 pt-1">
            <button
              type="button"
              disabled={checking || installing}
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
                disabled={installing}
                onClick={() => void handleInstall()}
                className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-[14px] font-semibold disabled:opacity-60"
              >
                <ArrowDownToLine className="h-4 w-4" />
                <span>{installing ? t("updateInstallingBtn") : t("updateInstallRestartBtn")}</span>
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
