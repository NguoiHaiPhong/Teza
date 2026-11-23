export type EmojiStyle = "retro" | "system";

const EMOJI_STYLE_KEY = "teza.emojiStyle";

export function loadEmojiStyle(): EmojiStyle {
  if (typeof localStorage === "undefined") return "retro";
  try {
    return localStorage.getItem(EMOJI_STYLE_KEY) === "system" ? "system" : "retro";
  } catch {
    return "retro";
  }
}

export function saveEmojiStyle(style: EmojiStyle): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(EMOJI_STYLE_KEY, style);
  } catch {
    // Ignore storage quota restrictions
  }
}

export type RetroEmoticonId =
  | "smile"
  | "big_grin"
  | "laugh"
  | "rofl"
  | "wink"
  | "blush"
  | "love"
  | "kiss"
  | "tongue"
  | "cool"
  | "angel"
  | "devil"
  | "sad"
  | "cry"
  | "angry"
  | "surprised"
  | "confused"
  | "sweat"
  | "sleepy"
  | "sick"
  | "shh"
  | "smug"
  | "eyebrow"
  | "nerd"
  | "hug"
  | "clap"
  | "thumbs_up"
  | "thumbs_down"
  | "heart"
  | "broken_heart"
  | "rose"
  | "coffee"
  | "party"
  | "star"
  | "fire"
  | "pray";

export interface RetroEmoticonDef {
  id: RetroEmoticonId;
  emoji: string;
  aliases: readonly string[];
  primaryShortcut: string;
  shortcuts: readonly string[];
  label: string;
  keywords: string;
}

