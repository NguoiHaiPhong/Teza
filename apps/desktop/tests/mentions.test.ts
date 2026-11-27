import test from "node:test";
import assert from "node:assert/strict";
import {
  buildComposerMentionMembers,
  extractCompletedMentionSpans,
  filterMentionCandidates,
  findActiveMentionQuery,
  foldVietnameseText,
  isMentionAllMember,
  isPlaceholderMemberName,
  MENTION_ALL_ID,
  reconcileCompletedMentions,
  tokenizeMentions,
} from "../src/utils/mentions.ts";

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

  // @Mọi người is also recognized as an all-mention
  const moiNguoi = tokenizeMentions("@Mọi người xem lịch họp nhé", [
    { id: MENTION_ALL_ID, name: "All" },
  ]);
  assert.deepEqual(moiNguoi, [
    { kind: "mention", text: "@Mọi người", userId: null, isAll: true },
    { kind: "text", text: " xem lịch họp nhé" },
  ]);
});

test("foldVietnameseText normalizes NFC, NFD, case, diacritics, and đ/Đ identically", () => {
  const nfcLoc = "Lộc";
  const nfdLoc = "Lo\u0302\u0323c"; // Decomposed NFD from macOS/IME
  assert.equal(foldVietnameseText(nfcLoc), "loc");
  assert.equal(foldVietnameseText(nfdLoc), "loc");
  assert.equal(foldVietnameseText("LỘC"), "loc");
  assert.equal(foldVietnameseText("Đức Lộc"), "duc loc");
  assert.equal(foldVietnameseText("đào nụ"), "dao nu");
  assert.equal(foldVietnameseText("ĐÀO NỤ"), "dao nu");
  assert.equal(foldVietnameseText("ĂÂĐÊÔƠƯ ăâđêôơư"), "aadeoou aadeoou");
  assert.equal(foldVietnameseText("Trần Văn Lộc"), "tran van loc");
});

test("findActiveMentionQuery detects single/multi-word queries and blocks emails & invalid delimiters", () => {
  // Single-word and empty queries
  assert.deepEqual(findActiveMentionQuery("@", 1), { start: 0, query: "" });
  assert.deepEqual(findActiveMentionQuery("@L", 2), { start: 0, query: "L" });
  assert.deepEqual(findActiveMentionQuery("@loc", 4), { start: 0, query: "loc" });
  assert.deepEqual(findActiveMentionQuery("@Lộc", 4), { start: 0, query: "Lộc" });

  // NFD decomposed Vietnamese input
  const nfdDraft = "@Lo\u0302\u0323c";
  assert.deepEqual(findActiveMentionQuery(nfdDraft, nfdDraft.length), {
    start: 0,
    query: "Lo\u0302\u0323c",
  });

  // After whitespace and opening punctuation
  assert.deepEqual(findActiveMentionQuery("Chào @Lộc", 9), { start: 5, query: "Lộc" });
  assert.deepEqual(findActiveMentionQuery("(@Nguyễn L", 10), { start: 1, query: "Nguyễn L" });
  assert.deepEqual(findActiveMentionQuery('"@Tran Van', 10), { start: 1, query: "Tran Van" });
  assert.deepEqual(findActiveMentionQuery("Line 1\n@Lộc", 11), { start: 7, query: "Lộc" });

  // Multi-word queries up to 4 words + single trailing space between words
  assert.deepEqual(findActiveMentionQuery("@Nguyễn ", 8), { start: 0, query: "Nguyễn " });
  assert.deepEqual(findActiveMentionQuery("@Nguyễn L", 9), { start: 0, query: "Nguyễn L" });
  assert.deepEqual(findActiveMentionQuery("@Trần Văn Lộc", 13), {
    start: 0,
    query: "Trần Văn Lộc",
  });
  assert.deepEqual(findActiveMentionQuery("@Nguyễn Thị Thanh Thảo", 22), {
    start: 0,
    query: "Nguyễn Thị Thanh Thảo",
  });

  // Multiple mentions in same draft target the active one at cursor
  assert.deepEqual(findActiveMentionQuery("@An chào @Lộc", 13), { start: 9, query: "Lộc" });
  assert.deepEqual(findActiveMentionQuery("Chào @Lộc đi ăn nhé", 9), { start: 5, query: "Lộc" });

  // Emails MUST NEVER trigger
  assert.equal(findActiveMentionQuery("foo@bar.com", 11), null);
  assert.equal(findActiveMentionQuery("foo@bar", 7), null);
  assert.equal(findActiveMentionQuery("Gửi mail user.name@domain.com", 29), null);
  assert.equal(findActiveMentionQuery("a_b@test", 8), null);

  // Stop matching on leading space, 2+ spaces, newlines, punctuation, >4 words, >32 chars
  assert.equal(findActiveMentionQuery("@ ", 2), null);
  assert.equal(findActiveMentionQuery("@Lộc  ", 6), null);
  assert.equal(findActiveMentionQuery("@Lộc  đang bận", 14), null);
  assert.equal(findActiveMentionQuery("@Lộc\nchào", 9), null);
  assert.equal(findActiveMentionQuery("@Lộc, anh ơi", 12), null);
  assert.equal(findActiveMentionQuery("@Lộc.", 5), null);
  assert.equal(findActiveMentionQuery("@};-", 4), null);
  assert.equal(findActiveMentionQuery("@a b c d ", 9), null);
  assert.equal(findActiveMentionQuery("@a b c d e", 10), null);
  assert.equal(findActiveMentionQuery("@" + "a".repeat(33), 34), null);
  assert.equal(findActiveMentionQuery("@Lộc", 0), null);
});

