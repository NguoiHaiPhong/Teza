import type { MessageDto, PendingAttachment } from "../types.ts";

export const MAX_STAGED_ATTACHMENTS = 20;

export interface OutboxAttachmentPayload {
  att: PendingAttachment;
  caption: string;
  ownedPreviewUrl: string | null;
  rootMsgId?: string;
}

export interface OutboxEntry {
  msg: MessageDto;
  quoteTarget?: MessageDto | null;
  attachment?: OutboxAttachmentPayload;
}

export type OutboxByThread = Record<string, OutboxEntry[]>;

export interface ThreadOutboxSummary {
  failedCount: number;
  sendingCount: number;
  latestFailed: MessageDto | null;
  latestSending: MessageDto | null;
}

export interface StageAttachmentsResult {
  next: PendingAttachment[];
  addedCount: number;
  duplicateCount: number;
  overflowCount: number;
  rejectedUrls: string[];
}

export interface AttachmentSendItem {
  att: PendingAttachment;
  caption: string;
}

export function isOutboxTempId(msgId: string): boolean {
  return msgId.startsWith("opt-");
}

export function isRevocableBlobUrl(url: string | null | undefined): url is string {
  return typeof url === "string" && url.startsWith("blob:");
}

export function normalizeAttachmentPathKey(rawPath: string): string {
  const trimmed = rawPath.trim();
  if (/^[a-zA-Z]:[\\/]/.test(trimmed) || trimmed.startsWith("\\\\")) {
    return trimmed.replace(/\\/g, "/").toLowerCase();
  }
  return trimmed;
}

export function upsertOutboxEntry(
  outbox: OutboxByThread,
  entry: OutboxEntry,
): OutboxByThread {
  const threadId = entry.msg.thread_id;
  if (!threadId) return outbox;
  const existing = outbox[threadId] ?? [];
  const idx = existing.findIndex((item) => item.msg.msg_id === entry.msg.msg_id);
  const nextList =
    idx >= 0
      ? existing.map((item, i) => (i === idx ? entry : item))
      : [...existing, entry];
  nextList.sort((a, b) => a.msg.ts - b.msg.ts);
  return {
    ...outbox,
    [threadId]: nextList,
  };
}

export function findOutboxEntry(
  outbox: OutboxByThread,
  tempId: string,
  threadId?: string,
): OutboxEntry | null {
  if (threadId) {
    const list = outbox[threadId];
    return list?.find((item) => item.msg.msg_id === tempId) ?? null;
  }
  for (const list of Object.values(outbox)) {
    const hit = list.find((item) => item.msg.msg_id === tempId);
    if (hit) return hit;
  }
  return null;
}

export function markOutboxFailed(
  outbox: OutboxByThread,
  threadId: string,
  tempId: string,
  errorMsg: string,
): { next: OutboxByThread; updated: OutboxEntry | null } {
  const list = outbox[threadId];
  if (!list || list.length === 0) {
    return { next: outbox, updated: null };
  }
  const idx = list.findIndex((item) => item.msg.msg_id === tempId);
  if (idx < 0) {
    return { next: outbox, updated: null };
  }
  const prevEntry = list[idx];
  const updated: OutboxEntry = {
    ...prevEntry,
    msg: {
      ...prevEntry.msg,
      send_status: "failed",
      send_error: errorMsg,
    },
  };
  const nextList = list.map((item, i) => (i === idx ? updated : item));
  return {
    next: {
      ...outbox,
      [threadId]: nextList,
    },
    updated,
  };
}

export function removeOutboxEntry(
  outbox: OutboxByThread,
  tempId: string,
  threadId?: string,
): { next: OutboxByThread; removed: OutboxEntry | null } {
  const candidateThreads = threadId ? [threadId] : Object.keys(outbox);
  for (const tid of candidateThreads) {
    const list = outbox[tid];
    if (!list || list.length === 0) continue;
    const idx = list.findIndex((item) => item.msg.msg_id === tempId);
    if (idx < 0) continue;
    const removed = list[idx];
    const remaining = [...list.slice(0, idx), ...list.slice(idx + 1)];
    const next: OutboxByThread = { ...outbox };
    if (remaining.length === 0) {
      delete next[tid];
    } else {
      next[tid] = remaining;
    }
    return { next, removed };
  }
  return { next: outbox, removed: null };
}

