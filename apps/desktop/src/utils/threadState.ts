import type { ChatFolderDto, MessageDto, PollDto, ReactionSummary, ThreadDto } from "../types.ts";
import type { UiLanguage } from "../i18n/types.ts";
import { uiLanguageToLocale } from "../i18n/types.ts";
import { POLL_PREFIX, parsePollPayload } from "./richMessage.ts";
import { looksLikeInternationalPhoneQuery } from "./searchNormalize.ts";

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

export function isOptimisticPollMsgId(msgId: string): boolean {
  return msgId.startsWith("poll-") || msgId.startsWith("local-poll-");
}

export function isPollVoteActivityQuestion(raw: string): boolean {
  const q = raw.trim();
  if (!q) return true;
  const low = q.toLowerCase();
  if (
    low === "bình chọn" ||
    low === "[bình chọn]" ||
    low === "poll" ||
    low === "[poll]" ||
    low === "투표"
  ) {
    return true;
  }
  if (q.endsWith("?")) return false;
  return (
    /(?:^|\s)đã\s+(?:tham\s+gia\s+bình\s+chọn|bình\s+chọn|bỏ\s+phiếu|khóa\s+bình\s+chọn|kết\s+thúc\s+bình\s+chọn|tạo\s+bình\s+chọn|cập\s+nhật\s+bình\s+chọn|thêm\s+(?:lựa\s+chọn|phương\s+án))/i.test(
      low,
    ) ||
    /^(?:.*?\s+)?(?:voted(?:\s+(?:in|on|for)\b.*)?|closed\s+the\s+poll|locked\s+the\s+poll|created\s+a\s+poll|added\s+an?\s+option\b.*)[.!]?$/i.test(
      low,
    )
  );
}

export function mergePollDto(existing: PollDto, incoming: PollDto): PollDto {
  const incomingHasOptions = incoming.options.length > 0;
  const existingHasOptions = existing.options.length > 0;
  const incomingQ = (incoming.question || "").trim();
  const existingQ = (existing.question || "").trim();
  const incomingHasRealQuestion = Boolean(incomingQ && !isPollVoteActivityQuestion(incomingQ));
  const existingHasRealQuestion = Boolean(existingQ && !isPollVoteActivityQuestion(existingQ));
  const isPartial =
    (!incomingHasRealQuestion && existingHasRealQuestion) ||
    (!incomingHasOptions && existingHasOptions);
  const question = incomingHasRealQuestion
    ? incomingQ
    : existingHasRealQuestion
      ? existingQ
      : incomingQ || existingQ || "Bình chọn";
  return {
    poll_id: incoming.poll_id > 0 ? incoming.poll_id : existing.poll_id,
    question,
    options: incomingHasOptions ? incoming.options : existing.options,
    allow_multi: isPartial
      ? existing.allow_multi || incoming.allow_multi
      : incoming.allow_multi,
    allow_add_option: isPartial
      ? existing.allow_add_option || incoming.allow_add_option
      : incoming.allow_add_option,
    is_anonymous: isPartial
      ? existing.is_anonymous || incoming.is_anonymous
      : incoming.is_anonymous,
    expired_time: incoming.expired_time > 0 ? incoming.expired_time : existing.expired_time,
    closed: existing.closed || incoming.closed,
  };
}

function withMergedPollPayload(target: MessageDto, merged: PollDto): MessageDto {
  return {
    ...target,
    msg_type: "chat.poll",
    attachment_url: null,
    attachment_name: `${POLL_PREFIX}${JSON.stringify(merged)}`,
  };
}

function extractPollIdForDedupe(m: MessageDto): { pollId: number; poll: PollDto | null } {
  const poll = parsePollPayload(m);
  if (poll && poll.poll_id > 0) {
    return { pollId: poll.poll_id, poll };
  }
  if (isOptimisticPollMsgId(m.msg_id)) {
    const match = /^(?:local-)?poll-(\d+)$/.exec(m.msg_id);
    if (match) {
      const parsed = Number.parseInt(match[1], 10);
      if (Number.isFinite(parsed) && parsed > 0) {
        return { pollId: parsed, poll };
      }
    }
  }
  return { pollId: 0, poll };
}

interface PollDedupeEntry {
  idx: number;
  msg: MessageDto;
  poll: PollDto | null;
  isOptimistic: boolean;
}