export const RETRO_EMOTICONS: readonly RetroEmoticonDef[] = [
  {
    id: "smile",
    emoji: "😀",
    aliases: ["🙂"],
    primaryShortcut: ":)",
    shortcuts: [":-)", ":)"],
    label: "Cười tươi",
    keywords: "cuoi vui grin happy smile",
  },
  {
    id: "big_grin",
    emoji: "😃",
    aliases: ["😄"],
    primaryShortcut: ":D",
    shortcuts: [":-D", ":D"],
    label: "Cười toe toét",
    keywords: "cuoi toe toet big grin happy haha",
  },
  {
    id: "laugh",
    emoji: "😆",
    aliases: ["😁"],
    primaryShortcut: ":))",
    shortcuts: [":-)))", ":)))", ":-))", ":))"],
    label: "Cười khoái chí",
    keywords: "cuoi haha hehe lol khoai chi laugh",
  },
  {
    id: "rofl",
    emoji: "🤣",
    aliases: ["😂"],
    primaryShortcut: "=))",
    shortcuts: ["=)))", "=))"],
    label: "Cười lăn lộn",
    keywords: "cuoi lan lon rofl lmao haha",
  },
  {
    id: "wink",
    emoji: "😉",
    aliases: [],
    primaryShortcut: ";)",
    shortcuts: [";-)", ";)"],
    label: "Nháy mắt",
    keywords: "nhay mat wink",
  },
  {
    id: "blush",
    emoji: "😊",
    aliases: ["☺️", "☺"],
    primaryShortcut: ':")',
    shortcuts: [':">', ':")'],
    label: "Ngượng ngùng",
    keywords: "ngai nguong do ma blush shy",
  },
  {
    id: "love",
    emoji: "😍",
    aliases: ["🥰"],
    primaryShortcut: ":X",
    shortcuts: [":-X", ":-x", ":X", ":x"],
    label: "Yêu thích",
    keywords: "yeu thuong mat tim crush love",
  },
  {
    id: "kiss",
    emoji: "😘",
    aliases: ["😗", "😚"],
    primaryShortcut: ":-*",
    shortcuts: [":-*", ":*"],
    label: "Hôn gió",
    keywords: "hon gio chụt kiss",
  },
  {
    id: "tongue",
    emoji: "😛",
    aliases: ["😜", "😝", "🤪"],
    primaryShortcut: ":P",
    shortcuts: [":-P", ":P", ":-p", ":p"],
    label: "Lè lưỡi trêu",
    keywords: "le the luoi troll treu tongue",
  },
  {
    id: "cool",
    emoji: "😎",
    aliases: [],
    primaryShortcut: "B-)",
    shortcuts: ["B-)", "B)"],
    label: "Ngầu / Kính râm",
    keywords: "ngau kinh ram cool sunglasses",
  },
  {
    id: "angel",
    emoji: "😇",
    aliases: [],
    primaryShortcut: "O:-)",
    shortcuts: ["O:-)", "O:)", "0:-)", "0:)"],
    label: "Thiên thần",
    keywords: "thien than ngoan hien angel innocent",
  },
  {
    id: "devil",
    emoji: "😈",
    aliases: ["👿"],
    primaryShortcut: ">:)",
    shortcuts: [">:-)", ">:)"],
    label: "Cười nham hiểm",
    keywords: "ac quy nham hiem troll devil",
  },
  {
    id: "sad",
    emoji: "😢",
    aliases: ["😞", "😔"],
    primaryShortcut: ":(",
    shortcuts: [":-(", ":("],
    label: "Buồn",
    keywords: "buon rầu cry sad",
  },
  {
    id: "cry",
    emoji: "😭",
    aliases: [],
    primaryShortcut: ":((",
    shortcuts: [":-(((", ":(((", ":-((", ":(("],
    label: "Khóc nức nở",
    keywords: "khoc tu tu nuc no sob cry",
  },
  {
    id: "angry",
    emoji: "😠",
    aliases: ["😡", "🤬"],
    primaryShortcut: "X(",
    shortcuts: ["X-(", "x-(", ":-@", "X(", "x(", ">:("],
    label: "Tức giận",
    keywords: "tuc gian cau angry mad",
  },
  {
    id: "surprised",
    emoji: "😮",
    aliases: ["😯", "😲"],
    primaryShortcut: ":-O",
    shortcuts: [":-O", ":-o", ":O", ":o"],
    label: "Ngạc nhiên",
    keywords: "ngac nhien bat ngo wow surprised",
  },
  {
    id: "confused",
    emoji: "🤔",
    aliases: ["😕"],
    primaryShortcut: ":-?",
    shortcuts: [":-?", ":-/", ":/"],
    label: "Bối rối / Suy nghĩ",
    keywords: "suy nghi boi roi thac mac hmm confused thinking",
  },
  {
    id: "sweat",
    emoji: "😅",
    aliases: ["😰", "😓"],
    primaryShortcut: "#:-S",
    shortcuts: ["#:-S", "#:-s", ":-S", ":-s"],
    label: "Toát mồ hôi / Hú hồn",
    keywords: "toat mo hoi lo lang hu hon phew sweat",
  },
  {
    id: "sleepy",
    emoji: "😴",
    aliases: ["🥱"],
    primaryShortcut: "I-)",
    shortcuts: ["I-)", "|-)", ":-zz"],
    label: "Buồn ngủ",
    keywords: "buon ngu ngap sleep tired zzz",
  },
  {
    id: "sick",
    emoji: "🤢",
    aliases: ["🤮", "🤒"],
    primaryShortcut: ":-&",
    shortcuts: [":-&", ":&"],
    label: "Chóng mặt / Ốm",
    keywords: "om met buon non sick nauseous",
  },
  {
    id: "shh",
    emoji: "🤫",
    aliases: ["🤐"],
    primaryShortcut: ":-$",
    shortcuts: [":-$", ":$"],
    label: "Suỵt / Giữ bí mật",
    keywords: "suyt im lang bi mat shh quiet",
  },
  {
    id: "smug",
    emoji: "😏",
    aliases: [],
    primaryShortcut: ":-j",
    shortcuts: [":-j", ":->", ":>"],
    label: "Nhếch mép / Tự đắc",
    keywords: "nhech mep tu dac khu khu smug smirk",
  },
  {
    id: "eyebrow",
    emoji: "🤨",
    aliases: ["🙄"],
    primaryShortcut: "/:)",
    shortcuts: ["/:-)", "/:)"],
    label: "Nghi ngờ / Nhướn mày",
    keywords: "nghi ngo nhuon may liec eyebrow",
  },
  {
    id: "nerd",
    emoji: "🤓",
    aliases: [],
    primaryShortcut: ":-B",
    shortcuts: [":-B", ":B"],
    label: "Mọt sách / Kính cận",
    keywords: "mot sach kinh can nerd glasses",
  },
  {
    id: "hug",
    emoji: "🤗",
    aliases: [],
    primaryShortcut: ">:D<",
    shortcuts: [">:D<"],
    label: "Ôm thắm thiết",
    keywords: "om thuong hug",
  },
  {
    id: "clap",
    emoji: "👏",
    aliases: [],
    primaryShortcut: "=D>",
    shortcuts: ["=D>"],
    label: "Vỗ tay",
    keywords: "vo tay hoan ho bravo clap",
  },
  {
    id: "thumbs_up",
    emoji: "👍",
    aliases: [],
    primaryShortcut: "(y)",
    shortcuts: [":-bd", "(y)", "(Y)"],
    label: "Thích / Tuyệt vời",
    keywords: "thich ok like tuyet voi thumbs up",
  },
  {
    id: "thumbs_down",
    emoji: "👎",
    aliases: [],
    primaryShortcut: "(n)",
    shortcuts: [":-q", "(n)", "(N)"],
    label: "Không thích",
    keywords: "khong thich che dislike thumbs down",
  },
  {
    id: "heart",
    emoji: "❤️",
    aliases: ["❤", "♥️", "♥", "💖"],
    primaryShortcut: "<3",
    shortcuts: ["<3"],
    label: "Trái tim",
    keywords: "trai tim yeu love heart",
  },
  {
    id: "broken_heart",
    emoji: "💔",
    aliases: [],
    primaryShortcut: "</3",
    shortcuts: ["</3", "=(("],
    label: "Tan vỡ",
    keywords: "tan vo dau long broken heart",
  },
  {
    id: "rose",
    emoji: "🌹",
    aliases: [],
    primaryShortcut: "@};-",
    shortcuts: ["@};-"],
    label: "Hoa hồng",
    keywords: "hoa hong tang qua rose flower",
  },
  {
    id: "coffee",
    emoji: "☕",
    aliases: ["☕️"],
    primaryShortcut: "~O)",
    shortcuts: ["~O)"],
    label: "Cà phê",
    keywords: "ca phe tra coffee tea",
  },
  {
    id: "party",
    emoji: "🥳",
    aliases: ["🎉"],
    primaryShortcut: "<:-P",
    shortcuts: ["<:-P", "<:-p"],
    label: "Ăn mừng / Tiệc tùng",
    keywords: "an mung tiec chuc mung party celebrate",
  },
  {
    id: "star",
    emoji: "⭐",
    aliases: ["🌟", "🤩"],
    primaryShortcut: "(*)",
    shortcuts: ["(*)"],
    label: "Ngôi sao",
    keywords: "ngoi sao xuat sac star",
  },
  {
    id: "fire",
    emoji: "🔥",
    aliases: [],
    primaryShortcut: ":fire:",
    shortcuts: [":fire:"],
    label: "Cháy / Rực lửa",
    keywords: "chay lua hot fire lit",
  },
  {
    id: "pray",
    emoji: "🙏",
    aliases: [],
    primaryShortcut: "[-O<",
    shortcuts: ["[-O<"],
    label: "Chắp tay / Cảm ơn",
    keywords: "chap tay cam on xin loi pray please thank",
  },
];

