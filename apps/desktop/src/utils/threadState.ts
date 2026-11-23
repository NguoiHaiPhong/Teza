import type { ChatFolderDto, MessageDto, ThreadDto } from "../types.ts";
import type { UiLanguage } from "../i18n/types.ts";
import { uiLanguageToLocale } from "../i18n/types.ts";

export function dayKey(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

export function formatDayLabel(ts: number, lang: UiLanguage = "vi"): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  const today = new Date();
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(d.getTime()) === dayKey(today.getTime())) {
    return lang === "en" ? "Today" : lang === "ko" ? "오늘" : "Hôm nay";
  }
  if (dayKey(d.getTime()) === dayKey(yest.getTime())) {
    return lang === "en" ? "Yesterday" : lang === "ko" ? "어제" : "Hôm qua";
  }
  return d.toLocaleDateString(uiLanguageToLocale(lang));
}

export function formatThreadTime(ts: number, lang: UiLanguage = "vi"): string {
  if (!ts) return "";
  const ms = ts > 1e12 ? ts : ts * 1000;
  const d = new Date(ms);
  const now = new Date();
  const locale = uiLanguageToLocale(lang);
  if (dayKey(ms) === dayKey(now.getTime())) {
    return d.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
  }
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(ms) === dayKey(yest.getTime())) {
    return lang === "en" ? "Yesterday" : lang === "ko" ? "어제" : "Hôm qua";
  }
  return d.toLocaleDateString(locale, { day: "2-digit", month: "2-digit" });
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

  if (!isPhoto && !isVideo && !isVoice && !isSticker && (isFile || hasAtt)) {
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
  if (selection === "archived") {
    return threads.filter((t) => Boolean(t.is_archived));
  }
  const active = threads.filter((t) => !t.is_archived);
  if (selection === "all") return active;
  if (selection === "unread") return active.filter((t) => t.unread_count > 0);
  if (selection === "direct" || selection === "group" || selection === "cloud") {
    return active.filter((t) => t.kind === selection);
  }
  if (!selection.startsWith("custom:")) return active;

  const folderId = selection.slice("custom:".length);
  const folder = customFolders.find((f) => f.id === folderId);
  if (!folder) return active;

  const hasExplicitSet = folder.thread_ids.length > 0;
  const hasKindRule = folder.include_kinds.length > 0;
  const idSet = hasExplicitSet ? new Set(folder.thread_ids) : null;
  const excludedSet = folder.excluded_thread_ids?.length
    ? new Set(folder.excluded_thread_ids)
    : null;

  return active.filter((t) => {
    if (excludedSet?.has(t.id)) return false;
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

export function moveArrayItem<T>(items: T[], fromIdx: number, toIdx: number): T[] {
  if (
    fromIdx < 0 ||
    toIdx < 0 ||
    fromIdx >= items.length ||
    toIdx >= items.length ||
    fromIdx === toIdx
  ) {
    return items;
  }
  const next = [...items];
  const [moved] = next.splice(fromIdx, 1);
  next.splice(toIdx, 0, moved);
  return next;
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
    last_read_in_ts: !incUnread
      ? Math.max(target.last_read_in_ts ?? 0, msg.ts)
      : (target.last_read_in_ts ?? 0),
    peer_read_ts: !msg.is_outgoing
      ? Math.max(target.peer_read_ts ?? 0, msg.ts)
      : (target.peer_read_ts ?? 0),
    unread_count: msg.is_outgoing
      ? 0
      : incUnread
        ? target.unread_count + 1
        : target.unread_count,
  };
  return next.sort(compareThreads);
}

export function findFirstUnreadMsgId(
  messages: MessageDto[],
  lastReadInTs: number,
  fallbackUnreadCount: number,
): string | null {
  if (fallbackUnreadCount <= 0 || messages.length === 0) return null;
  let lastOutgoingTs = 0;
  for (const m of messages) {
    if (m.is_outgoing && m.ts > lastOutgoingTs) {
      lastOutgoingTs = m.ts;
    }
  }
  const effectiveWatermark = Math.max(lastReadInTs, lastOutgoingTs);
  const incomingAfter =
    effectiveWatermark > 0
      ? messages.filter((m) => !m.is_outgoing && m.ts > effectiveWatermark)
      : messages.filter((m) => !m.is_outgoing);
  if (incomingAfter.length === 0) return null;

  if (lastReadInTs > 0) {
    const firstNew = incomingAfter[0];
    const latestTs = messages[messages.length - 1].ts;
    const firstIdx = messages.indexOf(firstNew);
    const staleByCount =
      incomingAfter.length > Math.max(fallbackUnreadCount + 5, fallbackUnreadCount * 2);
    const staleByAgeOrTop =
      latestTs - firstNew.ts > 6 * 3_600_000 || (messages.length >= 25 && firstIdx < 3);
    if (staleByCount && staleByAgeOrTop) {
      return incomingAfter[Math.max(0, incomingAfter.length - fallbackUnreadCount)].msg_id;
    }
    return firstNew.msg_id;
  }

  return incomingAfter[Math.max(0, incomingAfter.length - fallbackUnreadCount)].msg_id;
}
