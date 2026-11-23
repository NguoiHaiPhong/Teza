export type TranslateSkipReason = "non_linguistic" | "same_language";

const NON_TRANSLATABLE_PLACEHOLDERS = new Set([
  "[Nhãn dán]",
  "[Hình ảnh]",
  "[Ảnh GIF]",
  "[Tin nhắn thoại]",
  "[Video]",
  "[Tin nhắn đã thu hồi]",
  "[Tệp đính kèm]",
  "[Thiệp/Danh thiếp]",
  "[Vị trí]",
  "[Cuộc gọi]",
  "[BUZZ!]",
  "BUZZ!",
]);

const SYMBOLIC_EMOTICONS = [
  ">:D<",
  "=))",
  ":))",
  ";))",
  ":-)",
  ":-(",
  ":-D",
  ":-P",
  ":'(",
  "B-)",
  "O:)",
  ":-*",
  ":)",
  ":(",
  ";)",
  ":D",
  ":P",
  ":-/",
  ":/",
  ":O",
  ":-O",
  "^^",
  "<3",
];

const UNIQUELY_VI_CHARS = new Set(
  "đĐăĂơƠưƯảẢẻẺỉỈỏỎủỦỷỶạẠẹẸịỊọỌụỤỵỴấẤầẦẩẨẫẪậẬắẮằẰẳẲẵẴặẶếẾềỀểỂễỄệỆốỐồỒổỔỗỖộỘớỚờỜởỞỡỠợỢứỨừỪửỬữỮựỰỳỲỹỸ".split(
    "",
  ),
);

