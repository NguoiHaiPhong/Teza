import test from "node:test";
import assert from "node:assert/strict";
import { tokenizeMentions } from "../src/utils/mentions.ts";

test("tokenizeMentions highlights multi-word @mention name and links userId cleanly", () => {
  const text =
    "@Đào Nụ chạy như này thì PCBA đi làm ngày lễ à c. 3 line còn chưa nuốt nổi";
  const tokens = tokenizeMentions(text, [{ id: "u-nu", name: "Đào Nụ" }]);

  assert.deepEqual(tokens, [
    { kind: "mention", text: "@Đào Nụ", userId: "u-nu", displayName: "Đào Nụ" },
    {
      kind: "text",
      text: " chạy như này thì PCBA đi làm ngày lễ à c. 3 line còn chưa nuốt nổi",
    },
  ]);

  // Without loaded member list, structural fallback still highlights full "@Đào Nụ"
  const fallbackTokens = tokenizeMentions(text, []);
  assert.deepEqual(fallbackTokens, [
    { kind: "mention", text: "@Đào Nụ", userId: null },
    {
      kind: "text",
      text: " chạy như này thì PCBA đi làm ngày lễ à c. 3 line còn chưa nuốt nổi",
    },
  ]);
});

test("tokenizeMentions handles multiple mentions, longest match, accent-insensitive match, and commas", () => {
  const candidates = [
    { id: "u-thanh", name: "Nguyễn Thanh" },
    { id: "u-tung", name: "Nguyễn Thanh Tùng" },
    { id: "u-da", name: "Đ A Nguyễn" },
    { id: "u-ha", name: "Hà Minh Thường" },
    { id: "u-nu", name: "Đào Nụ" },
    { id: "u-choi", name: "Korea Choi" },
  ];

  // Longest-first avoids matching "@Nguyễn Thanh" when "@Nguyễn Thanh Tùng" appears
  const longest = tokenizeMentions("@Nguyễn Thanh Tùng xem giúp nhé", candidates);
  assert.equal(longest[0].kind, "mention");
  if (longest[0].kind === "mention") {
    assert.equal(longest[0].text, "@Nguyễn Thanh Tùng");
    assert.equal(longest[0].userId, "u-tung");
  }

  // Multiple multi-word mentions in one message
  const multi = tokenizeMentions(
    "@Nguyễn Thanh @Đ A Nguyễn @Hà Minh Thường e gửi todolist nhé",
    candidates,
  );
  assert.deepEqual(
    multi.filter((t) => t.kind === "mention"),
    [
      { kind: "mention", text: "@Nguyễn Thanh", userId: "u-thanh", displayName: "Nguyễn Thanh" },
      { kind: "mention", text: "@Đ A Nguyễn", userId: "u-da", displayName: "Đ A Nguyễn" },
      { kind: "mention", text: "@Hà Minh Thường", userId: "u-ha", displayName: "Hà Minh Thường" },
    ],
  );

  // Accent-insensitive unaccented mention matches member profile
  const unaccented = tokenizeMentions("@Dao Nu please share", candidates);
  assert.deepEqual(unaccented, [
    { kind: "mention", text: "@Dao Nu", userId: "u-nu", displayName: "Đào Nụ" },
    { kind: "text", text: " please share" },
  ]);

  // Mid-sentence mention before comma keeps comma in plain text
  const comma = tokenizeMentions("Dear Mr. @Sky and Mr. @Korea Choi, please verify", candidates);
  assert.deepEqual(
    comma.filter((t) => t.kind === "mention"),
    [
      { kind: "mention", text: "@Sky", userId: null },
      { kind: "mention", text: "@Korea Choi", userId: "u-choi", displayName: "Korea Choi" },
    ],
  );
  assert.equal(comma[comma.length - 1]?.text, ", please verify");
});

test("tokenizeMentions distinguishes @All and ignores email addresses", () => {
  const tokens = tokenizeMentions("@All gửi mail về support@teza.app nhé", [
    { id: "u1", name: "An" },
  ]);
  assert.deepEqual(tokens, [
    { kind: "mention", text: "@All", userId: null, isAll: true },
    { kind: "text", text: " gửi mail về support@teza.app nhé" },
  ]);

  // Prefix word-boundary guard: "@Anh" must not match member "An"
  const boundary = tokenizeMentions("@Anh kiểm tra nhé", [{ id: "u1", name: "An" }]);
  assert.deepEqual(boundary, [
    { kind: "mention", text: "@Anh", userId: null },
    { kind: "text", text: " kiểm tra nhé" },
  ]);

  // Punctuation after "@" (e.g. rose emoticon "@};-" or "@#$") must remain plain text
  const punct = tokenizeMentions("Tặng hoa @};- và @#$ nhé", [{ id: "u1", name: "An" }]);
  assert.deepEqual(punct, [{ kind: "text", text: "Tặng hoa @};- và @#$ nhé" }]);
});
