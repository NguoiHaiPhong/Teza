import type { ChatFolderDto, MessageDto, ThreadDto } from "../types.ts";

export function dayKey(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function formatDayLabel(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  const today = new Date();
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(d.getTime()) === dayKey(today.getTime())) return "Hôm nay";
  if (dayKey(d.getTime()) === dayKey(yest.getTime())) return "Hôm qua";
  return d.toLocaleDateString("vi-VN");
}

export function formatThreadTime(ts: number): string {
  if (!ts) return "";
  const ms = ts > 1e12 ? ts : ts * 1000;
  const d = new Date(ms);
  const now = new Date();
  if (dayKey(ms) === dayKey(now.getTime())) {
    return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
  }
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(ms) === dayKey(yest.getTime())) {
    return "Hôm qua";
  }
  return d.toLocaleDateString("vi-VN", { day: "2-digit", month: "2-digit" });
}

export function upsertMsg(list: MessageDto[], incoming: MessageDto): MessageDto[] {
  const idx = list.findIndex((m) => m.msg_id === incoming.msg_id);
  if (idx >= 0) {
    const next = [...list];
    next[idx] = incoming;
    return next;
  }
  let base = list;
  if (incoming.is_outgoing && !incoming.msg_id.startsWith("opt-")) {
    const incomingText = incoming.text.trim();
    const optIdx = base.findIndex(
      (m) =>
        m.is_outgoing &&
        m.msg_id.startsWith("opt-") &&
        m.thread_id === incoming.thread_id &&
        m.text.trim() === incomingText &&
        Math.abs(incoming.ts - m.ts) < 30_000,
    );
    if (optIdx >= 0) {
      base = [...base.slice(0, optIdx), ...base.slice(optIdx + 1)];
    }
  }
  return [...base, incoming].sort((a, b) => a.ts - b.ts);
}

export function compareThreads(a: ThreadDto, b: ThreadDto): number {
  const aPinned = a.is_pinned ? 1 : 0;
  const bPinned = b.is_pinned ? 1 : 0;
  if (bPinned !== aPinned) return bPinned - aPinned;
  if (b.last_ts !== a.last_ts) return b.last_ts - a.last_ts;
  return a.id.localeCompare(b.id);
}

export function getDisplayBodyText(
  msg: Pick<MessageDto, "msg_type" | "text" | "attachment_url" | "attachment_name">,
): string {
  const raw = (msg.text || "").trim();
  if (!raw) return "";

  const t = (msg.msg_type || "").toLowerCase();
  const hasAtt = Boolean(msg.attachment_url || msg.attachment_name);
  const isSticker = t.includes("sticker") || t === "5" || t === "35" || t === "36";
  if (isSticker && raw === "[Nhãn dán]") return "";

  const isPhoto = t.includes("photo") || t === "2" || t.includes("gif");
  const isVideo = t.includes("video") || t === "4" || t === "44";
  const isVoice = t.includes("voice") || t === "31";
  const isFile = t.includes("file") || t === "19" || t === "share.file";

  if (hasAtt && isPhoto && raw === "[Hình ảnh]") return "";
  if (hasAtt && isVideo && raw === "[Video]") return "";
  if (hasAtt && isVoice && raw === "[Tin nhắn thoại]") return "";

  if (isFile || hasAtt) {
    if (raw === "[Tập tin]") return "";
    const fileName = (msg.attachment_name || "").trim();
    const rawLower = raw.toLowerCase();
    if (fileName) {
      const fnLower = fileName.toLowerCase();
      if (rawLower === fnLower || rawLower === `[tập tin] ${fnLower}`) {
        return "";
      }
      const nlIdx = raw.indexOf("\n");
      if (nlIdx > 0) {
        const firstLine = raw.slice(0, nlIdx).trim().toLowerCase();
        if (firstLine === fnLower || firstLine === `[tập tin] ${fnLower}`) {
          return raw.slice(nlIdx + 1).trim();
        }
      }
    } else if (isFile && hasAtt && /^\[tập tin\]\s+[^\n]+$/i.test(raw)) {
      return "";
    }
  }

  return raw;
}

export type TextLinkPart = { text: string; url?: string };

