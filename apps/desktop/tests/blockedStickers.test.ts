import test from "node:test";
import assert from "node:assert/strict";
import type { MessageDto } from "../src/types.ts";
import { findFirstUnreadMsgId } from "../src/utils/threadState.ts";
import {
  addBlockedStickerRule,
  BLOCKED_STICKERS_STORAGE_KEY,
  buildBlockedStickerRuleFromMessage,
  DEFAULT_BLOCKED_STICKERS_CONFIG,
  extractStickerIdentity,
  isStickerMsgType,
  loadBlockedStickersConfig,
  matchBlockedStickerRule,
  MAX_BLOCKED_STICKER_RULES,
  normalizeBlockedStickersConfig,
  normalizeStickerFallbackUrl,
  removeBlockedStickerRule,
  saveBlockedStickersConfig,
  setBlockedStickerDisplayMode,
} from "../src/utils/blockedStickers.ts";

function makeStickerMsg(overrides: Partial<MessageDto> = {}): MessageDto {
  return {
    msg_id: "m-stk-1",
    thread_id: "thread-a",
    sender_id: "u-peer",
    sender_name: "Trần Minh",
    text: "[Nhãn dán]",
    msg_type: "chat.sticker",
    ts: 1_700_000_000_000,
    is_outgoing: false,
    is_recalled: false,
    is_pinned: false,
    quote: null,
    reactions: [],
    attachment_url: "https://stickers.example.invalid/api/emoticon/sticker/webpc?eid=2005&size=130",
    attachment_name: "sticker_1002_2005.png",
    ...overrides,
  };
}

function makeMemoryStorage() {
  const map = new Map<string, string>();
  return {
    map,
    getItem(key: string): string | null {
      return map.has(key) ? (map.get(key) ?? null) : null;
    },
    setItem(key: string, value: string): void {
      map.set(key, value);
    },
  };
}

test("extractStickerIdentity parses attachment_name, query eid, /thumb/ path, and fallbackUrl safely", () => {
  assert.equal(isStickerMsgType("chat.sticker"), true);
  assert.equal(isStickerMsgType("sticker"), true);
  assert.equal(isStickerMsgType("5"), true);
  assert.equal(isStickerMsgType("35"), true);
  assert.equal(isStickerMsgType("36"), true);
  assert.equal(isStickerMsgType("chat.photo"), false);
  assert.equal(isStickerMsgType("chat.text"), false);

  // Standard parser output: sticker_{cateId}_{stickerId}.png + URL
  assert.deepEqual(extractStickerIdentity(makeStickerMsg()), {
    isSticker: true,
    stickerId: "2005",
    cateId: "1002",
    fallbackUrl: "https://stickers.example.invalid/api/emoticon/sticker/webpc?eid=2005&size=130",
  });

  // cateId === 0 (default/unknown pack) yields cateId: null so blocking pack 0 never blocks unrelated stickers
  assert.deepEqual(
    extractStickerIdentity(
      makeStickerMsg({
        attachment_name: "sticker_0_15.webp",
        attachment_url: "https://cdn.example.invalid/images/stickers/default/thumb/15.png",
      }),
    ),
    {
      isSticker: true,
      stickerId: "15",
      cateId: null,
      fallbackUrl: "https://cdn.example.invalid/images/stickers/default/thumb/15.png",
    },
  );

  // URL-only ?eid=... extraction (including case-insensitive EID and cateId param, stripping #fragment)
  assert.deepEqual(
    extractStickerIdentity(
      makeStickerMsg({
        msg_type: "5",
        attachment_name: null,
        attachment_url:
          "HTTPS://Stickers.Example.Invalid/webpc?size=130&EID=777&catId=42#preview",
      }),
    ),
    {
      isSticker: true,
      stickerId: "777",
      cateId: "42",
      fallbackUrl: "https://stickers.example.invalid/webpc?size=130&EID=777&catId=42",
    },
  );

  // URL-only /thumb/{sid}.png extraction when attachment_name has invalid sid=0
  assert.deepEqual(
    extractStickerIdentity(
      makeStickerMsg({
        attachment_name: "sticker_10_0.png",
        attachment_url: "https://cdn.example.invalid/images/stickers/default/thumb/24.png",
      }),
    ),
    {
      isSticker: true,
      stickerId: "24",
      cateId: "10",
      fallbackUrl: "https://cdn.example.invalid/images/stickers/default/thumb/24.png",
    },
  );

  // Custom sticker URL with no numeric IDs falls back cleanly to normalized URL
  assert.deepEqual(
    extractStickerIdentity(
      makeStickerMsg({
        attachment_name: "custom_wave.webp",
        attachment_url: "https://CDN.Example.Invalid/packs/wave.webp#v1",
      }),
    ),
    {
      isSticker: true,
      stickerId: null,
      cateId: null,
      fallbackUrl: "https://cdn.example.invalid/packs/wave.webp",
    },
  );

  // Ephemeral blob:/data: URLs are ignored for fallbackUrl
  assert.equal(normalizeStickerFallbackUrl("blob:https://app.local/1234"), null);
  assert.equal(normalizeStickerFallbackUrl("data:image/png;base64,AAAA"), null);

  // Non-sticker messages MUST never be treated as stickers even with a sticker-like filename
  assert.deepEqual(
    extractStickerIdentity(
      makeStickerMsg({
        msg_type: "chat.photo",
        attachment_name: "sticker_1002_2005.png",
      }),
    ),
    {
      isSticker: false,
      stickerId: null,
      cateId: null,
      fallbackUrl: null,
    },
  );
});