export function removeOutboxEntriesForThread(
  outbox: OutboxByThread,
  threadId: string,
): { next: OutboxByThread; removed: OutboxEntry[] } {
  const existing = outbox[threadId];
  if (!existing || existing.length === 0) {
    return { next: outbox, removed: [] };
  }
  const next: OutboxByThread = { ...outbox };
  delete next[threadId];
  return { next, removed: existing };
}

export function collectAllOutboxEntries(outbox: OutboxByThread): OutboxEntry[] {
  const all: OutboxEntry[] = [];
  for (const list of Object.values(outbox)) {
    for (const entry of list) {
      all.push(entry);
    }
  }
  return all;
}

export function getOutboxMessages(
  outbox: OutboxByThread,
  threadId: string,
): MessageDto[] {
  const list = outbox[threadId];
  if (!list || list.length === 0) return [];
  return list.map((entry) => entry.msg);
}

function isLikelyReconciledSendingEntry(
  entry: OutboxEntry,
  candidate: MessageDto,
): boolean {
  if (entry.msg.send_status !== "sending") return false;
  if (!candidate.is_outgoing) return false;
  if (isOutboxTempId(candidate.msg_id)) return false;
  if (candidate.thread_id !== entry.msg.thread_id) return false;
  // Allow modest clock skew while preventing an earlier identical message from hiding a new send.
  if (candidate.ts < entry.msg.ts - 5_000 || candidate.ts > entry.msg.ts + 30_000) {
    return false;
  }

  const entryAttName = (entry.msg.attachment_name || "").trim();
  const candAttName = (candidate.attachment_name || "").trim();
  if (entry.attachment || entryAttName) {
    if (entryAttName && candAttName && entryAttName === candAttName) {
      return true;
    }
    const entryType = entry.msg.msg_type.toLowerCase();
    const candType = candidate.msg_type.toLowerCase();
    const bothPhoto =
      (entryType.includes("photo") || entryType === "2") &&
      (candType.includes("photo") || candType === "2");
    if (bothPhoto && candidate.text.trim() === entry.msg.text.trim()) {
      return true;
    }
    return false;
  }

  return candidate.text.trim() === entry.msg.text.trim();
}

export function mergeOutboxIntoMessages(
  threadId: string,
  baseMessages: MessageDto[],
  outbox: OutboxByThread,
): MessageDto[] {
  const entries = outbox[threadId];
  if (!entries || entries.length === 0) {
    return baseMessages;
  }

  const merged = [...baseMessages];
  const usedRealIds = new Set<string>();
  let changed = false;

  for (const entry of entries) {
    const exactIdx = merged.findIndex((m) => m.msg_id === entry.msg.msg_id);

    // Never drop failed entries based on heuristic content matching — only reconcile in-flight
    // sending entries that already have an unmatched real server echo in baseMessages (including
    // when both the optimistic sending bubble and the WS server echo are present).
    if (entry.msg.send_status === "sending") {
      const matchedReal = merged.find(
        (m) => !usedRealIds.has(m.msg_id) && isLikelyReconciledSendingEntry(entry, m),
      );
      if (matchedReal) {
        usedRealIds.add(matchedReal.msg_id);
        if (exactIdx >= 0) {
          merged.splice(exactIdx, 1);
          changed = true;
        }
        continue;
      }
    }

    if (exactIdx >= 0) {
      if (merged[exactIdx] !== entry.msg) {
        merged[exactIdx] = entry.msg;
        changed = true;
      }
      continue;
    }

    merged.push(entry.msg);
    changed = true;
  }

  if (!changed) {
    return baseMessages;
  }

  return merged.sort((a, b) => a.ts - b.ts);
}