function dedupePollGroups(
  list: MessageDto[],
  filter?: { threadId?: string; pollId?: number },
): MessageDto[] {
  const groups = new Map<string, PollDedupeEntry[]>();
  for (let i = 0; i < list.length; i++) {
    const m = list[i];
    if (filter?.threadId !== undefined && m.thread_id !== filter.threadId) continue;
    const { pollId, poll } = extractPollIdForDedupe(m);
    if (pollId <= 0) continue;
    if (filter?.pollId !== undefined && pollId !== filter.pollId) continue;
    const key = `${m.thread_id}:${pollId}`;
    let entries = groups.get(key);
    if (!entries) {
      entries = [];
      groups.set(key, entries);
    }
    entries.push({
      idx: i,
      msg: m,
      poll,
      isOptimistic: isOptimisticPollMsgId(m.msg_id),
    });
  }

  let hasDuplicates = false;
  for (const entries of groups.values()) {
    if (entries.length > 1) {
      hasDuplicates = true;
      break;
    }
  }
  if (!hasDuplicates) return list;

  const droppedIndices = new Set<number>();
  const replacementByIdx = new Map<number, MessageDto>();

  for (const entries of groups.values()) {
    if (entries.length <= 1) continue;
    const optimistics = entries
      .filter((e) => e.isOptimistic)
      .sort((a, b) => a.msg.ts - b.msg.ts || a.idx - b.idx);
    const reals = entries
      .filter((e) => !e.isOptimistic)
      .sort((a, b) => a.msg.ts - b.msg.ts || a.idx - b.idx);

    const canonical = reals.length > 0 ? reals[0] : optimistics[0];
    let mergedPoll: PollDto | null = null;
    for (const entry of [...optimistics, ...reals]) {
      if (!entry.poll) continue;
      mergedPoll = mergedPoll ? mergePollDto(mergedPoll, entry.poll) : entry.poll;
    }

    for (const entry of entries) {
      if (entry.idx !== canonical.idx) {
        droppedIndices.add(entry.idx);
      }
    }
    replacementByIdx.set(
      canonical.idx,
      mergedPoll ? withMergedPollPayload(canonical.msg, mergedPoll) : canonical.msg,
    );
  }

  const next: MessageDto[] = [];
  for (let i = 0; i < list.length; i++) {
    if (droppedIndices.has(i)) continue;
    next.push(replacementByIdx.get(i) ?? list[i]);
  }
  return next;
}

export function dedupeOptimisticPollMessages(list: MessageDto[]): MessageDto[] {
  return dedupePollGroups(list);
}

export const dedupePollMessages = dedupeOptimisticPollMessages;

export function applyPollUpdateToMessages(
  list: MessageDto[],
  pollId: number,
  updated: PollDto,
  preferredMsgId?: string,
): MessageDto[] {
  if (pollId <= 0) return list;
  const normalizedUpdate: PollDto =
    updated.poll_id > 0 ? updated : { ...updated, poll_id: pollId };

  const groupsByThread = new Map<string, PollDedupeEntry[]>();
  for (let i = 0; i < list.length; i++) {
    const m = list[i];
    const { pollId: rowPollId, poll } = extractPollIdForDedupe(m);
    const matches =
      rowPollId === pollId || (Boolean(preferredMsgId) && m.msg_id === preferredMsgId);
    if (!matches) continue;
    let entries = groupsByThread.get(m.thread_id);
    if (!entries) {
      entries = [];
      groupsByThread.set(m.thread_id, entries);
    }
    entries.push({
      idx: i,
      msg: m,
      poll,
      isOptimistic: isOptimisticPollMsgId(m.msg_id),
    });
  }

  if (groupsByThread.size === 0) return list;

  const droppedIndices = new Set<number>();
  const replacementByIdx = new Map<number, MessageDto>();

  for (const entries of groupsByThread.values()) {
    const optimistics = entries
      .filter((e) => e.isOptimistic)
      .sort((a, b) => a.msg.ts - b.msg.ts || a.idx - b.idx);
    const reals = entries
      .filter((e) => !e.isOptimistic)
      .sort((a, b) => a.msg.ts - b.msg.ts || a.idx - b.idx);

    const canonical = reals.length > 0 ? reals[0] : optimistics[0];
    let mergedExisting: PollDto | null = null;
    for (const entry of [...optimistics, ...reals]) {
      if (!entry.poll) continue;
      mergedExisting = mergedExisting ? mergePollDto(mergedExisting, entry.poll) : entry.poll;
    }
    const merged = mergedExisting
      ? mergePollDto(mergedExisting, normalizedUpdate)
      : normalizedUpdate;

    for (const entry of entries) {
      if (entry.idx !== canonical.idx) {
        droppedIndices.add(entry.idx);
      }
    }
    replacementByIdx.set(canonical.idx, withMergedPollPayload(canonical.msg, merged));
  }

  const next: MessageDto[] = [];
  for (let i = 0; i < list.length; i++) {
    if (droppedIndices.has(i)) continue;
    next.push(replacementByIdx.get(i) ?? list[i]);
  }
  return next;
}