test("matchBlockedStickerRule enforces received-only, thread vs global scope, stickerId, cateId, and fallbackUrl", () => {
  let cfg = DEFAULT_BLOCKED_STICKERS_CONFIG;
  cfg = addBlockedStickerRule(cfg, {
    id: "r-thread-stk",
    kind: "sticker",
    value: "2005",
    scopeThreadId: "thread-a",
    createdAt: 100,
  });
  cfg = addBlockedStickerRule(cfg, {
    id: "r-global-stk",
    kind: "sticker",
    value: "9001",
    scopeThreadId: null,
    createdAt: 200,
  });
  cfg = addBlockedStickerRule(cfg, {
    id: "r-thread-cat",
    kind: "category",
    value: "555",
    scopeThreadId: "thread-b",
    createdAt: 300,
  });
  cfg = addBlockedStickerRule(cfg, {
    id: "r-global-url",
    kind: "sticker",
    value: "https://CDN.Example.Invalid/custom/cat.webp#frag",
    scopeThreadId: null,
    createdAt: 400,
  });

  // Thread-scoped sticker matches in thread-a (even if CDN size query differs), NOT in thread-b
  const matchedThread = matchBlockedStickerRule(
    makeStickerMsg({
      thread_id: "thread-a",
      attachment_name: "sticker_1002_2005.png",
      attachment_url: "https://stickers.example.invalid/webpc?eid=2005&size=260",
    }),
    cfg.rules,
  );
  assert.equal(matchedThread?.id, "r-thread-stk");
  assert.equal(
    matchBlockedStickerRule(makeStickerMsg({ thread_id: "thread-b" }), cfg.rules),
    null,
  );

  // Global sticker matches across any thread
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-z",
        attachment_name: null,
        attachment_url: "https://stickers.example.invalid/webpc?eid=9001&size=130",
      }),
      cfg.rules,
    )?.id,
    "r-global-stk",
  );

  // Category rule matches any sticker in pack 555 inside thread-b only
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-b",
        attachment_name: "sticker_555_8888.png",
        attachment_url: "https://stickers.example.invalid/webpc?eid=8888&size=130",
      }),
      cfg.rules,
    )?.id,
    "r-thread-cat",
  );
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-a",
        attachment_name: "sticker_555_8888.png",
        attachment_url: "https://stickers.example.invalid/webpc?eid=8888&size=130",
      }),
      cfg.rules,
    ),
    null,
  );

  // Normalized fallbackUrl matching when sticker has no numeric ID
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-c",
        attachment_name: "cat.webp",
        attachment_url: "https://cdn.example.invalid/custom/cat.webp",
      }),
      cfg.rules,
    )?.id,
    "r-global-url",
  );

  // Outgoing stickers MUST NEVER be blocked
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-a",
        is_outgoing: true,
        attachment_name: "sticker_1002_2005.png",
      }),
      cfg.rules,
    ),
    null,
  );
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({
        thread_id: "thread-z",
        is_outgoing: true,
        attachment_name: "sticker_1_9001.png",
      }),
      cfg.rules,
    ),
    null,
  );

  // Recalled messages and non-sticker messages MUST NEVER be blocked
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({ thread_id: "thread-a", is_recalled: true }),
      cfg.rules,
    ),
    null,
  );
  assert.equal(
    matchBlockedStickerRule(
      makeStickerMsg({ thread_id: "thread-a", msg_type: "chat.text", text: "Hello" }),
      cfg.rules,
    ),
    null,
  );
});