export function splitTextLinks(input: string): TextLinkPart[] {
  if (!input) return [];
  const regex = /https?:\/\/[^\s<>"']+/gi;
  const parts: TextLinkPart[] = [];
  let lastIdx = 0;
  for (const match of input.matchAll(regex)) {
    const start = match.index ?? 0;
    let url = match[0];
    while (url.length > 0 && /[.,;:!?\]}]/.test(url[url.length - 1])) {
      url = url.slice(0, -1);
    }
    while (url.endsWith(")")) {
      const openCount = (url.match(/\(/g) || []).length;
      const closeCount = (url.match(/\)/g) || []).length;
      if (closeCount > openCount) {
        url = url.slice(0, -1);
      } else {
        break;
      }
    }
    if (start > lastIdx) {
      parts.push({ text: input.slice(lastIdx, start) });
    }
    if (url) {
      parts.push({ text: url, url });
      lastIdx = start + url.length;
    } else {
      parts.push({ text: match[0] });
      lastIdx = start + match[0].length;
    }
  }
  if (lastIdx < input.length) {
    parts.push({ text: input.slice(lastIdx) });
  }
  return parts;
}

export type LayoutMode = "single" | "compact" | "full";

export function computeLayoutMode(
  width: number,
  manualCompact: boolean,
): { mode: LayoutMode; autoCompactDrawer: boolean } {
  if (width < 640) {
    return { mode: "single", autoCompactDrawer: true };
  }
  if (width < 900 || manualCompact) {
    return { mode: "compact", autoCompactDrawer: width < 1160 };
  }
  return { mode: "full", autoCompactDrawer: width < 1160 };
}

export function looksLikePhoneQuery(query: string): boolean {
  const cleaned = query.trim().replace(/[\s\-().\t]/g, "");
  return /^(?:\+?84\d{8,10}|0\d{8,10}|[1-9]\d{8})$/.test(cleaned);
}

export function extractFirstUrl(text: string): string | null {
  if (!text) return null;
  const hit = splitTextLinks(text).find((p) => Boolean(p.url));
  return hit?.url ?? null;
}

export function filterThreadsByFolder(
  threads: ThreadDto[],
  selection: string,
  customFolders: ChatFolderDto[],
): ThreadDto[] {
  if (selection === "all") return threads;
  if (selection === "unread") return threads.filter((t) => t.unread_count > 0);
  if (selection === "direct" || selection === "group" || selection === "cloud") {
    return threads.filter((t) => t.kind === selection);
  }
  if (!selection.startsWith("custom:")) return threads;

  const folderId = selection.slice("custom:".length);
  const folder = customFolders.find((f) => f.id === folderId);
  if (!folder) return threads;

  const hasExplicitSet = folder.thread_ids.length > 0;
  const hasKindRule = folder.include_kinds.length > 0;
  const idSet = hasExplicitSet ? new Set(folder.thread_ids) : null;

  return threads.filter((t) => {
    if (folder.exclude_muted && t.is_muted) return false;
    if (folder.only_unread && t.unread_count <= 0) return false;
    if (hasExplicitSet || hasKindRule) {
      const inIds = idSet ? idSet.has(t.id) : false;
      const inKind = hasKindRule ? folder.include_kinds.includes(t.kind) : false;
      return inIds || inKind;
    }
    return folder.only_unread || folder.exclude_muted;
  });
}

export function countFolderUnread(
  threads: ThreadDto[],
  selection: string,
  customFolders: ChatFolderDto[],
): number {
  const matched = filterThreadsByFolder(threads, selection, customFolders);
  return matched.reduce((acc, t) => {
    if (selection === "unread") {
      return acc + t.unread_count;
    }
    return acc + (t.is_muted ? 0 : t.unread_count);
  }, 0);
}

export function bumpThreadList(list: ThreadDto[], msg: MessageDto, incUnread: boolean): ThreadDto[] {
  const rawPreview = (
    msg.text || (msg.attachment_name ? `[Tệp: ${msg.attachment_name}]` : `[${msg.msg_type}]`)
  )
    .replace(/\s+/g, " ")
    .trim();
  const idx = list.findIndex((t) => t.id === msg.thread_id);
  if (idx < 0) return list;
  const target = list[idx];
  let preview = rawPreview;
  if (rawPreview) {
    if (msg.is_outgoing) {
      preview = rawPreview.startsWith("Bạn:") ? rawPreview : `Bạn: ${rawPreview}`;
    } else if (
      target.kind === "group" &&
      msg.sender_name &&
      msg.sender_name !== "Bạn" &&
      !/^\d{6,}$/.test(msg.sender_name.trim())
    ) {
      preview = `${msg.sender_name}: ${rawPreview}`;
    }
  }
  const next = [...list];
  next[idx] = {
    ...target,
    last_preview: msg.ts >= target.last_ts ? preview : target.last_preview,
    last_ts: Math.max(target.last_ts, msg.ts),
    unread_count: msg.is_outgoing
      ? 0
      : incUnread
        ? target.unread_count + 1
        : target.unread_count,
  };
  return next.sort(compareThreads);
}