test("filterMentionCandidates matches Vietnamese diacritics, NFD/NFC, đ/Đ, multi-word queries, and deduplicates", () => {
  const members = [
    { id: "u-nguyen-loc", name: "Nguyễn Lộc" },
    { id: "u-tran-van-loc", name: "Trần Văn Lộc" },
    { id: "u-duc-loc", name: "Đức Lộc" },
    { id: "u-loc", name: "Lộc" },
    { id: "u-loc-dup", name: "Lộc" }, // different id
    { id: "u-loc", name: "Lộc Duplicate ID" }, // duplicate id must be ignored
    { id: "u-an", name: "Nguyễn An" },
    { id: "u-nfd", name: "Đa\u0300o Nu\u0323" }, // NFD member name
  ];

  // @Lộc, @loc, @lộc, @Loc, @L all match all 4 Lộc variants
  for (const q of ["Lộc", "loc", "lộc", "Loc", "L", "Lo\u0302\u0323c"]) {
    const res = filterMentionCandidates(members, q, 10);
    const ids = res.map((m) => m.id);
    assert.ok(ids.includes("u-loc"), `query "${q}" should match Lộc`);
    assert.ok(ids.includes("u-nguyen-loc"), `query "${q}" should match Nguyễn Lộc`);
    assert.ok(ids.includes("u-tran-van-loc"), `query "${q}" should match Trần Văn Lộc`);
    assert.ok(ids.includes("u-duc-loc"), `query "${q}" should match Đức Lộc`);
    assert.equal(
      ids.filter((id) => id === "u-loc").length,
      1,
      `query "${q}" must deduplicate by id`,
    );
  }

  // đ/Đ folding both directions
  const ducRes = filterMentionCandidates(members, "duc", 10);
  assert.deepEqual(
    ducRes.map((m) => m.id),
    ["u-duc-loc"],
  );
  const daoRes = filterMentionCandidates(members, "Đào N", 10);
  assert.deepEqual(
    daoRes.map((m) => m.id),
    ["u-nfd"],
  );

  // Multi-word queries: @Nguyễn L and @tran loc (skipping middle name "Văn")
  const nguyenL = filterMentionCandidates(members, "Nguyễn L", 10);
  assert.deepEqual(
    nguyenL.map((m) => m.id),
    ["u-nguyen-loc"],
  );

  const tranLoc = filterMentionCandidates(members, "tran loc", 10);
  assert.deepEqual(
    tranLoc.map((m) => m.id),
    ["u-tran-van-loc"],
  );

  // Trailing space narrows multi-word names and does NOT re-match exact 1-word completed name
  const locTrailing = filterMentionCandidates(
    [
      { id: "u1", name: "Lộc" },
      { id: "u2", name: "Lộc Phát" },
    ],
    "Lộc ",
    10,
  );
  assert.deepEqual(
    locTrailing.map((m) => m.id),
    ["u2"],
  );
});

