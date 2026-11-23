import test from "node:test";
import assert from "node:assert/strict";
import {
  convertRetroShortcutsToEmoji,
  findRetroEmoticonByEmoji,
  isPureRetroEmoticonMessage,
  RETRO_EMOTICONS,
  searchRetroEmoticons,
  tokenizeRetroEmoticons,
} from "../src/utils/retroEmoticons.ts";

test("RETRO_EMOTICONS catalog has 36 unique entries with valid shortcuts and labels", () => {
  assert.equal(RETRO_EMOTICONS.length, 36);
  const ids = new Set(RETRO_EMOTICONS.map((e) => e.id));
  assert.equal(ids.size, 36);
  for (const item of RETRO_EMOTICONS) {
    assert.ok(item.emoji.length > 0);
    assert.ok(item.primaryShortcut.length > 0);
    assert.ok(item.label.trim().length > 0);
  }
});

test("findRetroEmoticonByEmoji resolves primary, alias, and variation selector variants", () => {
  assert.equal(findRetroEmoticonByEmoji("😂")?.id, "rofl");
  assert.equal(findRetroEmoticonByEmoji("🤣")?.id, "rofl");
  assert.equal(findRetroEmoticonByEmoji("❤️")?.id, "heart");
  assert.equal(findRetroEmoticonByEmoji("❤")?.id, "heart");
  assert.equal(findRetroEmoticonByEmoji("☕️")?.id, "coffee");
  assert.equal(findRetroEmoticonByEmoji("🚀"), undefined);
});

test("convertRetroShortcutsToEmoji uses longest-first matching and preserves URLs/timestamps", () => {
  assert.equal(convertRetroShortcutsToEmoji(":)) haha =))"), "😆 haha 🤣");
  assert.equal(convertRetroShortcutsToEmoji("vui quá =))) :)))"), "vui quá 🤣 😆");
  assert.equal(convertRetroShortcutsToEmoji(":(("), "😭");
  assert.equal(convertRetroShortcutsToEmoji(":("), "😢");
  assert.equal(convertRetroShortcutsToEmoji(">:D< >:)"), "🤗 😈");
  assert.equal(convertRetroShortcutsToEmoji("</3 <3"), "💔 ❤️");
  assert.equal(convertRetroShortcutsToEmoji("Cảm ơn :-*, @};-!"), "Cảm ơn 😘, 🌹!");

  // Must NEVER corrupt URLs, clocks, paths, or host:port strings
  assert.equal(
    convertRetroShortcutsToEmoji("Xem https://example.com/a:)/b lúc 10:30 ở C:\\dir\\foo:bar nhé :)"),
    "Xem https://example.com/a:)/b lúc 10:30 ở C:\\dir\\foo:bar nhé 😀",
  );
});

test("tokenizeRetroEmoticons parses mixed unicode emojis, ASCII shortcuts, URLs, and text", () => {
  const tokens = tokenizeRetroEmoticons("Chào bạn ;) xem https://example.com/:) nhé ❤️");
  assert.deepEqual(
    tokens.map((t) => (t.kind === "text" ? t.text : `#${t.emoticon.id}`)),
    ["Chào bạn ", "#wink", " xem https://example.com/:) nhé ", "#heart"],
  );
});

test("isPureRetroEmoticonMessage matches 1-3 standalone emoticons only", () => {
  assert.equal(isPureRetroEmoticonMessage("=))"), true);
  assert.equal(isPureRetroEmoticonMessage("🤣 😄"), true);
  assert.equal(isPureRetroEmoticonMessage("😀😄😉"), true);
  assert.equal(isPureRetroEmoticonMessage("😀 😄 😉 😍"), false);
  assert.equal(isPureRetroEmoticonMessage("haha =))"), false);
  assert.equal(isPureRetroEmoticonMessage("   "), false);
});

test("searchRetroEmoticons searches Vietnamese labels, keywords, and ASCII shortcuts", () => {
  assert.ok(searchRetroEmoticons("lan lon").some((e) => e.id === "rofl"));
  assert.ok(searchRetroEmoticons("@};-").some((e) => e.id === "rose"));
  assert.ok(searchRetroEmoticons("khoc").some((e) => e.id === "cry"));
});
