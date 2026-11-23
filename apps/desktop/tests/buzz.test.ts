import test from "node:test";
import assert from "node:assert/strict";
import {
  BUZZ_STREAK_WINDOW_MS,
  type BuzzStreakMap,
  isBuzzMessage,
  recordIncomingBuzz,
  resetBuzzStreak,
} from "../src/utils/buzz.ts";

test("isBuzzMessage detects active BUZZ markers and ignores recalled or normal texts", () => {
  assert.equal(isBuzzMessage({ is_recalled: false, text: "⚡ BUZZ!" }), true);
  assert.equal(isBuzzMessage({ is_recalled: false, text: "  ⚡   buzz  " }), true);
  assert.equal(isBuzzMessage({ is_recalled: true, text: "⚡ BUZZ!" }), false);
  assert.equal(isBuzzMessage({ is_recalled: false, text: "hello world" }), false);
});

test("recordIncomingBuzz counts consecutive streak, triggers full-screen at 3rd, and deduplicates", () => {
  const map: BuzzStreakMap = new Map();
  const base = 1_700_000_000_000;

  assert.deepEqual(recordIncomingBuzz(map, "t1", "m1", base), {
    count: 1,
    isDuplicate: false,
    shouldShakeScreen: false,
  });

  // Duplicate message id must not advance streak
  assert.deepEqual(recordIncomingBuzz(map, "t1", "m1", base + 500), {
    count: 1,
    isDuplicate: true,
    shouldShakeScreen: false,
  });

  assert.deepEqual(recordIncomingBuzz(map, "t1", "m2", base + 10_000), {
    count: 2,
    isDuplicate: false,
    shouldShakeScreen: false,
  });

  // 3rd consecutive BUZZ triggers full-screen shake
  assert.deepEqual(recordIncomingBuzz(map, "t1", "m3", base + 20_000), {
    count: 3,
    isDuplicate: false,
    shouldShakeScreen: true,
  });

  // 4th consecutive BUZZ also triggers full-screen shake
  assert.deepEqual(recordIncomingBuzz(map, "t1", "m4", base + 30_000), {
    count: 4,
    isDuplicate: false,
    shouldShakeScreen: true,
  });
});

test("buzz streaks isolate by thread, reset on normal/outgoing message, and expire after window", () => {
  const map: BuzzStreakMap = new Map();
  const base = 1_700_000_000_000;

  recordIncomingBuzz(map, "t1", "a1", base);
  recordIncomingBuzz(map, "t1", "a2", base + 1000);

  // Separate thread starts independently at 1
  assert.deepEqual(recordIncomingBuzz(map, "t2", "b1", base + 2000), {
    count: 1,
    isDuplicate: false,
    shouldShakeScreen: false,
  });

  // Reset t1 on normal message -> next BUZZ starts at 1
  resetBuzzStreak(map, "t1");
  assert.deepEqual(recordIncomingBuzz(map, "t1", "a3", base + 3000), {
    count: 1,
    isDuplicate: false,
    shouldShakeScreen: false,
  });

  // Expiring past BUZZ_STREAK_WINDOW_MS resets to 1
  assert.deepEqual(recordIncomingBuzz(map, "t2", "b2", base + 2000 + BUZZ_STREAK_WINDOW_MS + 1), {
    count: 1,
    isDuplicate: false,
    shouldShakeScreen: false,
  });
});