test("filterMentionCandidates enforces ranking priority: accented prefix > folded prefix > substring", () => {
  const members = [
    { id: "block", name: "Block" },
    { id: "phuc-loc-tho", name: "Phúc Lộc Thọ" },
    { id: "tran-loc-ascii", name: "Trần Loc" },
    { id: "nguyen-loc", name: "Nguyễn Lộc" },
    { id: "loc-phat", name: "Lộc Phát" },
    { id: "loc", name: "Lộc" },
  ];

  // With accented "@Lộc": Lộc > Lộc Phát > Nguyễn Lộc > Phúc Lộc Thọ > Trần Loc > Block
  const accented = filterMentionCandidates(members, "Lộc", 10).map((m) => m.id);
  assert.deepEqual(accented, [
    "loc",
    "loc-phat",
    "nguyen-loc",
    "phuc-loc-tho",
    "tran-loc-ascii",
    "block",
  ]);

  // With unaccented "@loc": folded word/full prefixes STILL rank above mid-word substring "Block"
  const unaccented = filterMentionCandidates(members, "loc", 10).map((m) => m.id);
  assert.ok(unaccented.indexOf("loc") < unaccented.indexOf("block"));
  assert.ok(unaccented.indexOf("nguyen-loc") < unaccented.indexOf("block"));
  assert.ok(unaccented.indexOf("phuc-loc-tho") < unaccented.indexOf("block"));
  assert.equal(unaccented[unaccented.length - 1], "block");
});

test("filterMentionCandidates supports @All / @Mọi người matching without polluting @L queries", () => {
  const members = [
    { id: MENTION_ALL_ID, name: "All" },
    { id: "u-loc", name: "Lộc" },
    { id: "u-minh", name: "Minh" },
  ];

  // Empty query includes @All first
  assert.equal(filterMentionCandidates(members, "", 8)[0]?.id, MENTION_ALL_ID);

  // Queries matching All / Mọi người / everyone
  for (const q of ["all", "All", "al", "moi", "mọi", "mọi người", "everyone"]) {
    const ids = filterMentionCandidates(members, q, 8).map((m) => m.id);
    assert.ok(ids.includes(MENTION_ALL_ID), `query "${q}" should match @All`);
  }

  // Typing @L / @loc / @Lộc must NOT spuriously match @All via substring "l"
  for (const q of ["L", "l", "loc", "Lộc"]) {
    const ids = filterMentionCandidates(members, q, 8).map((m) => m.id);
    assert.ok(!ids.includes(MENTION_ALL_ID), `query "${q}" must not match @All`);
    assert.ok(ids.includes("u-loc"), `query "${q}" should match Lộc`);
  }
});

test("buildComposerMentionMembers merges group details, message senders, direct partner, and excludes self", () => {
  const directDirectory = new Map([
    ["u-loc", { name: "Nguyễn Lộc", avatar: "https://cdn.example/loc.jpg" }],
    ["u-an", { name: "Trần An", avatar: "https://cdn.example/an.jpg" }],
  ]);

  // Group chat: includes @All, merges cached/loaded group members + recent message senders,
  // upgrades raw numeric placeholder names, excludes self and generic "Bạn"
  const groupList = buildComposerMentionMembers({
    thread: { id: "g-1", title: "Team Core", avatar: "", kind: "group" },
    groupMembers: [
      { id: "u-me", name: "Tôi", avatar: "", role: "Trưởng nhóm" },
      { id: "u-loc", name: "123456789", avatar: "", role: "Thành viên" },
    ],
    messages: [
      { sender_id: "u-me", sender_name: "Bạn", is_outgoing: true },
      { sender_id: "u-an", sender_name: "Trần An", is_outgoing: false },
      { sender_id: "u-raw", sender_name: "987654321", is_outgoing: false },
      { sender_id: "u-duc", sender_name: "Đức Lộc", is_outgoing: false },
    ],
    selfUid: "u-me",
    directDirectory,
    mentionAllRole: "Mọi người",
  });

  assert.equal(groupList[0]?.id, MENTION_ALL_ID);
  assert.equal(groupList[0]?.name, "All");
  assert.equal(groupList[0]?.role, "Mọi người");
  assert.ok(!groupList.some((m) => m.id === "u-me"), "must exclude self");
  assert.ok(!groupList.some((m) => m.id === "u-raw"), "must skip unresolved raw numeric IDs");

  const loc = groupList.find((m) => m.id === "u-loc");
  assert.equal(loc?.name, "Nguyễn Lộc");
  assert.equal(loc?.avatar, "https://cdn.example/loc.jpg");

  // Newest message sender (Đức Lộc) appears before older message sender (Trần An)
  const ducIdx = groupList.findIndex((m) => m.id === "u-duc");
  const anIdx = groupList.findIndex((m) => m.id === "u-an");
  assert.ok(ducIdx > 0 && anIdx > ducIdx);

  // Direct chat: includes conversation partner so @Lộc auto-suggests in 1-1 chats
  const directList = buildComposerMentionMembers({
    thread: {
      id: "u-loc",
      title: "Nguyễn Lộc",
      avatar: "https://cdn.example/loc.jpg",
      kind: "direct",
    },
    groupMembers: [],
    messages: [{ sender_id: "u-me", sender_name: "Bạn", is_outgoing: true }],
    selfUid: "u-me",
    directDirectory,
  });
  assert.deepEqual(directList, [
    {
      id: "u-loc",
      name: "Nguyễn Lộc",
      avatar: "https://cdn.example/loc.jpg",
      role: "",
    },
  ]);

  // Cloud chat returns empty
  const cloudList = buildComposerMentionMembers({
    thread: { id: "cloud", title: "Cloud của tôi", avatar: "", kind: "cloud" },
    groupMembers: [],
    messages: [],
    selfUid: "u-me",
  });
  assert.deepEqual(cloudList, []);
});