const MENTION_CONTINUATION_STOPWORDS = new Set([
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

const EN_STOPWORDS = new Set([
  "the",
  "and",
  "you",
  "that",
  "have",
  "for",
  "with",
  "this",
  "from",
  "they",
  "would",
  "there",
  "their",
  "what",
  "about",
  "which",
  "when",
  "make",
  "please",
  "thanks",
  "thank",
  "hello",
  "sorry",
  "could",
  "should",
  "your",
  "been",
  "were",
  "will",
  "are",
  "was",
  "not",
  "but",
  "all",
  "how",
  "why",
  "where",
  "who",
  "does",
  "did",
  "has",
  "had",
  "into",
  "just",
  "like",
  "know",
  "take",
  "want",
  "because",
  "before",
  "after",
  "between",
  "through",
  "without",
  "during",
  "under",
  "again",
  "here",
  "very",
  "really",
  "good",
  "morning",
  "afternoon",
  "evening",
  "night",
  "today",
  "tomorrow",
  "yesterday",
  "meeting",
  "schedule",
  "update",
  "issue",
  "problem",
  "working",
  "need",
  "help",
  "send",
  "sent",
  "check",
  "email",
  "message",
  "server",
  "returned",
  "error",
  "session",
  "expired",
  "account",
  "suspended",
  "share",
  "plan",
  "moving",
  "attend",
  "review",
  "list",
  "verify",
  "confirm",
  "report",
  "result",
  "results",
  "production",
  "customer",
  "finish",
  "finished",
  "making",
  "still",
  "job",
  "congratulation",
  "congratulations",
  "together",
  "dear",
  "sir",
  "these",
  "those",
  "already",
  "status",
  "progress",
  "quality",
  "feedback",
  "request",
  "support",
  "reply",
  "team",
  "regards",
  "noted",
  "pls",
  "everyone",
  "i'm",
  "i'll",
  "i've",
  "i'd",
  "we're",
  "we'll",
  "you're",
  "you'll",
  "don't",
  "doesn't",
  "didn't",
  "can't",
  "won't",
  "isn't",
  "aren't",
  "hasn't",
  "haven't",
  "it's",
  "that's",
  "let's",
]);

const UNAMBIGUOUS_VI_WORDS = new Set([
  "không",
  "được",
  "chưa",
  "nhưng",
  "người",
  "chúng",
  "mình",
  "nhiều",
  "giúp",
  "cảm",
  "chào",
  "vậy",
  "nhé",
  "rồi",
  "đang",
  "với",
  "của",
  "từ",
  "trong",
  "ngoài",
  "trên",
  "dưới",
  "hoặc",
  "lắm",
  "quá",
  "rất",
  "một",
  "hôm",
  "ngày",
  "giờ",
  "phút",
  "tuần",
  "tháng",
  "bây",
  "lúc",
  "sáng",
  "trưa",
  "chiều",
  "tối",
  "ăn",
  "uống",
  "ngủ",
  "làm",
  "đi",
  "về",
  "đến",
  "tới",
  "gửi",
  "gọi",
  "nhắn",
  "nói",
  "hỏi",
  "biết",
  "hiểu",
  "thấy",
  "cần",
  "muốn",
  "thích",
  "phải",
  "đúng",
  "tốt",
  "đẹp",
  "mới",
  "cũ",
  "đâu",
  "thế",
  "gì",
  "nào",
  "mọi",
  "việc",
  "tiền",
  "hàng",
  "nhóm",
  "cuộc",
  "họp",
  "lỗi",
  "sửa",
  "nhận",
  "chuyển",
  "khoản",
  "vâng",
  "dạ",
  "ơi",
  "luôn",
  "sếp",
  "lịch",
  "hình",
  "ảnh",
  "bảo",
  "quản",
  "là",
  "sẽ",
  "như",
  "này",
  "chút",
  "thôi",
  "ạ",
  "bên",
  "vẫn",
  "để",
  "có",
  "vào",
  "nhờ",
  "đây",
  "đó",
  "chị",
  "bạn",
  "tôi",
  "đã",
  "thì",
  "mà",
  "ít",
  "bốn",
  "năm",
  "sáu",
  "bảy",
  "tám",
  "chín",
  "mười",
  "đêm",
  "chậm",
  "nhà",
  "máy",
  "điện",
  "thoại",
  "báo",
  "cáo",
  "trạng",
  "nguyên",
  "nhân",
  "đối",
  "sách",
  "tình",
  "khong",
  "duoc",
  "chua",
  "nhung",
  "nguoi",
  "chung",
  "minh",
  "nhieu",
  "giup",
  "kiem",
  "chuyen",
  "khoan",
  "chao",
  "hom",
  "ngay",
  "tuan",
  "thang",
  "phut",
  "chieu",
  "viec",
  "tien",
  "nhom",
  "biet",
  "hieu",
  "thay",
  "muon",
  "thich",
  "phai",
  "dung",
  "vang",
  "nhe",
  "xong",
  "gui",
  "nhan",
  "sua",
  "loi",
  "luon",
  "truoc",
  "duoi",
  "ngoai",
  "voi",
  "cua",
  "vay",
  "roi",
  "dang",
  "giay",
  "dien",
  "thoai",
]);

export function normalizeLangBase(code: string): string {
  const upper = code.trim().toUpperCase();
  if (upper === "EN" || upper === "EN-US" || upper === "EN-GB") return "EN";
  if (upper === "ZH" || upper === "ZH-HANS" || upper === "ZH-CN") return "ZH";
  if (upper === "ZH-HANT" || upper === "ZH-TW" || upper === "ZH-HK") return "ZH-HANT";
  return upper.split("-")[0] || upper;
}

export function isSameLangCode(detected: string | null | undefined, target: string): boolean {
  const det = detected?.trim();
  if (!det) return false;
  const dBase = normalizeLangBase(det);
  const tUpper = target.trim().toUpperCase();
  const tBase = normalizeLangBase(tUpper);
  if (dBase === tBase) return true;
  return tUpper === "ZH" && (dBase === "ZH" || dBase === "ZH-HANT");
}

export function isEffectivelySameText(orig: string, translated: string): boolean {
  const norm = (s: string) => s.trim().replace(/\s+/g, " ").toLowerCase();
  const tNorm = norm(translated);
  return tNorm.length === 0 || tNorm === norm(orig);
}

function stripFilePrefixOrPlaceholder(text: string): string | null {
  const trimmed = text.trim();
  if (NON_TRANSLATABLE_PLACEHOLDERS.has(trimmed)) {
    return null;
  }
  if (trimmed.slice(0, 9).toLowerCase() === "[tập tin]") {
    const nlIdx = trimmed.indexOf("\n");
    if (nlIdx < 0) return null;
    const cap = trimmed.slice(nlIdx + 1).trim();
    return cap.length > 0 ? cap : null;
  }
  return trimmed;
}

function isLetterChar(ch: string): boolean {
  return /\p{L}/u.test(ch);
}

function isUpperLetterChar(ch: string): boolean {
  return /\p{Lu}/u.test(ch);
}

function isLowerLetterChar(ch: string): boolean {
  return /\p{Ll}/u.test(ch);
}

function isTitleCaseWord(word: string): boolean {
  const chars = [...word];
  if (chars.length < 2) return false;
  if (!isUpperLetterChar(chars[0])) return false;
  return chars.slice(1).every((c) => isLowerLetterChar(c));
}

function isAcceptedMentionContinuation(core: string): boolean {
  if (!core) return false;
  const chars = [...core];
  if (chars.length === 1) {
    return core !== "I" && isUpperLetterChar(chars[0]);
  }
  return isTitleCaseWord(core) && !MENTION_CONTINUATION_STOPWORDS.has(core.toLowerCase());
}

function stripNonLinguisticTokens(text: string): string {
  const tokens = text.split(/\s+/).filter(Boolean);
  const kept: string[] = [];
  for (let i = 0; i < tokens.length; i += 1) {
    const rawTok = tokens[i];
    const trimmed = rawTok.replace(/^[()[\]<>"',;.!?]+|[()[\]<>"',;.!?]+$/g, "");
    const lower = trimmed.toLowerCase();
    if (lower.startsWith("http://") || lower.startsWith("https://") || lower.startsWith("www.")) {
      continue;
    }
    if (trimmed.startsWith("@") && trimmed.length > 1) {
      const mentionCore = trimmed.slice(1).toLowerCase();
      const rawEndsClause = /[,.:;!?)\]]$/.test(rawTok);
      if (!rawEndsClause && mentionCore !== "all" && mentionCore !== "everyone") {
        let consumed = 0;
        while (consumed < 3 && i + 1 < tokens.length) {
          const rawNext = tokens[i + 1];
          const core = rawNext.replace(/[,.:;!?)\]]+$/, "");
          if (!isAcceptedMentionContinuation(core)) break;
          i += 1;
          consumed += 1;
          if (core.length < rawNext.length) break;
        }
      }
      continue;
    }
    const atIdx = trimmed.indexOf("@");
    if (atIdx > 0 && trimmed.slice(atIdx + 1).includes(".")) {
      continue;
    }
    const exact = rawTok.trim();
    if (
      exact.toLowerCase() === "xd" ||
      exact === "T_T" ||
      exact === "ToT" ||
      SYMBOLIC_EMOTICONS.includes(exact)
    ) {
      continue;
    }
    let cleaned = rawTok;
    for (const emo of SYMBOLIC_EMOTICONS) {
      if (cleaned.includes(emo)) {
        cleaned = cleaned.split(emo).join(" ");
      }
    }
    if (cleaned.trim()) kept.push(cleaned);
  }
  return kept.join(" ");
}