function foldText(input: string): string {
  return input
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/đ/g, "d")
    .replace(/Đ/g, "D")
    .toLowerCase();
}

function stripVariationSelector(input: string): string {
  return input.replace(/\uFE0F/g, "");
}

const EMOJI_TO_RETRO = new Map<string, RetroEmoticonDef>();
const EMOJI_MATCHES_SORTED: { glyph: string; def: RetroEmoticonDef }[] = [];
const SHORTCUT_MATCHES_SORTED: { shortcut: string; def: RetroEmoticonDef }[] = [];

for (const def of RETRO_EMOTICONS) {
  const allGlyphs = [def.emoji, ...def.aliases];
  for (const glyph of allGlyphs) {
    if (!EMOJI_TO_RETRO.has(glyph)) {
      EMOJI_TO_RETRO.set(glyph, def);
    }
    const stripped = stripVariationSelector(glyph);
    if (stripped && !EMOJI_TO_RETRO.has(stripped)) {
      EMOJI_TO_RETRO.set(stripped, def);
    }
    EMOJI_MATCHES_SORTED.push({ glyph, def });
  }
  for (const shortcut of def.shortcuts) {
    SHORTCUT_MATCHES_SORTED.push({ shortcut, def });
  }
}

EMOJI_MATCHES_SORTED.sort((a, b) => b.glyph.length - a.glyph.length);
SHORTCUT_MATCHES_SORTED.sort((a, b) => b.shortcut.length - a.shortcut.length);

export function findRetroEmoticonByEmoji(emoji: string): RetroEmoticonDef | undefined {
  return EMOJI_TO_RETRO.get(emoji) ?? EMOJI_TO_RETRO.get(stripVariationSelector(emoji));
}