test("tokenizeMentions recognizes @tất cả / @tat ca / @everyone as all-mentions with boundaries", () => {
  const tatCa = tokenizeMentions("@tất cả chuẩn bị họp nhé", [{ id: "u1", name: "An" }]);
  assert.deepEqual(tatCa, [
    { kind: "mention", text: "@tất cả", userId: null, isAll: true },
    { kind: "text", text: " chuẩn bị họp nhé" },
  ]);

  const tatCaFolded = tokenizeMentions("Chào @Tat Ca, kiểm tra báo cáo", []);
  assert.deepEqual(tatCaFolded, [
    { kind: "text", text: "Chào " },
    { kind: "mention", text: "@Tat Ca", userId: null, isAll: true },
    { kind: "text", text: ", kiểm tra báo cáo" },
  ]);

  // NFD decomposed @tất cả normalizes to NFC and matches
  const tatCaNfd = tokenizeMentions("@ta\u0302\u0301t ca\u0309 xem nhé", []);
  assert.deepEqual(tatCaNfd, [
    { kind: "mention", text: "@tất cả", userId: null, isAll: true },
    { kind: "text", text: " xem nhé" },
  ]);

  const everyone = tokenizeMentions("@everyone check PR", []);
  assert.deepEqual(everyone, [
    { kind: "mention", text: "@everyone", userId: null, isAll: true },
    { kind: "text", text: " check PR" },
  ]);

  // Word-boundary and email exclusion
  const email = tokenizeMentions("Gửi mail contact@allianz.com hoặc user@everyone.org", []);
  assert.deepEqual(email, [
    { kind: "text", text: "Gửi mail contact@allianz.com hoặc user@everyone.org" },
  ]);

  const prefixOnly = tokenizeMentions("@tất cảabc test", []);
  assert.equal(prefixOnly[0]?.kind, "mention");
  if (prefixOnly[0]?.kind === "mention") {
    assert.equal(prefixOnly[0].isAll, undefined);
  }

  assert.equal(isMentionAllMember({ id: "x", name: "tất cả" }), true);
  assert.equal(isMentionAllMember({ id: "x", name: "Tat Ca" }), true);
});

test("isPlaceholderMemberName identifies generic/numeric placeholders and keeps real names", () => {
  for (const placeholder of [
    "",
    "   ",
    "Bạn",
    "bạn",
    "You",
    "you",
    "123456",
    "9876543210",
    "Người dùng",
    "Người dùng 12345",
    "nguoi dung 99",
  ]) {
    assert.equal(
      isPlaceholderMemberName(placeholder),
      true,
      `expected "${placeholder}" to be treated as placeholder`,
    );
  }

  for (const realName of ["An", "Lộc", "Đào Nụ", "Nguyễn Văn A", "Team 2026", "12345", "Bạn Bè"]) {
    assert.equal(
      isPlaceholderMemberName(realName),
      false,
      `expected "${realName}" to be treated as a real name`,
    );
  }
});