function isPureModelCode(cleaned: string): boolean {
  const parts = cleaned.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  if (parts.length === 0) return false;
  let digitBearingCount = 0;
  let hasUpperAndDigit = false;
  for (const tok of parts) {
    if (/[a-z]/.test(tok)) return false;
    const hasUpper = /[A-Z]/.test(tok);
    const hasDigit = /\d/.test(tok);
    if (hasDigit) {
      digitBearingCount += 1;
    } else if ([...tok].length >= 5 || EN_STOPWORDS.has(tok.toLowerCase())) {
      return false;
    }
    if (hasUpper && hasDigit) {
      hasUpperAndDigit = true;
    }
  }
  return hasUpperAndDigit && digitBearingCount * 2 >= parts.length;
}

interface WordMeta {
  lower: string;
  isTitleCase: boolean;
  isAllCaps: boolean;
  isSingleUpperNonI: boolean;
  hasUniquelyVi: boolean;
  hasAccentedLatin: boolean;
}

function extractWordMetas(cleaned: string): WordMeta[] {
  const chars = [...cleaned];
  const words: WordMeta[] = [];
  let i = 0;
  while (i < chars.length) {
    if (!isLetterChar(chars[i])) {
      i += 1;
      continue;
    }
    const rawAlpha: string[] = [];
    let lower = "";
    let hasApostrophe = false;
    while (i < chars.length) {
      const c = chars[i];
      if (isLetterChar(c)) {
        rawAlpha.push(c);
        lower += c.toLowerCase();
        i += 1;
      } else if (
        (c === "'" || c === "\u2019") &&
        i + 1 < chars.length &&
        isLetterChar(chars[i + 1])
      ) {
        hasApostrophe = true;
        lower += "'";
        i += 1;
      } else {
        break;
      }
    }
    const isTitleCase =
      !hasApostrophe &&
      rawAlpha.length >= 2 &&
      isUpperLetterChar(rawAlpha[0]) &&
      rawAlpha.slice(1).every((c) => isLowerLetterChar(c));
    const isAllCaps =
      !hasApostrophe && rawAlpha.length >= 2 && rawAlpha.every((c) => /^[A-Z]$/.test(c));
    const isSingleUpperNonI =
      !hasApostrophe && rawAlpha.length === 1 && /^[A-Z]$/.test(rawAlpha[0]) && rawAlpha[0] !== "I";
    const hasUniquelyVi = rawAlpha.some((c) => UNIQUELY_VI_CHARS.has(c));
    const hasAccentedLatin = rawAlpha.some((c) => (c.codePointAt(0) ?? 0) > 0x7f);
    words.push({
      lower,
      isTitleCase,
      isAllCaps,
      isSingleUpperNonI,
      hasUniquelyVi,
      hasAccentedLatin,
    });
  }
  return words;
}