export function upsertMsg(list: MessageDto[], incoming: MessageDto): MessageDto[] {
  const { pollId: incomingPollId, poll: incomingPoll } = extractPollIdForDedupe(incoming);

  const idx = list.findIndex((m) => m.msg_id === incoming.msg_id);
  if (idx >= 0) {
    const next = [...list];
    const existing = list[idx];
    let replacement = incoming;
    if (incomingPollId > 0 && incomingPoll) {
      const existingPoll = parsePollPayload(existing);
      if (existingPoll && existingPoll.poll_id === incomingPollId) {
        replacement = withMergedPollPayload(incoming, mergePollDto(existingPoll, incomingPoll));
      }
    }
    next[idx] = replacement;
    return incomingPollId > 0
      ? dedupePollGroups(next, { threadId: incoming.thread_id, pollId: incomingPollId })
      : next;
  }

  let base = list;
  let toInsert = incoming;

  if (incomingPollId > 0 && incomingPoll) {
    if (!isOptimisticPollMsgId(incoming.msg_id)) {
      const matchingRealIndices: number[] = [];
      const matchingOptIndices: number[] = [];
      for (let i = 0; i < base.length; i++) {
        const m = base[i];
        if (m.thread_id !== incoming.thread_id) continue;
        const { pollId } = extractPollIdForDedupe(m);
        if (pollId !== incomingPollId) continue;
        if (isOptimisticPollMsgId(m.msg_id)) {
          matchingOptIndices.push(i);
        } else {
          matchingRealIndices.push(i);
        }
      }

      if (matchingRealIndices.length > 0) {
        matchingRealIndices.sort((a, b) => base[a].ts - base[b].ts || a - b);
        matchingOptIndices.sort((a, b) => base[a].ts - base[b].ts || a - b);
        const canonicalIdx = matchingRealIndices[0];
        const canonicalMsg = base[canonicalIdx];

        let mergedPoll: PollDto | null = null;
        for (const optIdx of matchingOptIndices) {
          const optPoll = parsePollPayload(base[optIdx]);
          if (optPoll) {
            mergedPoll = mergedPoll ? mergePollDto(mergedPoll, optPoll) : optPoll;
          }
        }
        const realPayloads: Array<{ ts: number; order: number; poll: PollDto }> = [];
        for (let k = 0; k < matchingRealIndices.length; k++) {
          const rIdx = matchingRealIndices[k];
          const p = parsePollPayload(base[rIdx]);
          if (p) {
            realPayloads.push({ ts: base[rIdx].ts, order: k, poll: p });
          }
        }
        realPayloads.push({
          ts: incoming.ts,
          order: matchingRealIndices.length,
          poll: incomingPoll,
        });
        realPayloads.sort((a, b) => a.ts - b.ts || a.order - b.order);
        for (const item of realPayloads) {
          mergedPoll = mergedPoll ? mergePollDto(mergedPoll, item.poll) : item.poll;
        }

        const dropped = new Set<number>([
          ...matchingOptIndices,
          ...matchingRealIndices.slice(1),
        ]);
        const next: MessageDto[] = [];
        for (let i = 0; i < base.length; i++) {
          if (dropped.has(i)) continue;
          next.push(
            i === canonicalIdx
              ? withMergedPollPayload(canonicalMsg, mergedPoll ?? incomingPoll)
              : base[i],
          );
        }
        return next;
      }

      if (matchingOptIndices.length > 0) {
        matchingOptIndices.sort((a, b) => base[a].ts - base[b].ts || a - b);
        let mergedOptPoll: PollDto | null = null;
        const dropped = new Set<number>(matchingOptIndices);
        for (const optIdx of matchingOptIndices) {
          const optPoll = parsePollPayload(base[optIdx]);
          if (optPoll) {
            mergedOptPoll = mergedOptPoll ? mergePollDto(mergedOptPoll, optPoll) : optPoll;
          }
        }
        base = base.filter((_, i) => !dropped.has(i));
        if (mergedOptPoll) {
          toInsert = withMergedPollPayload(incoming, mergePollDto(mergedOptPoll, incomingPoll));
        }
      }
    } else {
      const realIdx = base.findIndex(
        (m) =>
          m.thread_id === incoming.thread_id &&
          !isOptimisticPollMsgId(m.msg_id) &&
          extractPollIdForDedupe(m).pollId === incomingPollId,
      );
      if (realIdx >= 0) {
        const realMsg = base[realIdx];
        const realPoll = parsePollPayload(realMsg);
        if (
          realPoll &&
          (realPoll.options.length === 0 || isPollVoteActivityQuestion(realPoll.question))
        ) {
          const next = [...base];
          next[realIdx] = withMergedPollPayload(realMsg, mergePollDto(incomingPoll, realPoll));
          return next;
        }
        return base;
      }
    }
  }

  if (toInsert.is_outgoing && !toInsert.msg_id.startsWith("opt-")) {
    const incomingText = toInsert.text.trim();
    const optIdx = base.findIndex(
      (m) =>
        m.is_outgoing &&
        m.msg_id.startsWith("opt-") &&
        m.thread_id === toInsert.thread_id &&
        m.text.trim() === incomingText &&
        Math.abs(toInsert.ts - m.ts) < 30_000,
    );
    if (optIdx >= 0) {
      base = [...base.slice(0, optIdx), ...base.slice(optIdx + 1)];
    }
  }
  return [...base, toInsert].sort((a, b) => a.ts - b.ts);
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
  return looksLikeInternationalPhoneQuery(query);
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

export function applyThreadReadSync(
  threads: ThreadDto[],
  threadId: string,
  isUnread: boolean,
): ThreadDto[] {
  const idx = threads.findIndex((t) => t.id === threadId);
  if (idx < 0) return threads;
  const target = threads[idx];
  const next = [...threads];
  if (!isUnread) {
    next[idx] = {
      ...target,
      unread_count: 0,
      last_read_in_ts: Math.max(target.last_read_in_ts ?? 0, target.last_ts),
    };
  } else {
    next[idx] = {
      ...target,
      unread_count: Math.max(1, target.unread_count),
    };
  }
  return next;
}

export interface AutoMarkReadOnFocusParams {
  authed: boolean;
  isLocked: boolean;
  hasActiveWebApp: boolean;
  webAppHubOpen?: boolean;
  windowFocused: boolean;
  visibilityState: string;
  selectedId: string | null;
  pendingMarkReadId: string | null;
  activeThreadUnread: number;
  distFromBottom: number | null;
  isScrollable?: boolean;
  chatPaneVisible?: boolean;
  initialScrollReady?: boolean;
  nearBottomThresholdPx?: number;
}

export function shouldAutoMarkReadOnFocus(params: AutoMarkReadOnFocusParams): boolean {
  if (!params.authed) return false;
  if (params.isLocked) return false;
  if (params.hasActiveWebApp || params.webAppHubOpen) return false;
  if (params.chatPaneVisible === false) return false;
  if (params.initialScrollReady === false) return false;
  if (!params.windowFocused || params.visibilityState !== "visible") return false;
  if (!params.selectedId) return false;

  const hasUnread =
    params.pendingMarkReadId === params.selectedId || params.activeThreadUnread > 0;
  if (!hasUnread) return false;

  if (params.isScrollable === false) return true;
  if (params.distFromBottom === null) return false;
  const threshold = params.nearBottomThresholdPx ?? 90;
  return params.distFromBottom < threshold;
}

export function resolveFolderForOpenedThread(
  threads: ThreadDto[],
  targetThreadId: string,
  currentFolder: string,
  customFolders: ChatFolderDto[],
): string {
  const currentlyVisible = filterThreadsByFolder(threads, currentFolder, customFolders);
  if (currentlyVisible.some((t) => t.id === targetThreadId)) {
    return currentFolder;
  }
  const target = threads.find((t) => t.id === targetThreadId);
  if (target?.is_archived) {
    return "archived";
  }
  return "all";
}

export function computeThreadRenderLimitForTarget(
  visibleThreads: ThreadDto[],
  targetThreadId: string | null,
  currentLimit: number,
  pageSize = 100,
): number {
  const safePageSize = Math.max(1, pageSize);
  const safeLimit = Math.max(safePageSize, currentLimit);
  if (!targetThreadId) return safeLimit;
  const idx = visibleThreads.findIndex((t) => t.id === targetThreadId);
  if (idx < 0 || idx < safeLimit) {
    return safeLimit;
  }
  return Math.ceil((idx + 1) / safePageSize) * safePageSize;
}

export interface SidebarThreadSelectionState {
  filterText: string;
  selectedId: string | null;
  folder: string;
}

export interface SidebarThreadSelectionResult {
  filterText: string;
  selectedId: string;
  folder: string;
  clearedSearch: boolean;
  shouldScrollSidebar: boolean;
}

export function applyOpenThreadSearchState(
  state: SidebarThreadSelectionState,
  targetThreadId: string,
  threads: ThreadDto[],
  customFolders: ChatFolderDto[],
): SidebarThreadSelectionResult {
  const clearedSearch = state.filterText.length > 0;
  const nextFolder = resolveFolderForOpenedThread(
    threads,
    targetThreadId,
    state.folder,
    customFolders,
  );
  return {
    filterText: "",
    selectedId: targetThreadId,
    folder: nextFolder,
    clearedSearch,
    shouldScrollSidebar: clearedSearch || nextFolder !== state.folder,
  };
}

export const QUICK_REACTION_EMOJIS = ["👍", "❤️", "😆", "😮", "😢", "😠"] as const;

const REACTION_CODE_TO_CANONICAL: Record<string, string> = {
  "/-strong": "👍",
  "👍": "👍",
  "/-heart": "❤️",
  "/-loveu": "❤️",
  "❤": "❤️",
  "❤️": "❤️",
  ":>": "😆",
  ":))": "😆",
  "😆": "😆",
  ":o": "😮",
  ":-o": "😮",
  "😮": "😮",
  ":-((": "😢",
  ":((": "😢",
  "--b": "😢",
  "p-(": "😢",
  ":-<": "😢",
  "😢": "😢",
  "😭": "😢",
  ":-h": "😠",
  ">-|": "😠",
  "😠": "😠",
  "😡": "😠",
  ":-*": "😘",
  "😘": "😘",
  ":')": "😂",
  "😂": "😂",
  "/-shit": "💩",
  "💩": "💩",
  "/-rose": "🌹",
  "🌹": "🌹",
  "/-break": "💔",
  "💔": "💔",
  "/-weak": "👎",
  "/-bad": "👎",
  "👎": "👎",
  ";xx": "😍",
  "😍": "😍",
  ";-/": "😕",
  "😕": "😕",
  ";-)": "😉",
  "😉": "😉",
  "/-fade": "🥀",
  "🥀": "🥀",
  "/-li": "☀️",
  "☀": "☀️",
  "☀️": "☀️",
  "/-bd": "🎂",
  "🎂": "🎂",
  "/-bome": "💣",
  "💣": "💣",
  "/-ok": "👌",
  "👌": "👌",
  "/-v": "✌️",
  "✌": "✌️",
  "✌️": "✌️",
  "/-thanks": "🙏",
  "_()_": "🙏",
  "🙏": "🙏",
  "/-punch": "👊",
  "👊": "👊",
  "/-no": "🙅",
  "🙅": "🙅",
  "x-)": "😎",
  "b-)": "😎",
  "😎": "😎",
  "8-)": "🤓",
  "🤓": "🤓",
  ";-d": "😁",
  "😁": "😁",
  ":--|": "😐",
  "😐": "😐",
  ":-bye": "👋",
  "👋": "👋",
  "|-)": "😴",
  "😴": "😴",
  ":handclap": "👏",
  "👏": "👏",
  ";-x": "🤐",
  "🤐": "🤐",
  ";-s": "😳",
  "😳": "😳",
  ";-a": "😨",
  "😨": "😨",
  "$-)": "🤑",
  "🤑": "🤑",
  "/-beer": "🍺",
  "🍺": "🍺",
};

export function normalizeReactionEmoji(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return "";
  const lower = trimmed.toLowerCase();
  if (lower === "/-none" || lower === "/-remove" || lower === "none") {
    return "";
  }
  return REACTION_CODE_TO_CANONICAL[trimmed] ?? REACTION_CODE_TO_CANONICAL[lower] ?? trimmed;
}

export function normalizeReactions(
  reactions: readonly ReactionSummary[] | null | undefined,
): ReactionSummary[] {
  if (!reactions || reactions.length === 0) return [];
  const merged: ReactionSummary[] = [];
  const byEmoji = new Map<string, number>();
  for (const item of reactions) {
    if (!item || item.count <= 0) continue;
    const emoji = normalizeReactionEmoji(item.emoji);
    if (!emoji) continue;
    const existingIdx = byEmoji.get(emoji);
    if (existingIdx !== undefined) {
      merged[existingIdx] = {
        emoji,
        count: merged[existingIdx].count + item.count,
        me: merged[existingIdx].me || Boolean(item.me),
      };
    } else {
      byEmoji.set(emoji, merged.length);
      merged.push({
        emoji,
        count: item.count,
        me: Boolean(item.me),
      });
    }
  }
  return merged;
}

export function applyOptimisticReaction(
  reactions: readonly ReactionSummary[] | null | undefined,
  rawEmoji: string,
): ReactionSummary[] {
  const list = normalizeReactions(reactions).map((r) => ({ ...r }));
  const target = normalizeReactionEmoji(rawEmoji);
  if (!target) return list;

  const clicked = list.find((r) => r.emoji === target);
  if (clicked?.me) {
    clicked.me = false;
    clicked.count = Math.max(0, clicked.count - 1);
  } else {
    const prevMe = list.find((r) => r.me);
    if (prevMe) {
      prevMe.me = false;
      prevMe.count = Math.max(0, prevMe.count - 1);
    }
    if (clicked) {
      clicked.count += 1;
      clicked.me = true;
    } else {
      list.push({ emoji: target, count: 1, me: true });
    }
  }
  return list.filter((r) => r.count > 0);
}

export function reconcileUpdatedMessage(
  list: MessageDto[],
  incoming: MessageDto,
): MessageDto[] {
  const normalized: MessageDto = {
    ...incoming,
    reactions: normalizeReactions(incoming.reactions),
  };
  const idx = list.findIndex((m) => m.msg_id === normalized.msg_id);
  if (idx >= 0) {
    const next = [...list];
    const existingPoll = parsePollPayload(list[idx]);
    const incomingPoll = parsePollPayload(normalized);
    if (existingPoll && incomingPoll && existingPoll.poll_id > 0 && existingPoll.poll_id === incomingPoll.poll_id) {
      next[idx] = withMergedPollPayload(normalized, mergePollDto(existingPoll, incomingPoll));
      return dedupePollGroups(next, { threadId: normalized.thread_id, pollId: incomingPoll.poll_id });
    }
    next[idx] = normalized;
    return next;
  }
  const { pollId: incomingPollId, poll: incomingPoll } = extractPollIdForDedupe(normalized);
  if (incomingPoll && incomingPollId > 0) {
    const byPollIdx = list.findIndex(
      (m) =>
        m.thread_id === normalized.thread_id &&
        extractPollIdForDedupe(m).pollId === incomingPollId,
    );
    if (byPollIdx >= 0) {
      const existing = list[byPollIdx];
      const existingPoll = parsePollPayload(existing);
      const existingIsOpt = isOptimisticPollMsgId(existing.msg_id);
      const incomingIsOpt = isOptimisticPollMsgId(normalized.msg_id);
      const upgradeOptToReal = existingIsOpt && !incomingIsOpt;
      const incomingOlderThanReal =
        !existingIsOpt &&
        (incomingIsOpt || (normalized.ts > 0 && existing.ts > 0 && normalized.ts < existing.ts));
      const merged = existingPoll
        ? incomingOlderThanReal
          ? mergePollDto(incomingPoll, existingPoll)
          : mergePollDto(existingPoll, incomingPoll)
        : incomingPoll;
      const baseMsg = upgradeOptToReal ? normalized : existing;
      const next = [...list];
      next[byPollIdx] = withMergedPollPayload(baseMsg, merged);
      return dedupePollGroups(next, { threadId: normalized.thread_id, pollId: incomingPollId });
    }
  }
  if (list.length > 0 && normalized.ts < list[0].ts) {
    return list;
  }
  return upsertMsg(list, normalized);
}
