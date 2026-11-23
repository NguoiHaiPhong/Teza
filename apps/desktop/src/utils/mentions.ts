export interface MentionCandidate {
  id: string;
  name: string;
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

function stripAccents(input: string): string {
  return input
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/đ/g, "d")
    .replace(/Đ/g, "D")
    .toLowerCase();
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
    .filter((c) => c.nameNfc.length > 0 && c.nameLower !== "all" && c.nameLower !== "everyone")
    .sort((a, b) => b.nameNfc.length - a.nameNfc.length);

  const out: MentionToken[] = [];
  let cursor = 0;

  while (cursor < text.length) {
    const atPos = text.indexOf("@", cursor);
    if (atPos < 0) {
      pushText(out, text.slice(cursor));
      break;
    }

    if (atPos > 0 && !/[\s([{"',;:]/u.test(text[atPos - 1])) {
      pushText(out, text.slice(cursor, atPos + 1));
      cursor = atPos + 1;
      continue;
    }

    if (atPos > cursor) {
      pushText(out, text.slice(cursor, atPos));
    }

    const afterAt = text.slice(atPos + 1);
    const allMatch = afterAt.match(/^(all|everyone)(?=[^\p{L}\p{N}_]|$)/iu);
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
