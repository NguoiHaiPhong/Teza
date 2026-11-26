export const MENTION_ALL_ID = "__all__";

export interface MentionCandidate {
  id: string;
  name: string;
}

export interface MentionMemberItem {
  id: string;
  name: string;
  avatar: string;
  role: string;
}

export interface ActiveMentionQuery {
  start: number;
  query: string;
}

export type MentionToken =
  | { kind: "text"; text: string }
  | {
      kind: "mention";
      text: string;
      userId: string | null;
      displayName?: string;
      isAll?: boolean;
    };

const MENTION_ALL_ALIASES = ["All", "Mọi người", "everyone", "tất cả"] as const;
const MAX_MENTION_QUERY_CHARS = 32;
const MAX_MENTION_QUERY_WORDS = 4;

const MENTION_FALLBACK_STOPWORDS = new Set([
  "please",
  "dear",
  "hello",
  "hi",
  "good",
  "thanks",
  "thank",
  "sorry",
  "could",
  "would",
  "should",
  "will",
  "we",
  "you",
  "they",
  "this",
  "that",
  "the",
  "there",
  "here",
  "today",
  "tomorrow",
  "yesterday",
  "update",
  "meeting",
  "check",
  "send",
  "share",
  "attend",
  "confirm",
  "verify",
  "review",
  "note",
  "urgent",
  "important",
  "how",
  "what",
  "when",
  "where",
  "why",
  "who",
  "yes",
  "no",
  "ok",
  "okay",
  "sure",
  "done",
  "underfill",
  "auto",
  "pickup",
  "iconic",
  "still",
  "making",
]);

/**
 * Normalizes Vietnamese text for accent-insensitive search & mention matching:
 * NFC normalize -> lowercase -> map `đ` -> `d` -> strip combining diacritics.
 * Handles both NFC precomposed and NFD decomposed (IME/macOS) input identically.
 */
export function foldVietnameseText(input: string): string {
  if (!input) return "";
  return input
    .normalize("NFC")
    .toLowerCase()
    .replace(/đ/g, "d")
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .normalize("NFC");
}

function stripAccents(input: string): string {
  return foldVietnameseText(input);
}

export interface CompletedMentionSpan {
  start: number;
  text: string;
}

/**
 * Updates tracked completed `@Name ` spans after an edit from `prevDraft` to `nextDraft`.
 * Preserves spans unaffected by edits before/after them (shifting `start` on prepend/insert),
 * and drops spans whose characters were directly edited or backspaced into.
 */
export function reconcileCompletedMentions(
  prevList: CompletedMentionSpan[],
  prevDraft: string,
  nextDraft: string,
): CompletedMentionSpan[] {
  if (prevList.length === 0 || !nextDraft) return [];
  if (prevDraft === nextDraft) return prevList;

  const minLen = Math.min(prevDraft.length, nextDraft.length);
  let prefixLen = 0;
  while (prefixLen < minLen && prevDraft[prefixLen] === nextDraft[prefixLen]) {
    prefixLen += 1;
  }

  let suffixLen = 0;
  const maxSuffix = minLen - prefixLen;
  while (
    suffixLen < maxSuffix &&
    prevDraft[prevDraft.length - 1 - suffixLen] ===
      nextDraft[nextDraft.length - 1 - suffixLen]
  ) {
    suffixLen += 1;
  }

  const prevEditEnd = prevDraft.length - suffixLen;
  const delta = nextDraft.length - prevDraft.length;
  const nextList: CompletedMentionSpan[] = [];

  for (const entry of prevList) {
    const entryEnd = entry.start + entry.text.length;
    let nextStart: number;
    if (entryEnd <= prefixLen) {
      nextStart = entry.start;
    } else if (entry.start >= prevEditEnd) {
      nextStart = entry.start + delta;
    } else {
      continue;
    }
    if (
      nextStart >= 0 &&
      nextStart + entry.text.length <= nextDraft.length &&
      nextDraft.slice(nextStart, nextStart + entry.text.length) === entry.text
    ) {
      nextList.push({ start: nextStart, text: entry.text });
    }
  }

  return nextList;
}

export function isMentionAllMember(member: Pick<MentionCandidate, "id" | "name">): boolean {
  const id = member.id.trim().toLowerCase();
  if (id === MENTION_ALL_ID || id === "all") return true;
  const folded = foldVietnameseText(member.name.trim());
  return (
    folded === "all" ||
    folded === "moi nguoi" ||
    folded === "everyone" ||
    folded === "tat ca"
  );
}

