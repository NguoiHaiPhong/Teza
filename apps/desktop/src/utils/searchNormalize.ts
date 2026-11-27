const HANGUL_SYLLABLE_START = 0xac00;
const HANGUL_SYLLABLE_END = 0xd7a3;
const HANGUL_L_JAMO_START = 0x1100;
const HANGUL_L_JAMO_END = 0x1112;
const HANGUL_CHOSEONG_STRIDE = 21 * 28; // 588

const KOREAN_CHOSEONG = [
  "ㄱ",
  "ㄲ",
  "ㄴ",
  "ㄷ",
  "ㄸ",
  "ㄹ",
  "ㅁ",
  "ㅂ",
  "ㅃ",
  "ㅅ",
  "ㅆ",
  "ㅇ",
  "ㅈ",
  "ㅉ",
  "ㅊ",
  "ㅋ",
  "ㅌ",
  "ㅍ",
  "ㅎ",
] as const;

const KOREAN_CHOSEONG_SET = new Set<string>(KOREAN_CHOSEONG);

// Matches only precomposed Latin-1 / Latin Extended letters and Latin combining diacritics,
// deliberately excluding Hangul, CJK, and Kana so `.normalize("NFD")` NEVER decomposes them.
const LATIN_ACCENT_OR_COMBINING_RE = /[\u00C0-\u024F\u1E00-\u1EFF\u0300-\u036F]+/g;

export interface ImeKeyEventLike {
  isComposing?: boolean;
  keyCode?: number;
  nativeEvent?: {
    isComposing?: boolean;
    keyCode?: number;
  };
}

/**
 * Normalizes text for accent-insensitive search while keeping Hangul syllables in NFC.
 *
 * 1. Composes decomposed Hangul Jamo (NFD -> NFC) first via `.normalize("NFC")`.
 * 2. Strips diacritics ONLY on Latin/Vietnamese characters and Latin combining marks.
 * 3. Maps `đ`/`Đ` (and visually identical `ð`/`Ð`) to `d`, lowercases, and collapses whitespace.
 */
export function normalizeForSearch(input: string): string {
  if (!input) return "";
  const nfc = input.normalize("NFC");
  return nfc
    .replace(LATIN_ACCENT_OR_COMBINING_RE, (segment) =>
      segment.normalize("NFD").replace(/[\u0300-\u036F]/g, ""),
    )
    .replace(/[đĐðÐ]/g, "d")
    .toLowerCase()
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * Extracts Korean compatibility initial consonants (Choseong) from Hangul syllables
 * (after composing NFD Jamo to NFC), preserving non-Hangul characters in place.
 */
export function extractKoreanChoseong(input: string): string {
  if (!input) return "";
  const nfc = input.normalize("NFC");
  let out = "";
  for (const ch of nfc) {
    const code = ch.codePointAt(0);
    if (code === undefined) continue;
    if (code >= HANGUL_SYLLABLE_START && code <= HANGUL_SYLLABLE_END) {
      const choIdx = Math.floor((code - HANGUL_SYLLABLE_START) / HANGUL_CHOSEONG_STRIDE);
      out += KOREAN_CHOSEONG[choIdx];
    } else if (code >= HANGUL_L_JAMO_START && code <= HANGUL_L_JAMO_END) {
      out += KOREAN_CHOSEONG[code - HANGUL_L_JAMO_START];
    } else {
      out += ch;
    }
  }
  return out;
}

/**
 * Returns `true` when `input` contains at least one non-whitespace character and
 * every non-whitespace character is a Korean compatibility Choseong consonant.
 */
export function isKoreanChoseongQuery(input: string): boolean {
  if (!input) return false;
  let hasChoseong = false;
  for (const ch of input) {
    if (/\s/.test(ch)) continue;
    if (!KOREAN_CHOSEONG_SET.has(ch)) return false;
    hasChoseong = true;
  }
  return hasChoseong;
}

/**
 * Multi-token Unicode search matcher supporting:
 * - Accent-insensitive Vietnamese/Latin matching (`"Nguyễn Văn Minh"` matches `"nguyen minh"`)
 * - NFC-safe Korean Hangul substring matching (`"김민수 부장님"` matches `"부장"`, NFD matches NFC)
 * - Korean Choseong initial-consonant search (`"김민수"` matches `"ㄱㅁㅅ"`, `"회의실"` matches `"ㅎㅇ"`)
 */
export function matchesSearchQuery(haystack: string, rawQuery: string): boolean {
  const normQuery = normalizeForSearch(rawQuery).trim();
  if (!normQuery) return true;

  const tokens = normQuery.split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return true;

  const normHaystack = normalizeForSearch(haystack);
  let choseongHaystack: string | null = null;

  for (const token of tokens) {
    if (normHaystack.includes(token)) {
      continue;
    }
    if (isKoreanChoseongQuery(token)) {
      if (choseongHaystack === null) {
        choseongHaystack = extractKoreanChoseong(haystack.normalize("NFC"));
      }
      if (choseongHaystack.includes(token)) {
        continue;
      }
    }
    return false;
  }
  return true;
}

/**
 * Filters thread/recipient picker items using diacritic-insensitive Vietnamese/Latin
 * and Korean Hangul/Choseong search against `title` and `id`, applying any optional
 * result cap *after* filtering so deep matches are never hidden.
 */
export function filterThreadsBySearchQuery<T extends { id: string; title: string }>(
  threads: readonly T[],
  rawQuery: string,
  limit?: number,
): T[] {
  const q = rawQuery.trim();
  const matched = q
    ? threads.filter((t) => matchesSearchQuery(`${t.title ?? ""}\n${t.id ?? ""}`, q))
    : threads;
  return limit !== undefined ? matched.slice(0, limit) : [...matched];
}

/**
 * Detects an active IME composition key event (Korean/Japanese/Chinese/Vietnamese IME),
 * covering both React `SyntheticEvent` (`nativeEvent.isComposing`) and WebKit/Chromium
 * `keyCode === 229` during composition commit.
 */
export function isImeComposingEvent(e: ImeKeyEventLike | null | undefined): boolean {
  if (!e) return false;
  return Boolean(
    e.isComposing ||
      e.nativeEvent?.isComposing ||
      e.keyCode === 229 ||
      e.nativeEvent?.keyCode === 229,
  );
}

/**
 * Returns `true` when `query` looks like a Vietnamese (`+84...`, `0...`, 9-digit local),
 * Korean (`+82...`, `82...`, `010-...`, `02-...`), or general E.164 (`+<7..15 digits>`)
 * phone number query.
 */
export function looksLikeInternationalPhoneQuery(query: string): boolean {
  const trimmed = query.trim();
  if (!trimmed) return false;
  const cleaned = trimmed.replace(/[\s\-().\t]/g, "");
  if (/^\+[1-9]\d{6,14}$/.test(cleaned)) {
    return true;
  }
  return /^(?:84\d{8,10}|82\d{8,11}|0\d{8,10}|[1-9]\d{8})$/.test(cleaned);
}