export function searchRetroEmoticons(query: string): RetroEmoticonDef[] {
  const q = foldText(query.trim());
  if (!q) return [...RETRO_EMOTICONS];
  const rawLower = query.trim().toLowerCase();
  return RETRO_EMOTICONS.filter((def) => {
    if (def.emoji === query.trim()) return true;
    if (def.shortcuts.some((s) => s.toLowerCase().includes(rawLower))) return true;
    return foldText(`${def.label} ${def.keywords}`).includes(q);
  });
}

const URL_SEGMENT_RE = /(https?:\/\/\S+)/gi;
const LEFT_BOUNDARY_RE = /[\s(["'\u201C\u2018]/;
const RIGHT_BOUNDARY_RE = /[\s,.!?;)\]\u201D\u2019"']/;

function matchShortcutAt(seg: string, index: number): { shortcut: string; def: RetroEmoticonDef } | null {
  if (index > 0 && !LEFT_BOUNDARY_RE.test(seg[index - 1])) {
    return null;
  }
  for (const item of SHORTCUT_MATCHES_SORTED) {
    if (!seg.startsWith(item.shortcut, index)) continue;
    const afterIdx = index + item.shortcut.length;
    if (afterIdx < seg.length && !RIGHT_BOUNDARY_RE.test(seg[afterIdx])) {
      continue;
    }
    return item;
  }
  return null;
}

export function convertRetroShortcutsToEmoji(text: string): string {
  if (!text) return text;
  const segments = text.split(URL_SEGMENT_RE);
  return segments
    .map((seg, idx) => {
      if (idx % 2 === 1) return seg;
      let out = "";
      let i = 0;
      while (i < seg.length) {
        const matched = matchShortcutAt(seg, i);
        if (matched) {
          out += matched.def.emoji;
          i += matched.shortcut.length;
        } else {
          out += seg[i];
          i += 1;
        }
      }
      return out;
    })
    .join("");
}

export type RetroTextToken =
  | { kind: "text"; text: string }
  | { kind: "emoticon"; emoticon: RetroEmoticonDef; raw: string };

export function tokenizeRetroEmoticons(text: string): RetroTextToken[] {
  if (!text) return [];
  const tokens: RetroTextToken[] = [];
  const pushText = (chunk: string) => {
    if (!chunk) return;
    const last = tokens[tokens.length - 1];
    if (last && last.kind === "text") {
      last.text += chunk;
    } else {
      tokens.push({ kind: "text", text: chunk });
    }
  };

  const segments = text.split(URL_SEGMENT_RE);
  for (let sIdx = 0; sIdx < segments.length; sIdx += 1) {
    const seg = segments[sIdx];
    if (!seg) continue;
    if (sIdx % 2 === 1) {
      pushText(seg);
      continue;
    }

    let i = 0;
    let plainStart = 0;
    while (i < seg.length) {
      let matchedEmoji: { glyph: string; def: RetroEmoticonDef } | null = null;
      for (const candidate of EMOJI_MATCHES_SORTED) {
        if (seg.startsWith(candidate.glyph, i)) {
          matchedEmoji = candidate;
          break;
        }
      }

      if (matchedEmoji) {
        if (i > plainStart) pushText(seg.slice(plainStart, i));
        let consumeLen = matchedEmoji.glyph.length;
        if (seg[i + consumeLen] === "\uFE0F") {
          consumeLen += 1;
        }
        tokens.push({
          kind: "emoticon",
          emoticon: matchedEmoji.def,
          raw: seg.slice(i, i + consumeLen),
        });
        i += consumeLen;
        plainStart = i;
        continue;
      }

      const matchedShortcut = matchShortcutAt(seg, i);
      if (matchedShortcut) {
        if (i > plainStart) pushText(seg.slice(plainStart, i));
        tokens.push({
          kind: "emoticon",
          emoticon: matchedShortcut.def,
          raw: matchedShortcut.shortcut,
        });
        i += matchedShortcut.shortcut.length;
        plainStart = i;
        continue;
      }

      i += 1;
    }
    if (plainStart < seg.length) {
      pushText(seg.slice(plainStart));
    }
  }

  return tokens;
}

export function isPureRetroEmoticonMessage(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;
  const tokens = tokenizeRetroEmoticons(trimmed);
  let emoticonCount = 0;
  for (const tok of tokens) {
    if (tok.kind === "emoticon") {
      emoticonCount += 1;
    } else if (tok.text.trim()) {
      return false;
    }
  }
  return emoticonCount >= 1 && emoticonCount <= 3;
}
