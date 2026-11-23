import { useEffect, useState } from "react";
import { AlertTriangle, CheckCircle2, Download, ExternalLink, FileText, FolderOpen, X } from "lucide-react";
import { useI18n } from "../i18n";
import type { SavedFileResultDto } from "../types";
import {
  computeDownloadPercent,
  formatDownloadBytes,
  type DownloadProgressItem,
} from "../utils/downloadState";

interface Props {
  activeDownloads: DownloadProgressItem[];
  completedDownload: SavedFileResultDto | null;
  onDismissCompleted: () => void;
  onOpenFile: (path: string) => Promise<void>;
  onRevealFile: (path: string) => Promise<void>;
  onError: (err: unknown) => void;
}

export function DownloadOverlay({
  activeDownloads,
  completedDownload,
  onDismissCompleted,
  onOpenFile,
  onRevealFile,
  onError,
}: Props) {
  const { t } = useI18n();
  const [busyAction, setBusyAction] = useState<"open" | "reveal" | null>(null);

  useEffect(() => {
    setBusyAction(null);
  }, [completedDownload?.path]);

  useEffect(() => {
    if (!completedDownload) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onDismissCompleted();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [completedDownload, onDismissCompleted]);

  const handleOpen = async () => {
    if (!completedDownload || busyAction) return;
    setBusyAction("open");
    try {
      await onOpenFile(completedDownload.path);
      onDismissCompleted();
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  };

  const handleReveal = async () => {
    if (!completedDownload || busyAction) return;
    setBusyAction("reveal");
    try {
      await onRevealFile(completedDownload.path);
      onDismissCompleted();
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  };

  return (
    <>
      {activeDownloads.length > 0 && (
        <div
          aria-live="polite"
          className="pointer-events-auto fixed bottom-4 right-4 z-[65] flex w-80 max-w-[calc(100vw-2rem)] flex-col gap-2"
        >
          {activeDownloads.map((item) => {
            const pct = computeDownloadPercent(item.downloadedBytes, item.totalBytes);
            const isSaving = item.stage === "saving";
            const statusText = isSaving
              ? t("downloadChoosingLocation")
              : pct !== null
                ? `${pct}% · ${formatDownloadBytes(item.downloadedBytes)} / ${formatDownloadBytes(item.totalBytes ?? 0)}`
                : item.downloadedBytes > 0
                  ? `${t("downloadInProgress")} (${formatDownloadBytes(item.downloadedBytes)})`
                  : t("downloadInProgress");

            return (
              <div
                key={item.id}
                className="rounded-xl px-3.5 py-2.5 shadow-2xl backdrop-blur-md"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                <div className="flex items-center gap-2">
                  <Download
                    className="h-4 w-4 shrink-0"
                    style={{ color: "var(--accent)" }}
                  />
                  <div className="min-w-0 flex-1">
                    <div
                      className="truncate text-[12.5px] font-medium leading-tight"
                      title={item.fileName}
                    >
                      {item.fileName}
                    </div>
                    <div
                      className="mt-0.5 truncate text-[11px] tabular-nums"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {statusText}
                    </div>
                  </div>
                </div>
                <div
                  className="mt-2 h-1 w-full overflow-hidden rounded-full"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {isSaving || pct !== null ? (
                    <div
                      className="h-full rounded-full transition-[width] duration-150"
                      style={{
                        width: `${isSaving ? 100 : (pct ?? 0)}%`,
                        background: "var(--accent)",
                      }}
                    />
                  ) : (
                    <div
                      className="teza-progress-indeterminate h-full w-1/3 rounded-full"
                      style={{ background: "var(--accent)" }}
                    />
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {completedDownload && (
        <div
          className="fixed inset-0 z-[72] flex items-center justify-center bg-black/45 p-4"
          onClick={onDismissCompleted}
        >
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="download-complete-title"
            className="w-full max-w-md overflow-hidden rounded-2xl shadow-2xl"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div
              className="flex items-center justify-between px-4 py-3"
              style={{ borderBottom: "1px solid var(--border-subtle)" }}
            >
              <div className="flex items-center gap-2">
                <CheckCircle2
                  className="h-4.5 w-4.5 shrink-0"
                  style={{ color: "var(--accent)" }}
                />
                <h3
                  id="download-complete-title"
                  className="text-[14.5px] font-semibold"
                >
                  {t("downloadCompleteTitle")}
                </h3>
              </div>
              <button
                type="button"
                onClick={onDismissCompleted}
                title={t("close")}
                className="hover-menu rounded-lg p-1 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <div className="space-y-3 p-4">
              <div
                className="flex items-start gap-2.5 rounded-xl px-3 py-2.5"
                style={{ background: "var(--bg-surface)" }}
              >
                <FileText
                  className="mt-0.5 h-5 w-5 shrink-0"
                  style={{ color: "var(--accent)" }}
                />
                <div className="min-w-0 flex-1">
                  <div
                    className="truncate text-[13.5px] font-medium"
                    title={completedDownload.file_name}
                  >
                    {completedDownload.file_name}
                  </div>
                  <div
                    className="mt-0.5 truncate text-[11.5px]"
                    style={{ color: "var(--text-secondary)" }}
                    title={completedDownload.path}
                  >
                    {completedDownload.path}
                  </div>
                </div>
              </div>

              <p
                className="text-[13px]"
                style={{ color: "var(--text-secondary)" }}
              >
                {t("downloadCompleteQuestion")}
              </p>

              {!completedDownload.can_open_directly && (
                <div
                  className="flex items-start gap-2 rounded-xl px-3 py-2 text-[12px]"
                  style={{
                    color: "var(--text-primary)",
                    background: "rgba(245, 158, 11, 0.14)",
                    border: "1px solid rgba(245, 158, 11, 0.35)",
                  }}
                >
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
                  <span>{t("downloadExecutableBlocked")}</span>
                </div>
              )}

              <div className="flex flex-wrap items-center justify-end gap-2 pt-1">
                <button
                  type="button"
                  onClick={onDismissCompleted}
                  disabled={busyAction !== null}
                  className="hover-surface rounded-xl px-3 py-1.5 text-[13px] font-medium transition disabled:opacity-50"
                  style={{
                    background: "var(--bg-surface)",
                    color: "var(--text-secondary)",
                  }}
                >
                  {t("close")}
                </button>
                <button
                  type="button"
                  onClick={() => void handleReveal()}
                  disabled={busyAction !== null}
                  className="hover-surface inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-[13px] font-medium transition disabled:opacity-50"
                  style={{
                    background: "var(--bg-surface)",
                    color: "var(--text-primary)",
                  }}
                >
                  <FolderOpen className="h-3.5 w-3.5" />
                  {t("downloadRevealFolder")}
                </button>
                {completedDownload.can_open_directly && (
                  <button
                    type="button"
                    onClick={() => void handleOpen()}
                    disabled={busyAction !== null}
                    className="inline-flex items-center gap-1.5 rounded-xl px-3.5 py-1.5 text-[13px] font-medium transition hover:brightness-110 disabled:opacity-50"
                    style={{
                      background: "var(--accent)",
                      color: "var(--text-on-accent)",
                    }}
                  >
                    <ExternalLink className="h-3.5 w-3.5" />
                    {t("downloadOpenFile")}
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
