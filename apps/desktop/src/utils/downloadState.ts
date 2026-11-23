import type { DownloadProgressDto } from "../types.ts";

export interface DownloadProgressItem {
  id: string;
  fileName: string;
  stage: "downloading" | "saving";
  downloadedBytes: number;
  totalBytes: number | null;
}

export function formatDownloadBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${Math.round(bytes)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function computeDownloadPercent(downloaded: number, total: number | null): number | null {
  if (total === null || !Number.isFinite(total) || total <= 0) {
    return null;
  }
  const safeDownloaded = Math.max(0, Number.isFinite(downloaded) ? downloaded : 0);
  const pct = Math.round((safeDownloaded / total) * 100);
  return Math.max(0, Math.min(100, pct));
}

export function applyDownloadProgress(
  list: DownloadProgressItem[],
  ev: DownloadProgressDto,
): DownloadProgressItem[] {
  if (ev.stage === "done" || ev.stage === "cancelled" || ev.stage === "error") {
    return list.filter((item) => item.id !== ev.id);
  }
  const nextItem: DownloadProgressItem = {
    id: ev.id,
    fileName: ev.file_name || "download",
    stage: ev.stage === "saving" ? "saving" : "downloading",
    downloadedBytes: Math.max(0, ev.downloaded_bytes),
    totalBytes: ev.total_bytes,
  };
  const existingIdx = list.findIndex((item) => item.id === ev.id);
  if (existingIdx === -1) {
    return [...list, nextItem];
  }
  return list.map((item, idx) => (idx === existingIdx ? nextItem : item));
}

export function shouldFocusComposerOnReply(
  prevReplyId: string | null,
  nextReplyId: string | null,
  prevFocusSignal: number,
  nextFocusSignal: number,
): boolean {
  if (nextFocusSignal > prevFocusSignal) {
    return true;
  }
  return nextReplyId !== null && nextReplyId !== prevReplyId;
}