test("reconcileCompletedMentions preserves multi-mention spans across prepends and drops edited spans", () => {
  const initial = [
    { start: 0, text: "@An " },
    { start: 4, text: "@Lộc " },
  ];

  // Appending text after all mentions keeps both intact
  assert.deepEqual(
    reconcileCompletedMentions(initial, "@An @Lộc ", "@An @Lộc xem PR nhé"),
    initial,
  );

  // Prepending text shifts all completed mention offsets cleanly
  assert.deepEqual(
    reconcileCompletedMentions(initial, "@An @Lộc ", "Chào @An @Lộc "),
    [
      { start: 5, text: "@An " },
      { start: 9, text: "@Lộc " },
    ],
  );

  // Backspacing trailing space of the first mention drops only the edited span
  assert.deepEqual(
    reconcileCompletedMentions(initial, "@An @Lộc ", "@An@Lộc "),
    [{ start: 3, text: "@Lộc " }],
  );

  // Clearing draft drops all spans
  assert.deepEqual(reconcileCompletedMentions(initial, "@An @Lộc ", ""), []);
});

test("tokenizeMentions suppresses @all/@everyone/@mọi người when allowAll is false (direct/cloud chats)", () => {
  const candidates = [{ id: "u-an", name: "An" }];

  for (const sample of [
    "@all gửi file giúp nhé",
    "Chào @everyone nhé",
    "@Mọi người kiểm tra lại",
    "@tất cả xem giúp",
  ]) {
    const tokens = tokenizeMentions(sample, candidates, { allowAll: false });
    assert.deepEqual(tokens, [{ kind: "text", text: sample }]);
  }

  // Personal mentions in the same 1:1 message still tokenize cleanly
  const mixed = tokenizeMentions("Chào @An và @all nhé", candidates, { allowAll: false });
  assert.deepEqual(mixed, [
    { kind: "text", text: "Chào " },
    { kind: "mention", text: "@An", userId: "u-an", displayName: "An" },
    { kind: "text", text: " và @all nhé" },
  ]);

  // Member name starting with "All" (e.g. "Allen") still matches in direct chat
  const allen = tokenizeMentions("Hi @Allen!", [{ id: "u-allen", name: "Allen" }], {
    allowAll: false,
  });
  assert.deepEqual(allen, [
    { kind: "text", text: "Hi " },
    { kind: "mention", text: "@Allen", userId: "u-allen", displayName: "Allen" },
    { kind: "text", text: "!" },
  ]);
});

test("extractCompletedMentionSpans seeds already-complete @Name spans on restored drafts", () => {
  const members = [
    { id: MENTION_ALL_ID, name: "All" },
    { id: "u-thanh", name: "Nguyễn Thanh" },
    { id: "u-tung", name: "Nguyễn Thanh Tùng" },
    { id: "u-nu", name: "Đào Nụ" },
    { id: "u-an", name: "An" },
  ];

  // Longest-first multi-word match with trailing space
  assert.deepEqual(extractCompletedMentionSpans("@Nguyễn Thanh Tùng ", members), [
    { start: 0, text: "@Nguyễn Thanh Tùng " },
  ]);

  // Multiple completed mentions + @All in a group draft
  assert.deepEqual(extractCompletedMentionSpans("Chào @An @Đào Nụ @All ", members), [
    { start: 5, text: "@An " },
    { start: 9, text: "@Đào Nụ " },
    { start: 17, text: "@All " },
  ]);

  // Incomplete trailing mention without space is NOT seeded (allows resuming completion)
  assert.deepEqual(extractCompletedMentionSpans("@Đào Nụ", members), []);

  // Emails and unknown handles are ignored
  assert.deepEqual(extractCompletedMentionSpans("mail user@An @Unknown ", members), []);

  // NFD decomposed Vietnamese draft matches NFC member name while preserving exact draft slice
  const nfdDraft = "@Đa\u0300o Nu\u0323 ";
  assert.deepEqual(extractCompletedMentionSpans(nfdDraft, members), [
    { start: 0, text: nfdDraft },
  ]);
});
