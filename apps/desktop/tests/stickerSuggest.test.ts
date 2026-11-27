import test from "node:test";
import assert from "node:assert/strict";
import type { StickerItem } from "../src/types.ts";
import {
  extractStickerSuggestQuery,
  StickerSuggestCache,
} from "../src/utils/stickerSuggest.ts";

function makeSticker(id: number, cateId = 100): StickerItem {
  return {
    id,
    cate_id: cateId,
    sticker_type: 7,
    url: `https://example.invalid/sticker_${id}.webp`,
    category: "Gợi ý",
  };
}

test("extractStickerSuggestQuery accepts short Vietnamese, English, Korean words and collapses spaces", () => {
  assert.equal(extractStickerSuggestQuery("ok"), "ok");
  assert.equal(extractStickerSuggestQuery("  cảm   ơn  "), "cảm ơn");
  assert.equal(extractStickerSuggestQuery("ngủ ngon nhé"), "ngủ ngon nhé");
  assert.equal(extractStickerSuggestQuery("감사합니다"), "감사합니다");
  assert.equal(extractStickerSuggestQuery("ㅋㅋ"), "ㅋㅋ");
  assert.equal(extractStickerSuggestQuery("네"), "네");
});

test("extractStickerSuggestQuery maps single emoji triggers including variation selectors", () => {
  assert.equal(extractStickerSuggestQuery("😂"), "haha");
  assert.equal(extractStickerSuggestQuery("❤️"), "love");
  assert.equal(extractStickerSuggestQuery("❤"), "love");
  assert.equal(extractStickerSuggestQuery("👍"), "ok");
  assert.equal(extractStickerSuggestQuery("🙏"), "thanks");
  assert.equal(extractStickerSuggestQuery("😴"), "sleep");
});

test("extractStickerSuggestQuery suppresses noise, long text, commands, URLs, numbers, and disabled/overlay states", () => {
  assert.equal(extractStickerSuggestQuery(""), null);
  assert.equal(extractStickerSuggestQuery("   "), null);
  assert.equal(extractStickerSuggestQuery("a"), null);
  assert.equal(extractStickerSuggestQuery("123456"), null);
  assert.equal(extractStickerSuggestQuery("!!!???"), null);
  assert.equal(extractStickerSuggestQuery("@all"), null);
  assert.equal(extractStickerSuggestQuery("/help"), null);
  assert.equal(extractStickerSuggestQuery("#tag"), null);
  assert.equal(extractStickerSuggestQuery("https://example.com"), null);
  assert.equal(extractStickerSuggestQuery("user@company.com"), null);
  assert.equal(extractStickerSuggestQuery("line1\nline2"), null);
  assert.equal(extractStickerSuggestQuery("one two three four five"), null);
  assert.equal(extractStickerSuggestQuery("tin nhắn dài quá hai mươi bốn ký tự"), null);

  assert.equal(extractStickerSuggestQuery("ok", { enabled: false }), null);
  assert.equal(extractStickerSuggestQuery("ok", { hasPendingAttachment: true }), null);
  assert.equal(extractStickerSuggestQuery("ok", { mentionOpen: true }), null);
  assert.equal(extractStickerSuggestQuery("ok", { emojiOpen: true }), null);
});

test("StickerSuggestCache caches case-insensitively, respects TTLs, and evicts LRU entries", () => {
  const cache = new StickerSuggestCache(2, 10_000, 2_000);
  const s1 = [makeSticker(1)];
  const s2 = [makeSticker(2)];
  const s3 = [makeSticker(3)];

  cache.set("  Haha  ", s1, 1_000);
  assert.deepEqual(cache.get("haha", 1_500), s1);

  cache.set("empty", [], 1_000);
  assert.deepEqual(cache.get("empty", 2_500), []);
  // Empty result expires after 2_000ms
  assert.equal(cache.get("empty", 3_100), undefined);

  // Hit result still valid up to 11_000ms, refresh LRU
  assert.deepEqual(cache.get("haha", 5_000), s1);
  cache.set("ok", s2, 5_000);
  // Access "haha" so "ok" becomes oldest
  assert.deepEqual(cache.get("haha", 5_500), s1);
  // Adding 3rd entry evicts "ok"
  cache.set("love", s3, 6_000);
  assert.equal(cache.get("ok", 6_100), undefined);
  assert.deepEqual(cache.get("haha", 6_100), s1);
  assert.deepEqual(cache.get("love", 6_100), s3);

  // Hit result expires after TTL
  assert.equal(cache.get("haha", 11_500), undefined);
});