/**
 * Classifies whether a message should skip calling the translation API
 * because it contains no translatable text or is already in `targetLang`.
 */
export function classifySkipTranslation(
  text: string,
  targetLang: string,
): TranslateSkipReason | null {
  const nfc = text.normalize("NFC");
  const effectiveText = stripFilePrefixOrPlaceholder(nfc);
  if (effectiveText === null) {
    return "non_linguistic";
  }

  const cleaned = stripNonLinguisticTokens(effectiveText);
  let hangul = 0;
  let kana = 0;
  let han = 0;
  let cyrillic = 0;
  let thai = 0;
  let accentedLatin = 0;
  let totalLetters = 0;

  for (const ch of cleaned) {
    if (!isLetterChar(ch)) continue;
    totalLetters += 1;
    const cp = ch.codePointAt(0) ?? 0;
    if (
      (cp >= 0xac00 && cp <= 0xd7af) ||
      (cp >= 0x1100 && cp <= 0x11ff) ||
      (cp >= 0x3130 && cp <= 0x318f)
    ) {
      hangul += 1;
    } else if ((cp >= 0x3040 && cp <= 0x309f) || (cp >= 0x30a0 && cp <= 0x30ff)) {
      kana += 1;
    } else if ((cp >= 0x4e00 && cp <= 0x9fff) || (cp >= 0x3400 && cp <= 0x4dbf)) {
      han += 1;
    } else if (cp >= 0x0400 && cp <= 0x04ff) {
      cyrillic += 1;
    } else if (cp >= 0x0e00 && cp <= 0x0e7f) {
      thai += 1;
    } else if (cp > 0x7f) {
      accentedLatin += 1;
    }
  }

  if (totalLetters < 2) {
    return "non_linguistic";
  }
  if (
    hangul === 0 &&
    kana === 0 &&
    han === 0 &&
    cyrillic === 0 &&
    thai === 0 &&
    accentedLatin === 0 &&
    isPureModelCode(cleaned)
  ) {
    return "non_linguistic";
  }

  const words = extractWordMetas(cleaned);
  const viStrongWords = words.filter(
    (w) => UNAMBIGUOUS_VI_WORDS.has(w.lower) || (w.hasUniquelyVi && !w.isTitleCase),
  ).length;
  const lowercaseUniquelyViWords = words.filter(
    (w) => w.hasUniquelyVi && !w.isTitleCase,
  ).length;
  const enStrongWords = words.filter((w) => EN_STOPWORDS.has(w.lower)).length;
  const nonTitleAccentedLatin = words.filter(
    (w) => !w.isTitleCase && w.hasAccentedLatin,
  ).length;
  const naturalWords = words.filter((w) => !w.isAllCaps && !w.isSingleUpperNonI).length;

  let maxEnClauseStrong = 0;
  let curEnRun = 0;
  for (const w of words) {
    if (UNAMBIGUOUS_VI_WORDS.has(w.lower) || (w.hasUniquelyVi && !w.isTitleCase)) {
      curEnRun = 0;
    } else if (EN_STOPWORDS.has(w.lower)) {
      curEnRun += 1;
      if (curEnRun > maxEnClauseStrong) {
        maxEnClauseStrong = curEnRun;
      }
    }
  }

  const targetUpper = targetLang.trim().toUpperCase();
  const targetBase = normalizeLangBase(targetUpper);

  if (targetBase === "VI") {
    if (hangul === 0 && kana === 0 && han === 0 && cyrillic === 0 && thai === 0) {
      if (
        maxEnClauseStrong >= 4 ||
        (enStrongWords >= 3 && enStrongWords >= viStrongWords) ||
        (enStrongWords >= 2 && viStrongWords === 0)
      ) {
        return null;
      }
      if (
        viStrongWords >= 2 ||
        (viStrongWords >= 1 && words.length <= 4 && enStrongWords === 0) ||
        (lowercaseUniquelyViWords >= 1 && enStrongWords <= 1)
      ) {
        return "same_language";
      }
    }
  } else if (targetBase === "KO") {
    if (
      hangul >= 1 &&
      kana === 0 &&
      cyrillic === 0 &&
      thai === 0 &&
      viStrongWords === 0 &&
      maxEnClauseStrong < 4 &&
      enStrongWords < 3 &&
      (hangul >= 3 || hangul * 2 >= totalLetters)
    ) {
      return "same_language";
    }
  } else if (targetBase === "JA") {
    if (
      kana >= 1 &&
      hangul === 0 &&
      cyrillic === 0 &&
      thai === 0 &&
      viStrongWords === 0 &&
      maxEnClauseStrong < 4 &&
      enStrongWords < 3 &&
      (kana >= 2 || (kana + han) * 2 >= totalLetters)
    ) {
      return "same_language";
    }
  } else if (targetUpper === "ZH") {
    if (
      han >= 2 &&
      kana === 0 &&
      hangul === 0 &&
      cyrillic === 0 &&
      thai === 0 &&
      viStrongWords === 0 &&
      maxEnClauseStrong < 4 &&
      enStrongWords < 3 &&
      (han >= 4 || han * 5 >= totalLetters * 3)
    ) {
      return "same_language";
    }
  } else if (targetBase === "RU") {
    if (
      cyrillic >= 2 &&
      hangul === 0 &&
      kana === 0 &&
      han === 0 &&
      thai === 0 &&
      viStrongWords === 0 &&
      enStrongWords < 3 &&
      cyrillic * 5 >= totalLetters * 3
    ) {
      return "same_language";
    }
  } else if (targetBase === "TH") {
    if (
      thai >= 2 &&
      hangul === 0 &&
      kana === 0 &&
      han === 0 &&
      cyrillic === 0 &&
      viStrongWords === 0 &&
      enStrongWords < 3 &&
      thai * 5 >= totalLetters * 3
    ) {
      return "same_language";
    }
  } else if (targetBase === "EN") {
    if (
      hangul === 0 &&
      kana === 0 &&
      han === 0 &&
      cyrillic === 0 &&
      thai === 0 &&
      viStrongWords === 0 &&
      nonTitleAccentedLatin === 0 &&
      ((enStrongWords >= 2 && enStrongWords * 3 >= Math.max(naturalWords, 1)) ||
        (naturalWords <= 3 && enStrongWords >= 1))
    ) {
      return "same_language";
    }
  }

  return null;
}
