import type { StickerItem } from "../types";

export const STICKER_SUGGEST_MAX_CHARS = 24;
export const STICKER_SUGGEST_MAX_WORDS = 4;
export const STICKER_SUGGEST_DEBOUNCE_MS = 260;
export const STICKER_SUGGEST_LIMIT = 16;

const SINGLE_CHAR_TRIGGERS = new Set(["네", "응"]);

const EMOJI_TO_STICKER_KEYWORD: Record<string, string> = {
  "😂": "haha",
  "🤣": "haha",
  "😆": "haha",
  "😄": "haha",
  "😅": "haha",
  "❤": "love",
  "♥": "love",
  "💕": "love",
  "💖": "love",
  "😍": "love",
  "🥰": "love",
  "😘": "love",
  "👍": "ok",
  "👌": "ok",
  "🤝": "ok",
  "🙏": "thanks",
  "👏": "happy",
  "🎉": "happy",
  "🥳": "happy",
  "👋": "hi",
  "😢": "cry",
  "😭": "cry",
  "🥺": "cry",
  "💔": "cry",
  "😡": "angry",
  "🤬": "angry",
  "😤": "angry",
  "😴": "sleep",
  "😪": "sleep",
  "🔥": "happy",
  "🙇": "sorry",
};

const PURE_NON_WORD_RE = /^[\d\s+\-.,:;!?()[\]{}<>/*=\\|_~`'"@#$%^&]+$/u;
const URL_OR_EMAIL_RE = /(?:https?:\/\/|www\.|\S+@\S+\.\S+)/i;

export interface StickerSuggestContext {
  enabled?: boolean;
  hasPendingAttachment?: boolean;
  mentionOpen?: boolean;
  emojiOpen?: boolean;
}

/**
 * Extract a clean sticker suggestion query from the composer draft text,
 * or return `null` when suggestions should stay hidden (long messages,
 * mentions, slash commands, URLs, pure numbers, captions, or disabled).
 */
export function extractStickerSuggestQuery(
  draft: string,
  ctx: StickerSuggestContext = {},
): string | null {
  if (ctx.enabled === false || ctx.hasPendingAttachment || ctx.mentionOpen || ctx.emojiOpen) {
    return null;
  }
  if (!draft || draft.includes("\n") || draft.includes("\r")) {
    return null;
  }

  const trimmed = draft.trim().replace(/\s+/g, " ");
  if (!trimmed) {
    return null;
  }
  if (trimmed.startsWith("@") || trimmed.startsWith("/") || trimmed.startsWith("#")) {
    return null;
  }
  if (URL_OR_EMAIL_RE.test(trimmed)) {
    return null;
  }

  const strippedEmoji = trimmed.replace(/\uFE0F/g, "");
  const emojiMapped = EMOJI_TO_STICKER_KEYWORD[strippedEmoji];
  if (emojiMapped) {
    return emojiMapped;
  }

  if (PURE_NON_WORD_RE.test(trimmed)) {
    return null;
  }

  const charCount = Array.from(trimmed).length;
  if (charCount < 2 && !SINGLE_CHAR_TRIGGERS.has(trimmed)) {
    return null;
  }
  if (charCount > STICKER_SUGGEST_MAX_CHARS) {
    return null;
  }

  const words = trimmed.split(" ");
  if (words.length > STICKER_SUGGEST_MAX_WORDS) {
    return null;
  }

  return trimmed;
}

interface CacheEntry {
  items: StickerItem[];
  expiresAt: number;
}

const DEFAULT_HIT_TTL_MS = 10 * 60 * 1000;
const DEFAULT_EMPTY_TTL_MS = 2 * 60 * 1000;
const DEFAULT_MAX_ENTRIES = 80;

export class StickerSuggestCache {
  private readonly store = new Map<string, CacheEntry>();
  private readonly maxEntries: number;
  private readonly hitTtlMs: number;
  private readonly emptyTtlMs: number;

  constructor(
    maxEntries = DEFAULT_MAX_ENTRIES,
    hitTtlMs = DEFAULT_HIT_TTL_MS,
    emptyTtlMs = DEFAULT_EMPTY_TTL_MS,
  ) {
    this.maxEntries = maxEntries;
    this.hitTtlMs = hitTtlMs;
    this.emptyTtlMs = emptyTtlMs;
  }

  private normalizeKey(query: string): string {
    return query.trim().replace(/\s+/g, " ").toLowerCase();
  }

  get(query: string, now = Date.now()): StickerItem[] | undefined {
    const key = this.normalizeKey(query);
    if (!key) return undefined;
    const entry = this.store.get(key);
    if (!entry) return undefined;
    if (entry.expiresAt <= now) {
      this.store.delete(key);
      return undefined;
    }
    // Refresh LRU order.
    this.store.delete(key);
    this.store.set(key, entry);
    return entry.items;
  }

  set(query: string, items: StickerItem[], now = Date.now()): void {
    const key = this.normalizeKey(query);
    if (!key) return;
    if (this.store.has(key)) {
      this.store.delete(key);
    } else if (this.store.size >= this.maxEntries) {
      const oldestKey = this.store.keys().next().value;
      if (oldestKey !== undefined) {
        this.store.delete(oldestKey);
      }
    }
    const ttl = items.length > 0 ? this.hitTtlMs : this.emptyTtlMs;
    this.store.set(key, {
      items,
      expiresAt: now + ttl,
    });
  }

  clear(): void {
    this.store.clear();
  }
}

export const sharedStickerSuggestCache = new StickerSuggestCache();