export function summarizeThreadOutbox(
  outbox: OutboxByThread,
  threadId: string,
): ThreadOutboxSummary {
  const entries = outbox[threadId];
  if (!entries || entries.length === 0) {
    return {
      failedCount: 0,
      sendingCount: 0,
      latestFailed: null,
      latestSending: null,
    };
  }
  let failedCount = 0;
  let sendingCount = 0;
  let latestFailed: MessageDto | null = null;
  let latestSending: MessageDto | null = null;

  for (const entry of entries) {
    if (entry.msg.send_status === "failed") {
      failedCount += 1;
      if (!latestFailed || entry.msg.ts >= latestFailed.ts) {
        latestFailed = entry.msg;
      }
    } else if (entry.msg.send_status === "sending") {
      sendingCount += 1;
      if (!latestSending || entry.msg.ts >= latestSending.ts) {
        latestSending = entry.msg;
      }
    }
  }

  return {
    failedCount,
    sendingCount,
    latestFailed,
    latestSending,
  };
}

export function appendStagedAttachments(
  existing: PendingAttachment[],
  incoming: PendingAttachment[],
  maxItems = MAX_STAGED_ATTACHMENTS,
): StageAttachmentsResult {
  if (incoming.length === 0) {
    return {
      next: existing,
      addedCount: 0,
      duplicateCount: 0,
      overflowCount: 0,
      rejectedUrls: [],
    };
  }

  const safeMax = Math.max(1, maxItems);
  const seenPaths = new Set<string>();
  for (const item of existing) {
    if (item.kind === "path") {
      seenPaths.add(normalizeAttachmentPathKey(item.path));
    }
  }

  const next = [...existing];
  let addedCount = 0;
  let duplicateCount = 0;
  let overflowCount = 0;
  const rejectedUrls: string[] = [];

  for (const item of incoming) {
    if (item.kind === "path") {
      const key = normalizeAttachmentPathKey(item.path);
      if (seenPaths.has(key)) {
        duplicateCount += 1;
        continue;
      }
      if (next.length >= safeMax) {
        overflowCount += 1;
        continue;
      }
      seenPaths.add(key);
      next.push(item);
      addedCount += 1;
    } else {
      if (next.length >= safeMax) {
        overflowCount += 1;
        if (isRevocableBlobUrl(item.previewUrl)) {
          rejectedUrls.push(item.previewUrl);
        }
        continue;
      }
      next.push(item);
      addedCount += 1;
    }
  }

  return {
    next: addedCount > 0 ? next : existing,
    addedCount,
    duplicateCount,
    overflowCount,
    rejectedUrls,
  };
}

export function removeStagedAttachmentAt(
  existing: PendingAttachment[],
  index: number,
): { next: PendingAttachment[]; removed: PendingAttachment | null; revokedUrls: string[] } {
  if (index < 0 || index >= existing.length) {
    return { next: existing, removed: null, revokedUrls: [] };
  }
  const removed = existing[index];
  const next = [...existing.slice(0, index), ...existing.slice(index + 1)];
  const remainingUrls = new Set(collectRevocableAttachmentUrls(next));
  const revokedUrls =
    removed.kind === "raw" &&
    isRevocableBlobUrl(removed.previewUrl) &&
    !remainingUrls.has(removed.previewUrl)
      ? [removed.previewUrl]
      : [];
  return { next, removed, revokedUrls };
}

export function collectRevocableAttachmentUrls(items: PendingAttachment[]): string[] {
  const urls: string[] = [];
  const seen = new Set<string>();
  for (const item of items) {
    if (item.kind === "raw" && isRevocableBlobUrl(item.previewUrl) && !seen.has(item.previewUrl)) {
      seen.add(item.previewUrl);
      urls.push(item.previewUrl);
    }
  }
  return urls;
}

export function diffRevocableAttachmentUrls(
  prev: PendingAttachment[],
  next: PendingAttachment[],
): string[] {
  const kept = new Set(collectRevocableAttachmentUrls(next));
  return collectRevocableAttachmentUrls(prev).filter((url) => !kept.has(url));
}

export function buildAttachmentSendBatch(
  items: PendingAttachment[],
  caption: string,
): AttachmentSendItem[] {
  return items.map((att, idx) => ({
    att,
    caption: idx === 0 ? caption : "",
  }));
}
