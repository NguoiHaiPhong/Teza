import test from "node:test";
import assert from "node:assert/strict";
import { EMOJI_CATEGORIES, searchEmojis } from "../src/utils/emojiCatalog.ts";

test("emojiCatalog exposes 300+ unique emojis and matches Vietnamese/English keywords", () => {
  const unique = new Set<string>();
  for (const cat of EMOJI_CATEGORIES) {
    assert.ok(cat.items.length > 0, `Category ${cat.id} should not be empty`);
    for (const item of cat.items) {
      unique.add(item.emoji);
    }
  }
  assert.ok(unique.size >= 300, `Expected >= 300 unique emojis, got ${unique.size}`);

  assert.ok(searchEmojis("cười").includes("😂"));
  assert.ok(searchEmojis("cuoi").includes("😂"));
  assert.ok(searchEmojis("cà phê").includes("☕"));
  assert.ok(searchEmojis("tim").includes("❤️"));
  assert.ok(searchEmojis("coffee").includes("☕"));
  assert.deepEqual(searchEmojis("   "), []);
});