function isAllowedBeforeAt(ch: string | undefined): boolean {
  if (!ch) return true;
  return /[\s([{"',;:]/u.test(ch);
}

/**
 * Finds an active `@query` mention immediately before `cursorPos`.
 * Supports single-word and multi-word Vietnamese names (up to 4 words / 32 NFC chars),
 * ignores emails (`foo@bar.com`), newlines, punctuation, leading spaces after `@`,
 * and stops after 2+ consecutive spaces.
 */
export function findActiveMentionQuery(
  draft: string,
  cursorPos: number,
): ActiveMentionQuery | null {
  if (!draft || cursorPos <= 0) return null;
  const bounded = Math.max(0, Math.min(cursorPos, draft.length));
  const before = draft.slice(0, bounded);
  const atPos = before.lastIndexOf("@");
  if (atPos < 0) return null;
  if (atPos > 0 && !isAllowedBeforeAt(before[atPos - 1])) {
    return null;
  }

  const rawQuery = before.slice(atPos + 1);
  if (rawQuery.length === 0) {
    return { start: atPos, query: "" };
  }
  if (/[\r\n\t]/.test(rawQuery)) return null;
  if (/^\s/.test(rawQuery)) return null;
  if (/ {2,}/.test(rawQuery)) return null;
  if (rawQuery.normalize("NFC").length > MAX_MENTION_QUERY_CHARS) return null;
  if (!/^[\p{L}\p{N}\p{M}_' -]+$/u.test(rawQuery)) return null;

  const words = rawQuery.trim().split(" ").filter(Boolean);
  if (words.length > MAX_MENTION_QUERY_WORDS) return null;
  if (words.length >= MAX_MENTION_QUERY_WORDS && rawQuery.endsWith(" ")) return null;

  return { start: atPos, query: rawQuery };
}

function isMatchBoundary(haystack: string, index: number): boolean {
  if (index === 0) return true;
  const prev = haystack[index - 1];
  return prev === " " || prev === "-" || prev === "_" || prev === ".";
}

/**
 * Classifies how `needle` matches `haystack`:
 * - 0: exact full-string match
 * - 1: full-string prefix match
 * - 2: word-boundary prefix match on the final word segment (given name in Vietnamese names)
 * - 3: word-boundary prefix match on an earlier/middle word
 * - 4: mid-word substring match
 * - -1: no match
 */
function classifyMatch(haystack: string, needle: string): number {
  if (!needle) return 0;
  if (haystack === needle) return 0;
  if (haystack.startsWith(needle)) return 1;

  let best = -1;
  let searchFrom = 0;
  while (searchFrom < haystack.length) {
    const idx = haystack.indexOf(needle, searchFrom);
    if (idx < 0) break;
    if (isMatchBoundary(haystack, idx)) {
      const afterMatch = haystack.slice(idx + needle.length);
      const score = afterMatch.includes(" ") ? 3 : 2;
      if (best < 0 || score < best) best = score;
      if (best === 2) break;
    } else if (best < 0) {
      best = 4;
    }
    searchFrom = idx + 1;
  }
  return best;
}

function matchOrderedWordPrefixes(haystack: string, needle: string): boolean {
  const trailingSpace = needle.endsWith(" ");
  const needleWords = needle.trim().split(" ").filter(Boolean);
  if (needleWords.length < 2) return false;
  const hayWords = haystack.split(/[\s\-_.]+/).filter(Boolean);
  if (hayWords.length < needleWords.length) return false;

  let hIdx = 0;
  for (let nIdx = 0; nIdx < needleWords.length; nIdx += 1) {
    const nw = needleWords[nIdx];
    let found = false;
    while (hIdx < hayWords.length) {
      const hw = hayWords[hIdx];
      hIdx += 1;
      if (hw.startsWith(nw)) {
        found = true;
        break;
      }
    }
    if (!found) return false;
  }
  if (trailingSpace && hIdx >= hayWords.length) {
    return false;
  }
  return true;
}

function scoreStandardCandidate(
  nameLower: string,
  nameFolded: string,
  queryLower: string,
  queryFolded: string,
): number {
  const rawKind = classifyMatch(nameLower, queryLower);
  if (rawKind >= 0 && rawKind <= 3) {
    return rawKind; // 0..3: accented exact / full-prefix / given-name prefix / word prefix
  }

  const foldedKind = classifyMatch(nameFolded, queryFolded);
  if (foldedKind >= 0 && foldedKind <= 3) {
    return 10 + foldedKind; // 10..13: accent-insensitive exact / full-prefix / word prefix
  }

  if (matchOrderedWordPrefixes(nameLower, queryLower)) {
    return 14; // multi-word ordered word-prefix (accented), e.g. "@Trần Lộc" -> "Trần Văn Lộc"
  }
  if (matchOrderedWordPrefixes(nameFolded, queryFolded)) {
    return 15; // multi-word ordered word-prefix (folded), e.g. "@tran loc" -> "Trần Văn Lộc"
  }

  if (rawKind === 4) {
    return 20; // accented substring
  }
  if (foldedKind === 4) {
    return 30; // accent-insensitive substring
  }
  return -1;
}

function scoreMentionAllCandidate(
  nameNfc: string,
  queryLower: string,
  queryFolded: string,
): number {
  const aliases = [nameNfc, ...MENTION_ALL_ALIASES];
  let best = -1;
  for (const alias of aliases) {
    const aliasLower = alias.normalize("NFC").toLowerCase();
    const aliasFolded = foldVietnameseText(alias);
    const rawKind = classifyMatch(aliasLower, queryLower);
    if (rawKind >= 0 && rawKind <= 3) {
      if (best < 0 || rawKind < best) best = rawKind;
      continue;
    }
    const foldedKind = classifyMatch(aliasFolded, queryFolded);
    if (foldedKind >= 0 && foldedKind <= 3) {
      const score = 10 + foldedKind;
      if (best < 0 || score < best) best = score;
    }
  }
  return best;
}

/**
 * Filters, deduplicates (by `id`), and ranks mention candidates against `query`.
 * Ranking order:
 * 1. Exact/accented prefix on full name or word boundary (given name prioritized over middle word)
 * 2. Accent-insensitive prefix on full name or word boundary (+ ordered multi-word word-prefixes)
 * 3. Substring match (accented before accent-insensitive)
 */
export function filterMentionCandidates<T extends { id: string; name: string }>(
  members: T[],
  query: string,
  limit = 8,
): T[] {
  if (!Array.isArray(members) || members.length === 0 || limit <= 0) return [];

  const deduped: Array<{ item: T; nameNfc: string; index: number }> = [];
  const seenKeys = new Set<string>();
  for (let i = 0; i < members.length; i += 1) {
    const item = members[i];
    const nameNfc = (item.name || "").normalize("NFC").trim().replace(/\s+/g, " ");
    if (!nameNfc) continue;
    const idKey = (item.id || "").trim();
    const dedupeKey = idKey ? `id:${idKey}` : `name:${foldVietnameseText(nameNfc)}`;
    if (seenKeys.has(dedupeKey)) continue;
    seenKeys.add(dedupeKey);
    deduped.push({ item, nameNfc, index: i });
  }

  const queryNfc = (query || "")
    .normalize("NFC")
    .replace(/^\s+/, "")
    .replace(/\s{2,}/g, " ");
  if (!queryNfc) {
    return deduped.slice(0, limit).map((entry) => entry.item);
  }

  const queryLower = queryNfc.toLowerCase();
  const queryFolded = foldVietnameseText(queryNfc);
  if (!queryFolded.trim()) {
    return deduped.slice(0, limit).map((entry) => entry.item);
  }

  const scored: Array<{ item: T; score: number; nameLen: number; index: number }> = [];
  for (const entry of deduped) {
    const score = isMentionAllMember(entry.item)
      ? scoreMentionAllCandidate(entry.nameNfc, queryLower, queryFolded)
      : scoreStandardCandidate(
          entry.nameNfc.toLowerCase(),
          foldVietnameseText(entry.nameNfc),
          queryLower,
          queryFolded,
        );
    if (score >= 0) {
      scored.push({
        item: entry.item,
        score,
        nameLen: entry.nameNfc.length,
        index: entry.index,
      });
    }
  }

  scored.sort((a, b) => {
    if (a.score !== b.score) return a.score - b.score;
    if (a.nameLen !== b.nameLen) return a.nameLen - b.nameLen;
    return a.index - b.index;
  });

  return scored.slice(0, limit).map((entry) => entry.item);
}

export function isPlaceholderMemberName(name: string): boolean {
  const trimmed = name.trim();
  if (!trimmed) return true;
  const folded = foldVietnameseText(trimmed);
  if (folded === "ban" || folded === "you") return true;
  if (/^\d{6,}$/.test(trimmed)) return true;
  if (/^nguoi dung(?:\s+\d+)?$/.test(folded)) return true;
  return false;
}

export interface BuildComposerMentionMembersInput {
  thread: {
    id: string;
    title: string;
    avatar: string;
    kind: "direct" | "group" | "cloud";
  } | null;
  groupMembers: MentionMemberItem[];
  messages: Array<{
    sender_id: string;
    sender_name: string;
    is_outgoing?: boolean;
  }>;
  selfUid?: string | null;
  directDirectory?: Map<string, { name: string; avatar: string }>;
  mentionAllRole?: string;
}

/**
 * Builds the deduplicated composer autocomplete member list for the active thread:
 * - Group chats: `@All` entry + loaded/cached group members + known message senders
 * - Direct chats: conversation partner + non-self message senders
 * - Cloud chat: empty
 * Excludes self (`selfUid`) and replaces generic/numeric placeholder names when real names exist.
 */
export function buildComposerMentionMembers({
  thread,
  groupMembers,
  messages,
  selfUid,
  directDirectory,
  mentionAllRole = "Mọi người",
}: BuildComposerMentionMembersInput): MentionMemberItem[] {
  if (!thread || thread.kind === "cloud") return [];

  const normalizedSelf = (selfUid ?? "").trim();
  const byId = new Map<string, MentionMemberItem>();
  const order: string[] = [];

  const upsert = (
    rawId: string | undefined,
    rawName: string | undefined,
    rawAvatar: string | undefined,
    rawRole: string | undefined,
    allowPlaceholderInitially: boolean,
  ) => {
    const id = (rawId ?? "").trim();
    if (!id) return;
    if (normalizedSelf && id === normalizedSelf) return;

    const dirEntry = directDirectory?.get(id);
    let name = (rawName ?? "").normalize("NFC").trim();
    if (isPlaceholderMemberName(name) && dirEntry?.name && !isPlaceholderMemberName(dirEntry.name)) {
      name = dirEntry.name.normalize("NFC").trim();
    }
    const avatar = (rawAvatar ?? "").trim() || dirEntry?.avatar || "";
    const role = (rawRole ?? "").trim();

    if (!name) return;
    const isPlaceholder = isPlaceholderMemberName(name);
    if (isPlaceholder && !allowPlaceholderInitially) return;

    const existing = byId.get(id);
    if (!existing) {
      byId.set(id, { id, name, avatar, role });
      order.push(id);
      return;
    }

    const existingPlaceholder = isPlaceholderMemberName(existing.name);
    if (existingPlaceholder && !isPlaceholder) {
      existing.name = name;
    } else if (!existingPlaceholder && !isPlaceholder && name.length > existing.name.length && !existing.role) {
      // Only upgrade non-authoritative sender names when a fuller name appears
      existing.name = name;
    }
    if (!existing.avatar && avatar) {
      existing.avatar = avatar;
    }
    if (!existing.role && role) {
      existing.role = role;
    }
  };

  if (thread.kind === "direct") {
    upsert(thread.id, thread.title, thread.avatar, "", false);
  } else if (thread.kind === "group") {
    for (const mem of groupMembers) {
      upsert(mem.id, mem.name, mem.avatar, mem.role, true);
    }
  }

  // Walk messages newest-first so recently active speakers appear first when group details are still loading
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const msg = messages[i];
    if (msg.is_outgoing) continue;
    upsert(msg.sender_id, msg.sender_name, "", "", false);
  }

  const resolvedMembers: MentionMemberItem[] = [];
  if (thread.kind === "group") {
    resolvedMembers.push({
      id: MENTION_ALL_ID,
      name: "All",
      avatar: "",
      role: mentionAllRole,
    });
  }

  for (const id of order) {
    const item = byId.get(id);
    if (!item || isPlaceholderMemberName(item.name)) continue;
    resolvedMembers.push(item);
  }

  return resolvedMembers;
}

function isWordBoundaryChar(ch: string | undefined): boolean {
  if (!ch) return true;
  return !/[\p{L}\p{N}_]/u.test(ch);
}

function isTitleCaseWord(core: string): boolean {
  const chars = [...core];
  if (chars.length < 2) return false;
  return /\p{Lu}/u.test(chars[0]) && chars.slice(1).every((c) => /\p{Ll}/u.test(c));
}

function isAcceptedFallbackContinuation(core: string): boolean {
  if (!core) return false;
  const chars = [...core];
  if (chars.length === 1) {
    return core !== "I" && /\p{Lu}/u.test(chars[0]);
  }
  return isTitleCaseWord(core) && !MENTION_FALLBACK_STOPWORDS.has(core.toLowerCase());
}

function pushText(out: MentionToken[], s: string) {
  if (!s) return;
  const prev = out[out.length - 1];
  if (prev && prev.kind === "text") {
    prev.text += s;
  } else {
    out.push({ kind: "text", text: s });
  }
}

/**
 * Splits a message into plain text and `@mention` tokens, matching multi-word
 * member names (longest-first, accent-insensitive fallback) before falling back
 * to structural Title-Case multi-word name scanning.
 */
export function tokenizeMentions(
  input: string,
  candidates: MentionCandidate[] = [],
): MentionToken[] {
  if (!input) return [];
  const text = input.normalize("NFC");

  const prepared = candidates
    .map((c) => {
      const nameNfc = c.name.normalize("NFC").trim();
      return {
        id: c.id.trim(),
        nameNfc,
        nameLower: nameNfc.toLowerCase(),
        nameNoAccent: stripAccents(nameNfc),
      };
    })
    .filter(
      (c) =>
        c.nameNfc.length > 0 &&
        c.id !== MENTION_ALL_ID &&
        c.nameLower !== "all" &&
        c.nameLower !== "everyone" &&
        c.nameNoAccent !== "moi nguoi" &&
        c.nameNoAccent !== "tat ca",
    )
    .sort((a, b) => b.nameNfc.length - a.nameNfc.length);

  const out: MentionToken[] = [];
  let cursor = 0;

  while (cursor < text.length) {
    const atPos = text.indexOf("@", cursor);
    if (atPos < 0) {
      pushText(out, text.slice(cursor));
      break;
    }

    if (atPos > 0 && !isAllowedBeforeAt(text[atPos - 1])) {
      pushText(out, text.slice(cursor, atPos + 1));
      cursor = atPos + 1;
      continue;
    }

    if (atPos > cursor) {
      pushText(out, text.slice(cursor, atPos));
    }

    const afterAt = text.slice(atPos + 1);
    const allMatch = afterAt.match(
      /^(all|everyone|m[oọ]i[ \t]+ng(?:ười|uoi)|t[aấ]t[ \t]+c[aả])(?=[^\p{L}\p{N}_]|$)/iu,
    );
    if (allMatch) {
      const endPos = atPos + 1 + allMatch[0].length;
      out.push({
        kind: "mention",
        text: text.slice(atPos, endPos),
        userId: null,
        isAll: true,
      });
      cursor = endPos;
      continue;
    }

    let matchedCand: (typeof prepared)[number] | null = null;
    for (const cand of prepared) {
      const len = cand.nameNfc.length;
      if (atPos + 1 + len > text.length) continue;
      if (!isWordBoundaryChar(text[atPos + 1 + len])) continue;
      const slice = text.slice(atPos + 1, atPos + 1 + len);
      if (slice.toLowerCase() === cand.nameLower) {
        matchedCand = cand;
        break;
      }
    }

    if (!matchedCand) {
      for (const cand of prepared) {
        const len = cand.nameNfc.length;
        if (atPos + 1 + len > text.length) continue;
        if (!isWordBoundaryChar(text[atPos + 1 + len])) continue;
        const slice = text.slice(atPos + 1, atPos + 1 + len);
        if (stripAccents(slice) === cand.nameNoAccent) {
          matchedCand = cand;
          break;
        }
      }
    }

    if (matchedCand) {
      const endPos = atPos + 1 + matchedCand.nameNfc.length;
      out.push({
        kind: "mention",
        text: text.slice(atPos, endPos),
        userId: matchedCand.id || null,
        displayName: matchedCand.nameNfc,
      });
      cursor = endPos;
      continue;
    }

    const firstTokMatch = afterAt.match(/^[\p{L}\p{N}_][^\s@<>.,:;!?)\]]*/u);
    if (!firstTokMatch) {
      pushText(out, "@");
      cursor = atPos + 1;
      continue;
    }

    let endPos = atPos + 1 + firstTokMatch[0].length;
    if (endPos < text.length && !/^[,.:;!?)\]]/.test(text[endPos])) {
      let scanPos = endPos;
      let consumed = 0;
      while (consumed < 3 && scanPos < text.length) {
        const wsMatch = text.slice(scanPos).match(/^[ \t]+/);
        if (!wsMatch) break;
        const nextStart = scanPos + wsMatch[0].length;
        const nextWordMatch = text.slice(nextStart).match(/^\S+/);
        if (!nextWordMatch) break;
        const rawNext = nextWordMatch[0];
        const core = rawNext.replace(/[,.:;!?)\]]+$/, "");
        if (!isAcceptedFallbackContinuation(core)) break;
        endPos = nextStart + core.length;
        scanPos = endPos;
        consumed += 1;
        if (core.length < rawNext.length) break;
      }
    }

    out.push({
      kind: "mention",
      text: text.slice(atPos, endPos),
      userId: null,
    });
    cursor = endPos;
  }

  return out;
}