test("buildBlockedStickerRuleFromMessage constructs thread/global sticker and category inputs safely", () => {
  const msg = makeStickerMsg({ thread_id: "t-42" });

  assert.deepEqual(
    buildBlockedStickerRuleFromMessage(msg, {
      kind: "sticker",
      global: false,
      threadTitle: "Trần Minh",
    }),
    {
      kind: "sticker",
      value: "2005",
      scopeThreadId: "t-42",
      scopeThreadTitle: "Trần Minh",
      previewUrl: "https://stickers.example.invalid/api/emoticon/sticker/webpc?eid=2005&size=130",
      label: "#2005",
    },
  );

  assert.deepEqual(
    buildBlockedStickerRuleFromMessage(msg, { kind: "sticker", global: true }),
    {
      kind: "sticker",
      value: "2005",
      scopeThreadId: null,
      scopeThreadTitle: undefined,
      previewUrl: "https://stickers.example.invalid/api/emoticon/sticker/webpc?eid=2005&size=130",
      label: "#2005",
    },
  );

  assert.deepEqual(
    buildBlockedStickerRuleFromMessage(msg, {
      kind: "category",
      global: false,
      threadTitle: "Nhóm Dev",
    }),
    {
      kind: "category",
      value: "1002",
      scopeThreadId: "t-42",
      scopeThreadTitle: "Nhóm Dev",
      previewUrl: "https://stickers.example.invalid/api/emoticon/sticker/webpc?eid=2005&size=130",
      label: "#1002",
    },
  );

  // Sticker without positive cateId cannot produce a category rule
  assert.equal(
    buildBlockedStickerRuleFromMessage(
      makeStickerMsg({ attachment_name: "sticker_0_15.png" }),
      { kind: "category", global: false },
    ),
    null,
  );

  // Non-sticker returns null
  assert.equal(
    buildBlockedStickerRuleFromMessage(
      makeStickerMsg({ msg_type: "chat.photo" }),
      { kind: "sticker", global: true },
    ),
    null,
  );
});

test("addBlockedStickerRule deduplicates by (scope, kind, value), caps at 500, and supports remove/mode", () => {
  let cfg = DEFAULT_BLOCKED_STICKERS_CONFIG;
  cfg = addBlockedStickerRule(cfg, {
    id: "r1",
    kind: "sticker",
    value: "2005",
    scopeThreadId: "t1",
    createdAt: 1000,
  });
  // Exact duplicate enriches missing metadata without adding a second row
  cfg = addBlockedStickerRule(cfg, {
    id: "r1-dup",
    kind: "sticker",
    value: "02005",
    scopeThreadId: " t1 ",
    scopeThreadTitle: "Đào Nụ",
    createdAt: 2000,
  });
  assert.equal(cfg.rules.length, 1);
  assert.equal(cfg.rules[0]?.id, "r1");
  assert.equal(cfg.rules[0]?.scopeThreadTitle, "Đào Nụ");

  // Same sticker in global scope is distinct from thread scope
  cfg = addBlockedStickerRule(cfg, {
    id: "r2",
    kind: "sticker",
    value: "2005",
    scopeThreadId: null,
    createdAt: 3000,
  });
  assert.equal(cfg.rules.length, 2);

  // Invalid inputs are ignored cleanly
  const beforeInvalid = cfg;
  assert.equal(
    addBlockedStickerRule(cfg, { kind: "category", value: "0", scopeThreadId: null }),
    beforeInvalid,
  );
  assert.equal(
    addBlockedStickerRule(cfg, {
      kind: "sticker",
      value: "blob:https://app.local/abc",
      scopeThreadId: null,
    }),
    beforeInvalid,
  );

  // Mode toggle & removal
  cfg = setBlockedStickerDisplayMode(cfg, "hide");
  assert.equal(cfg.mode, "hide");
  cfg = removeBlockedStickerRule(cfg, "r1");
  assert.deepEqual(
    cfg.rules.map((r) => r.id),
    ["r2"],
  );
  assert.equal(removeBlockedStickerRule(cfg, "missing-id"), cfg);

  // Cap at MAX_BLOCKED_STICKER_RULES (500)
  let capped = DEFAULT_BLOCKED_STICKERS_CONFIG;
  for (let i = 1; i <= MAX_BLOCKED_STICKER_RULES + 25; i += 1) {
    capped = addBlockedStickerRule(capped, {
      id: `bulk-${i}`,
      kind: "sticker",
      value: String(i),
      scopeThreadId: null,
      createdAt: i,
    });
  }
  assert.equal(capped.rules.length, MAX_BLOCKED_STICKER_RULES);
  assert.equal(capped.rules[0]?.value, String(MAX_BLOCKED_STICKER_RULES + 25));
});

test("loadBlockedStickersConfig and saveBlockedStickersConfig round-trip cleanly and sanitize corrupt data", () => {
  const storage = makeMemoryStorage();

  // Empty storage yields defaults
  assert.deepEqual(loadBlockedStickersConfig(storage), { mode: "collapse", rules: [] });

  let cfg = setBlockedStickerDisplayMode(DEFAULT_BLOCKED_STICKERS_CONFIG, "hide");
  cfg = addBlockedStickerRule(cfg, {
    id: "persist-1",
    kind: "sticker",
    value: "2005",
    scopeThreadId: "thread-a",
    scopeThreadTitle: "Trần Minh",
    previewUrl: "https://stickers.example.invalid/webpc?eid=2005&size=130#frag",
    label: "#2005",
    createdAt: 1_700_000_000_000,
  });
  cfg = addBlockedStickerRule(cfg, {
    id: "persist-2",
    kind: "category",
    value: "1002",
    scopeThreadId: null,
    createdAt: 1_700_000_000_100,
  });

  saveBlockedStickersConfig(cfg, storage);
  assert.equal(storage.map.has(BLOCKED_STICKERS_STORAGE_KEY), true);

  const reloaded = loadBlockedStickersConfig(storage);
  assert.equal(reloaded.mode, "hide");
  assert.equal(reloaded.rules.length, 2);
  assert.equal(reloaded.rules[0]?.id, "persist-2");
  assert.equal(reloaded.rules[1]?.id, "persist-1");
  assert.equal(
    reloaded.rules[1]?.previewUrl,
    "https://stickers.example.invalid/webpc?eid=2005&size=130",
  );

  // Corrupt JSON falls back safely
  storage.setItem(BLOCKED_STICKERS_STORAGE_KEY, "{not valid json");
  assert.deepEqual(loadBlockedStickersConfig(storage), { mode: "collapse", rules: [] });

  // Malformed payload is normalized & deduplicated safely
  const sanitized = normalizeBlockedStickersConfig({
    mode: "invalid-mode",
    rules: [
      { id: "ok", kind: "sticker", value: "77", scopeThreadId: null, createdAt: 10 },
      { id: "dup", kind: "sticker", value: "77", scopeThreadId: "", createdAt: 20 },
      { id: "bad-cat", kind: "category", value: "0", scopeThreadId: null, createdAt: 30 },
      { id: "bad-kind", kind: "emoji", value: "1", scopeThreadId: null, createdAt: 40 },
      null,
    ],
  });
  assert.equal(sanitized.mode, "collapse");
  assert.equal(sanitized.rules.length, 1);
  assert.equal(sanitized.rules[0]?.id, "ok");

  // Throwing storage does not crash save
  assert.doesNotThrow(() =>
    saveBlockedStickersConfig(cfg, {
      setItem() {
        throw new Error("QuotaExceededError");
      },
    }),
  );
});

test("isStickerMsgType excludes sticker types 5, 35, 36 case-insensitively and hide-mode filtering preserves grouping & unread divider", () => {
  assert.equal(isStickerMsgType("35"), true);
  assert.equal(isStickerMsgType("36"), true);
  assert.equal(isStickerMsgType("5"), true);
  assert.equal(isStickerMsgType("CHAT.STICKER"), true);
  assert.equal(isStickerMsgType("chat.photo"), false);
  assert.equal(isStickerMsgType("2"), false);

  const cfg = addBlockedStickerRule(
    setBlockedStickerDisplayMode(DEFAULT_BLOCKED_STICKERS_CONFIG, "hide"),
    {
      id: "r-hide-35",
      kind: "sticker",
      value: "2005",
      scopeThreadId: "thread-a",
      createdAt: 1,
    },
  );

  const msgBefore = makeStickerMsg({
    msg_id: "m-1",
    sender_id: "u-peer",
    msg_type: "chat.text",
    text: "Tin 1",
    attachment_name: null,
    attachment_url: null,
    ts: 1_700_000_000_000,
  });
  const blockedMid = makeStickerMsg({
    msg_id: "m-2-stk",
    sender_id: "u-peer",
    msg_type: "35",
    ts: 1_700_000_010_000,
  });
  const msgAfter = makeStickerMsg({
    msg_id: "m-3",
    sender_id: "u-peer",
    msg_type: "chat.text",
    text: "Tin 2",
    attachment_name: null,
    attachment_url: null,
    ts: 1_700_000_020_000,
  });

  const raw = [msgBefore, blockedMid, msgAfter];
  const visible = raw.filter(
    (m) => m.is_outgoing || m.is_recalled || matchBlockedStickerRule(m, cfg.rules) === null,
  );
  assert.deepEqual(
    visible.map((m) => m.msg_id),
    ["m-1", "m-3"],
  );
  // First unread after watermark 1_700_000_005_000 anchors to visible m-3 (never hidden m-2-stk)
  assert.equal(findFirstUnreadMsgId(visible, 1_700_000_005_000, 1), "m-3");
});
