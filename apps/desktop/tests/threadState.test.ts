import test from "node:test";
import assert from "node:assert/strict";
import type { ChatFolderDto, MessageDto, PollDto, ThreadDto } from "../src/types.ts";
import { parsePollPayload } from "../src/utils/richMessage.ts";
import {
  QUICK_REACTION_EMOJIS,
  applyOpenThreadSearchState,
  applyOptimisticReaction,
  applyPollUpdateToMessages,
  applyThreadReadSync,
  bumpThreadList,
  compareThreads,
  computeLayoutMode,
  computeThreadRenderLimitForTarget,
  countFolderUnread,
  dedupeOptimisticPollMessages,
  dedupePollMessages,
  extractFirstUrl,
  filterThreadsByFolder,
  findFirstUnreadMsgId,
  getDisplayBodyText,
  isPollVoteActivityQuestion,
  looksLikePhoneQuery,
  mergePollDto,
  moveArrayItem,
  normalizeReactionEmoji,
  normalizeReactions,
  reconcileUpdatedMessage,
  resolveFolderForOpenedThread,
  shouldAutoMarkReadOnFocus,
  splitTextLinks,
  upsertMsg,
} from "../src/utils/threadState.ts";

function makeMsg(overrides: Partial<MessageDto>): MessageDto {
  return {
    msg_id: "m1",
    thread_id: "t1",
    sender_id: "u1",
    sender_name: "An",
    text: "hello",
    msg_type: "chat.text",
    ts: 1_700_000_000_000,
    is_outgoing: false,
    is_recalled: false,
    is_pinned: false,
    quote: null,
    reactions: [],
    attachment_url: null,
    attachment_name: null,
    ...overrides,
  };
}

function makeThread(overrides: Partial<ThreadDto>): ThreadDto {
  return {
    id: "t1",
    title: "Thread 1",
    avatar: "",
    kind: "direct",
    last_preview: "",
    last_ts: 1_700_000_000_000,
    unread_count: 0,
    is_pinned: false,
    is_muted: false,
    ...overrides,
  };
}

test("upsertMsg maintains ascending timestamp order and replaces existing messages in place", () => {
  const m2 = makeMsg({ msg_id: "m2", ts: 2000, text: "second" });
  const m1 = makeMsg({ msg_id: "m1", ts: 1000, text: "first" });

  let list = upsertMsg([], m2);
  list = upsertMsg(list, m1);
  assert.deepEqual(
    list.map((m) => m.msg_id),
    ["m1", "m2"],
  );

  const m2Updated = makeMsg({ msg_id: "m2", ts: 2000, text: "edited", is_pinned: true });
  list = upsertMsg(list, m2Updated);
  assert.equal(list.length, 2);
  assert.equal(list[1].text, "edited");
  assert.equal(list[1].is_pinned, true);
});

test("compareThreads orders pinned first, then newest timestamp regardless of unread", () => {
  const readOld = makeThread({ id: "a", last_ts: 100, unread_count: 0, is_pinned: false });
  const readNew = makeThread({ id: "b", last_ts: 200, unread_count: 0, is_pinned: false });
  const readSamePinned = makeThread({ id: "c", last_ts: 200, unread_count: 0, is_pinned: true });
  const pinnedOlder = makeThread({ id: "p", last_ts: 50, unread_count: 0, is_pinned: true });
  const unreadOld = makeThread({ id: "d", last_ts: 50, unread_count: 1, is_pinned: false });

  const sorted = [readOld, readNew, readSamePinned, pinnedOlder, unreadOld].sort(compareThreads);
  assert.deepEqual(
    sorted.map((t) => t.id),
    ["c", "p", "b", "a", "d"],
  );
});

test("bumpThreadList formats group/outgoing previews, increments/clears unread, and preserves newer preview", () => {
  const threads = [
    makeThread({ id: "dm", kind: "direct", last_ts: 1000, last_preview: "Cu", unread_count: 1 }),
    makeThread({ id: "grp", kind: "group", last_ts: 2000, last_preview: "Cu", unread_count: 0 }),
  ];

  // Incoming message on inactive group with numeric sender ID suppresses raw numeric UID
  const numericSenderMsg = makeMsg({
    thread_id: "grp",
    sender_name: "1234567890",
    text: "Xin chao nhom",
    ts: 3000,
  });
  let next = bumpThreadList(threads, numericSenderMsg, true);
  assert.equal(next[0].id, "grp"); // Both are unread now, grp has newer ts 3000 > 1000
  assert.equal(next[0].last_preview, "Xin chao nhom");
  assert.equal(next[0].unread_count, 1);

  // Incoming named sender on group formats "Name: text"
  const namedMsg = makeMsg({
    thread_id: "grp",
    sender_name: "Minh",
    attachment_name: "doc.pdf",
    text: "",
    ts: 4000,
  });
  next = bumpThreadList(next, namedMsg, true);
  assert.equal(next[0].id, "grp");
  assert.equal(next[0].last_preview, "Minh: [Tệp: doc.pdf]");
  assert.equal(next[0].unread_count, 2);

  // Older timestamp preserves newer last_preview while bumping unread count if requested
  const olderMsg = makeMsg({
    thread_id: "grp",
    sender_name: "Lan",
    text: "Tin cu hon",
    ts: 3500,
  });
  next = bumpThreadList(next, olderMsg, false);
  assert.equal(next[0].last_preview, "Minh: [Tệp: doc.pdf]");
  assert.equal(next[0].last_ts, 4000);
  assert.equal(next[0].unread_count, 2);

  // Outgoing message clears unread_count and prefixes "Bạn: " once
  const outMsg = makeMsg({
    thread_id: "dm",
    is_outgoing: true,
    text: "Bạn: Ok nha",
    ts: 5000,
  });
  next = bumpThreadList(next, outMsg, true);
  const dm = next.find((t) => t.id === "dm")!;
  assert.equal(dm.unread_count, 0);
  assert.equal(dm.last_preview, "Bạn: Ok nha");

  // Unknown thread returns identical list without throwing
  const unknown = makeMsg({ thread_id: "missing-999", ts: 9999 });
  assert.deepEqual(bumpThreadList(next, unknown, true), next);
});

test("upsertMsg replaces matching opt- optimistic bubble on confirmed outgoing message", () => {
  const opt = makeMsg({
    msg_id: "opt-123",
    is_outgoing: true,
    text: "Xin chao",
    ts: 1_700_000_001_000,
    send_status: "sending",
  });
  const confirmed = makeMsg({
    msg_id: "srv-456",
    is_outgoing: true,
    text: "Xin chao",
    ts: 1_700_000_002_500,
  });
  const list = upsertMsg([opt], confirmed);
  assert.equal(list.length, 1);
  assert.equal(list[0].msg_id, "srv-456");
});

test("getDisplayBodyText suppresses redundant file/media text and preserves captions", () => {
  assert.equal(
    getDisplayBodyText(
      makeMsg({
        msg_type: "share.file",
        text: "[Tập tin] bao_cao.pdf",
        attachment_url: "https://f.zdn.vn/bao_cao.pdf",
        attachment_name: "bao_cao.pdf",
      }),
    ),
    "",
  );
  assert.equal(
    getDisplayBodyText(
      makeMsg({
        msg_type: "chat.file",
        text: "bao_cao.pdf",
        attachment_url: "https://f.zdn.vn/bao_cao.pdf",
        attachment_name: "bao_cao.pdf",
      }),
    ),
    "",
  );
  assert.equal(
    getDisplayBodyText(
      makeMsg({
        msg_type: "chat.file",
        text: "[Tập tin] bao_cao.pdf\nXem trang 2 nha",
        attachment_url: "https://f.zdn.vn/bao_cao.pdf",
        attachment_name: "bao_cao.pdf",
      }),
    ),
    "Xem trang 2 nha",
  );
  assert.equal(
    getDisplayBodyText(
      makeMsg({
        msg_type: "chat.photo",
        text: "[Hình ảnh]",
        attachment_url: "https://f.zdn.vn/a.jpg",
      }),
    ),
    "",
  );
  assert.equal(
    getDisplayBodyText(
      makeMsg({
        msg_type: "chat.photo",
        text: "@All e gửi todolist nhé",
        attachment_url: "https://f.zdn.vn/a.jpg",
        attachment_name: "@All e gửi todolist nhé",
      }),
    ),
    "@All e gửi todolist nhé",
  );
});

test("splitTextLinks parses http/https URLs and trims trailing punctuation cleanly", () => {
  assert.deepEqual(splitTextLinks("Xem http://example.com/a(b)."), [
    { text: "Xem " },
    { text: "http://example.com/a(b)", url: "http://example.com/a(b)" },
    { text: "." },
  ]);
  assert.deepEqual(splitTextLinks("(https://litechat.app/test?x=1), ok"), [
    { text: "(" },
    { text: "https://litechat.app/test?x=1", url: "https://litechat.app/test?x=1" },
    { text: "), ok" },
  ]);
});

test("computeLayoutMode and looksLikePhoneQuery enforce responsive breakpoints and VN phone detection", () => {
  assert.deepEqual(computeLayoutMode(500, false), { mode: "single", autoCompactDrawer: true });
  assert.deepEqual(computeLayoutMode(800, false), { mode: "compact", autoCompactDrawer: true });
  assert.deepEqual(computeLayoutMode(1000, true), { mode: "compact", autoCompactDrawer: true });
  assert.deepEqual(computeLayoutMode(1280, true), { mode: "compact", autoCompactDrawer: false });
  assert.deepEqual(computeLayoutMode(1280, false), { mode: "full", autoCompactDrawer: false });

  assert.equal(looksLikePhoneQuery("0912 345 678"), true);
  assert.equal(looksLikePhoneQuery("+84912345678"), true);
  assert.equal(looksLikePhoneQuery("010-1234-5678"), true);
  assert.equal(looksLikePhoneQuery("+82 10-1234-5678"), true);
  assert.equal(looksLikePhoneQuery("821012345678"), true);
  assert.equal(looksLikePhoneQuery("+1 415 555 0100"), true);
  assert.equal(looksLikePhoneQuery("2025"), false);
  assert.equal(looksLikePhoneQuery("Minh Anh"), false);
  assert.equal(looksLikePhoneQuery("김민수"), false);
});

test("filterThreadsByFolder, countFolderUnread, and extractFirstUrl support custom folders and links", () => {
  const threads = [
    makeThread({ id: "u1", kind: "direct", unread_count: 3, is_muted: false }),
    makeThread({ id: "g1", kind: "group", unread_count: 5, is_muted: true }),
    makeThread({ id: "g2", kind: "group", unread_count: 2, is_muted: false }),
    makeThread({ id: "c1", kind: "cloud", unread_count: 0, is_muted: false }),
  ];
  const folders: ChatFolderDto[] = [
    {
      id: "work",
      name: "Cong viec",
      icon: "briefcase",
      color: "",
      thread_ids: ["u1"],
      excluded_thread_ids: [],
      include_kinds: ["group"],
      only_unread: false,
      exclude_muted: true,
      position: 1,
      remote_label_id: null,
    },
    {
      id: "unread-vip",
      name: "Chua doc VIP",
      icon: "star",
      color: "",
      thread_ids: [],
      excluded_thread_ids: [],
      include_kinds: [],
      only_unread: true,
      exclude_muted: false,
      position: 2,
      remote_label_id: null,
    },
  ];

  // Work folder includes u1 + unmuted groups (g2), excluding muted g1 and cloud c1
  assert.deepEqual(
    filterThreadsByFolder(threads, "custom:work", folders).map((t) => t.id),
    ["u1", "g2"],
  );
  assert.equal(countFolderUnread(threads, "custom:work", folders), 5);

  // "unread-vip" matches all unread threads including muted g1
  assert.deepEqual(
    filterThreadsByFolder(threads, "custom:unread-vip", folders).map((t) => t.id),
    ["u1", "g1", "g2"],
  );
  // Muted threads do not count in normal folder badge (only "unread" counts all)
  assert.equal(countFolderUnread(threads, "unread", folders), 10);
  assert.equal(countFolderUnread(threads, "group", folders), 2);

  assert.equal(
    extractFirstUrl("Tai tai lieu tai https://teza.app/docs(v2)."),
    "https://teza.app/docs(v2)",
  );
  assert.equal(extractFirstUrl("Khong co lien ket"), null);
});

test("findFirstUnreadMsgId uses last_read_in_ts watermark with count fallback and bumpThreadList updates receipt watermarks", () => {
  const msgs = [
    makeMsg({ msg_id: "m1", ts: 1000, is_outgoing: false }),
    makeMsg({ msg_id: "m2", ts: 2000, is_outgoing: true }),
    makeMsg({ msg_id: "m3", ts: 3000, is_outgoing: false }),
    makeMsg({ msg_id: "m4", ts: 4000, is_outgoing: false }),
  ];

  // Zero unread returns null
  assert.equal(findFirstUnreadMsgId(msgs, 1000, 0), null);
  // Watermark picks first incoming message after lastReadInTs
  assert.equal(findFirstUnreadMsgId(msgs, 2500, 1), "m3");
  // Fallback when watermark is 0 counts backwards by fallbackUnreadCount incoming messages
  assert.equal(findFirstUnreadMsgId(msgs, 0, 2), "m3");
  assert.equal(findFirstUnreadMsgId(msgs, 0, 1), "m4");

  const threads = [
    makeThread({ id: "t1", last_ts: 1000, unread_count: 0, last_read_in_ts: 1000, peer_read_ts: 1000 }),
  ];
  const afterUnreadIn = bumpThreadList(
    threads,
    makeMsg({ thread_id: "t1", ts: 3000, is_outgoing: false, text: "Ping" }),
    true,
  );
  assert.equal(afterUnreadIn[0].peer_read_ts, 3000);
  assert.equal(afterUnreadIn[0].last_read_in_ts, 1000);
  assert.equal(afterUnreadIn[0].unread_count, 1);

  const afterReadIn = bumpThreadList(
    afterUnreadIn,
    makeMsg({ thread_id: "t1", ts: 4000, is_outgoing: false, text: "Seen now" }),
    false,
  );
  assert.equal(afterReadIn[0].peer_read_ts, 4000);
  assert.equal(afterReadIn[0].last_read_in_ts, 4000);

  // Stale watermark from yesterday + outgoing message sent on phone today jumps only after outgoing reply
  const dayMs = 86_400_000;
  const phoneSyncedMsgs = [
    makeMsg({ msg_id: "y1", ts: 1000, is_outgoing: false }),
    makeMsg({ msg_id: "y2", ts: 2000, is_outgoing: false }),
    makeMsg({ msg_id: "today-out", ts: dayMs + 1000, is_outgoing: true }),
    makeMsg({ msg_id: "today-in", ts: dayMs + 2000, is_outgoing: false }),
  ];
  assert.equal(findFirstUnreadMsgId(phoneSyncedMsgs, 500, 1), "today-in");

  // If newest message after stale watermark is our own outgoing reply, nothing is unread
  const endingWithOutgoing = [
    makeMsg({ msg_id: "y1", ts: 1000, is_outgoing: false }),
    makeMsg({ msg_id: "today-out", ts: dayMs + 1000, is_outgoing: true }),
  ];
  assert.equal(findFirstUnreadMsgId(endingWithOutgoing, 500, 1), null);

  // Stale multi-device watermark (30 incoming messages spanning > 6h while unread_count = 1)
  const manyStaleMsgs = Array.from({ length: 30 }, (_, idx) =>
    makeMsg({
      msg_id: `m-${idx}`,
      ts: 1_000_000 + idx * 1_000_000,
      is_outgoing: false,
    }),
  );
  assert.equal(findFirstUnreadMsgId(manyStaleMsgs, 500_000, 1), "m-29");

  // Genuine busy group chat with 25 actual unread messages keeps the 1st of all 25
  assert.equal(findFirstUnreadMsgId(manyStaleMsgs, 5_000_000, 25), "m-5");
});

test("filterThreadsByFolder handles excluded_thread_ids, archived view, and moveArrayItem", () => {
  const threads = [
    makeThread({ id: "u1", kind: "direct", unread_count: 2 }),
    makeThread({ id: "g1", kind: "group", unread_count: 1 }),
    makeThread({ id: "g2", kind: "group", unread_count: 4 }),
    makeThread({ id: "arch1", kind: "direct", unread_count: 3, is_archived: true }),
  ];
  const folders = [
    {
      id: "work",
      name: "Work",
      icon: "briefcase",
      color: "",
      thread_ids: ["u1"],
      excluded_thread_ids: ["g2"],
      include_kinds: ["group" as const],
      only_unread: false,
      exclude_muted: false,
      position: 1,
      remote_label_id: null,
    },
  ];

  // Active lists exclude archived threads
  assert.deepEqual(
    filterThreadsByFolder(threads, "all", folders).map((t) => t.id),
    ["u1", "g1", "g2"],
  );
  assert.deepEqual(
    filterThreadsByFolder(threads, "unread", folders).map((t) => t.id),
    ["u1", "g1", "g2"],
  );
  assert.deepEqual(
    filterThreadsByFolder(threads, "custom:work", folders).map((t) => t.id),
    ["u1", "g1"],
  );
  assert.deepEqual(
    filterThreadsByFolder(threads, "archived", folders).map((t) => t.id),
    ["arch1"],
  );
  assert.equal(countFolderUnread(threads, "all", folders), 7);

  // Moving array items swaps order cleanly without mutating original array
  const src = ["a", "b", "c"];
  assert.deepEqual(moveArrayItem(src, 2, 0), ["c", "a", "b"]);
  assert.deepEqual(src, ["a", "b", "c"]);
});

test("applyThreadReadSync clears unread & advances watermark on phone read, marks unread on phone unread, and leaves unknown threads untouched", () => {
  const initial = [
    makeThread({ id: "t1", last_ts: 5000, unread_count: 4, last_read_in_ts: 2000 }),
    makeThread({ id: "t2", last_ts: 4000, unread_count: 0, last_read_in_ts: 4500 }),
    makeThread({ id: "t3", last_ts: 3000, unread_count: 2 }),
  ];

  // Phone reads t1 -> unread_count becomes 0 and last_read_in_ts advances to last_ts
  const afterReadT1 = applyThreadReadSync(initial, "t1", false);
  assert.notEqual(afterReadT1, initial);
  assert.equal(initial[0].unread_count, 4); // input not mutated
  assert.equal(afterReadT1[0].unread_count, 0);
  assert.equal(afterReadT1[0].last_read_in_ts, 5000);
  assert.equal(afterReadT1[1], initial[1]); // unrelated threads untouched

  // Phone reads t3 where last_read_in_ts was undefined -> advances from 0 to last_ts
  const afterReadT3 = applyThreadReadSync(afterReadT1, "t3", false);
  assert.equal(afterReadT3[2].unread_count, 0);
  assert.equal(afterReadT3[2].last_read_in_ts, 3000);

  // Phone reads t2 where last_read_in_ts (4500) > last_ts (4000) -> keeps higher watermark
  const afterReadT2 = applyThreadReadSync(afterReadT3, "t2", false);
  assert.equal(afterReadT2[1].unread_count, 0);
  assert.equal(afterReadT2[1].last_read_in_ts, 4500);

  // Phone marks read thread t1 as unread -> unread_count becomes at least 1
  const afterUnreadT1 = applyThreadReadSync(afterReadT2, "t1", true);
  assert.equal(afterUnreadT1[0].unread_count, 1);
  assert.equal(afterUnreadT1[0].last_read_in_ts, 5000);

  // Phone marks already-unread thread with count 4 -> preserves count >= 1 (keeps 4)
  const afterUnreadExisting = applyThreadReadSync(initial, "t1", true);
  assert.equal(afterUnreadExisting[0].unread_count, 4);

  // Unknown thread returns identical list reference without throwing
  assert.equal(applyThreadReadSync(initial, "missing-thread", false), initial);
  assert.equal(applyThreadReadSync(initial, "missing-thread", true), initial);
});

test("shouldAutoMarkReadOnFocus enforces focus, visibility, auth, lock, webApp, unread, and near-bottom guards", () => {
  const base = {
    authed: true,
    isLocked: false,
    hasActiveWebApp: false,
    webAppHubOpen: false,
    windowFocused: true,
    visibilityState: "visible",
    selectedId: "t1",
    pendingMarkReadId: "t1",
    activeThreadUnread: 0,
    distFromBottom: 20,
    isScrollable: true,
    chatPaneVisible: true,
    initialScrollReady: true,
  };

  // Happy paths: pendingMarkReadId matches OR activeThreadUnread > 0, near bottom or non-scrollable
  assert.equal(shouldAutoMarkReadOnFocus(base), true);
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, pendingMarkReadId: null, activeThreadUnread: 2 }),
    true,
  );
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, distFromBottom: 89 }), true);
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, distFromBottom: 0, isScrollable: false }),
    true,
  );
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, distFromBottom: null, isScrollable: false }),
    true,
  );

  // Scrolled up at or beyond 90px threshold -> must NOT auto-mark-read
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, distFromBottom: 90 }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, distFromBottom: 320 }), false);

  // Unmounted scroll container (e.g. WebApp unmounting or reply sub-panel) -> must NOT mark read
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, distFromBottom: null, isScrollable: undefined }),
    false,
  );
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, distFromBottom: null, isScrollable: true }),
    false,
  );

  // Window unfocused or hidden -> must NOT mark read
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, windowFocused: false }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, visibilityState: "hidden" }), false);

  // Unauthenticated, locked, WebApp open, WebApp Hub open, or chat pane hidden -> must NOT mark read
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, authed: false }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, isLocked: true }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, hasActiveWebApp: true }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, webAppHubOpen: true }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, chatPaneVisible: false }), false);
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, initialScrollReady: false }), false);

  // No selected thread or already read with no pending mark-read -> must NOT mark read
  assert.equal(shouldAutoMarkReadOnFocus({ ...base, selectedId: null }), false);
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, pendingMarkReadId: null, activeThreadUnread: 0 }),
    false,
  );
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, pendingMarkReadId: "other", activeThreadUnread: 0 }),
    false,
  );

  // Custom threshold works
  assert.equal(
    shouldAutoMarkReadOnFocus({ ...base, distFromBottom: 120, nearBottomThresholdPx: 140 }),
    true,
  );
});

test("applyOpenThreadSearchState, resolveFolderForOpenedThread, and computeThreadRenderLimitForTarget clear search and reveal opened threads", () => {
  const threads = [
    makeThread({ id: "dm-minh", title: "Trần Minh", kind: "direct", last_ts: 5000 }),
    makeThread({ id: "grp-1", title: "Nhóm Kỹ thuật", kind: "group", last_ts: 4000 }),
    makeThread({ id: "arch-1", title: "Lưu trữ", kind: "direct", last_ts: 3000, is_archived: true }),
    makeThread({ id: "phone-new", title: "Liên hệ mới", kind: "direct", last_ts: 0 }),
  ];
  const folders: ChatFolderDto[] = [
    {
      id: "work",
      name: "Work",
      icon: "briefcase",
      color: "",
      thread_ids: ["grp-1"],
      excluded_thread_ids: [],
      include_kinds: [],
      only_unread: false,
      exclude_muted: false,
      position: 1,
      remote_label_id: null,
    },
  ];

  // Clicking a search-filtered thread clears filterText and requests sidebar scroll while keeping folder
  assert.deepEqual(
    applyOpenThreadSearchState(
      { filterText: "Kỹ thuật", selectedId: "dm-minh", folder: "group" },
      "grp-1",
      threads,
      folders,
    ),
    {
      filterText: "",
      selectedId: "grp-1",
      folder: "group",
      clearedSearch: true,
      shouldScrollSidebar: true,
    },
  );

  // Clicking an already-selected thread while search is active still clears filterText
  assert.deepEqual(
    applyOpenThreadSearchState(
      { filterText: "Minh", selectedId: "dm-minh", folder: "all" },
      "dm-minh",
      threads,
      folders,
    ),
    {
      filterText: "",
      selectedId: "dm-minh",
      folder: "all",
      clearedSearch: true,
      shouldScrollSidebar: true,
    },
  );

  // Opening a direct chat from phone search while on "group" clears filterText and switches to "all"
  assert.deepEqual(
    applyOpenThreadSearchState(
      { filterText: "0912345678", selectedId: "grp-1", folder: "group" },
      "phone-new",
      threads,
      folders,
    ),
    {
      filterText: "",
      selectedId: "phone-new",
      folder: "all",
      clearedSearch: true,
      shouldScrollSidebar: true,
    },
  );

  // Opening an archived chat from QuickSearch switches folder to "archived" and clears active search
  assert.deepEqual(
    applyOpenThreadSearchState(
      { filterText: "abc", selectedId: "dm-minh", folder: "all" },
      "arch-1",
      threads,
      folders,
    ),
    {
      filterText: "",
      selectedId: "arch-1",
      folder: "archived",
      clearedSearch: true,
      shouldScrollSidebar: true,
    },
  );

  // Normal click with empty search preserves folder and avoids unnecessary scroll jump
  assert.deepEqual(
    applyOpenThreadSearchState(
      { filterText: "", selectedId: "dm-minh", folder: "custom:work" },
      "grp-1",
      threads,
      folders,
    ),
    {
      filterText: "",
      selectedId: "grp-1",
      folder: "custom:work",
      clearedSearch: false,
      shouldScrollSidebar: false,
    },
  );

  // resolveFolderForOpenedThread falls back cleanly for custom folder mismatch
  assert.equal(resolveFolderForOpenedThread(threads, "dm-minh", "custom:work", folders), "all");
  assert.equal(resolveFolderForOpenedThread(threads, "grp-1", "custom:work", folders), "custom:work");

  // computeThreadRenderLimitForTarget expands pagination when target is past index 99
  const manyThreads = Array.from({ length: 160 }, (_, idx) =>
    makeThread({ id: `t-${idx}`, last_ts: 10_000 - idx }),
  );
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, "t-0", 100), 100);
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, "t-99", 100), 100);
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, "t-100", 100), 200);
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, "t-142", 100), 200);
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, "missing", 100), 100);
  assert.equal(computeThreadRenderLimitForTarget(manyThreads, null, 100), 100);
});

test("normalizeReactionEmoji and normalizeReactions align canonical palette, variants, and extended mobile emojis", () => {
  assert.deepEqual(QUICK_REACTION_EMOJIS, ["👍", "❤️", "😆", "😮", "😢", "😠"]);

  // Canonical palette & wire shortcodes
  assert.equal(normalizeReactionEmoji("/-strong"), "👍");
  assert.equal(normalizeReactionEmoji("/-heart"), "❤️");
  assert.equal(normalizeReactionEmoji("❤"), "❤️");
  assert.equal(normalizeReactionEmoji(":>"), "😆");
  assert.equal(normalizeReactionEmoji(":o"), "😮");
  assert.equal(normalizeReactionEmoji(":-(("), "😢");
  assert.equal(normalizeReactionEmoji("😭"), "😢");
  assert.equal(normalizeReactionEmoji(":-h"), "😠");
  assert.equal(normalizeReactionEmoji("😡"), "😠");

  // Removal sentinels become empty string
  assert.equal(normalizeReactionEmoji(""), "");
  assert.equal(normalizeReactionEmoji("/-none"), "");
  assert.equal(normalizeReactionEmoji("NONE"), "");

  // Extended & custom mobile Unicode emojis pass through verbatim (distinct from 😆)
  assert.equal(normalizeReactionEmoji("😂"), "😂");
  assert.equal(normalizeReactionEmoji("🔥"), "🔥");
  assert.equal(normalizeReactionEmoji("🎉"), "🎉");

  // normalizeReactions merges canonical variants, drops count<=0, and preserves distinct emojis
  assert.deepEqual(
    normalizeReactions([
      { emoji: "❤", count: 1, me: false },
      { emoji: "❤️", count: 1, me: true },
      { emoji: "😂", count: 2, me: false },
      { emoji: "😆", count: 1, me: false },
      { emoji: "👍", count: 0, me: false },
      { emoji: "/-none", count: 1, me: false },
    ]),
    [
      { emoji: "❤️", count: 2, me: true },
      { emoji: "😂", count: 2, me: false },
      { emoji: "😆", count: 1, me: false },
    ],
  );
});

test("applyOptimisticReaction and reconcileUpdatedMessage handle 2-way add, switch, unreact, and pagination bounds", () => {
  // Add first reaction
  let reactions = applyOptimisticReaction([], "❤️");
  assert.deepEqual(reactions, [{ emoji: "❤️", count: 1, me: true }]);

  // Switch active reaction to 👍
  reactions = applyOptimisticReaction(reactions, "👍");
  assert.deepEqual(reactions, [{ emoji: "👍", count: 1, me: true }]);

  // Toggle same emoji removes it completely
  reactions = applyOptimisticReaction(reactions, "👍");
  assert.deepEqual(reactions, []);

  // Reacting onto an existing peer emoji increments and highlights `me`, switching preserves peer count
  const withPeer = [{ emoji: "😮", count: 1, me: false }];
  const joinedPeer = applyOptimisticReaction(withPeer, "😮");
  assert.deepEqual(joinedPeer, [{ emoji: "😮", count: 2, me: true }]);
  const switched = applyOptimisticReaction(joinedPeer, "😢");
  assert.deepEqual(switched, [
    { emoji: "😮", count: 1, me: false },
    { emoji: "😢", count: 1, me: true },
  ]);

  // Removing a phone-originated variant/custom reaction by clicking it works cleanly
  assert.deepEqual(
    applyOptimisticReaction([{ emoji: "❤", count: 1, me: true }], "❤️"),
    [],
  );
  assert.deepEqual(
    applyOptimisticReaction([{ emoji: "🔥", count: 2, me: true }], "🔥"),
    [{ emoji: "🔥", count: 1, me: false }],
  );

  // reconcileUpdatedMessage updates existing message in place (including removing last reaction)
  const windowMsgs = [
    makeMsg({ msg_id: "m10", ts: 5000, reactions: [{ emoji: "👍", count: 1, me: true }] }),
    makeMsg({ msg_id: "m11", ts: 6000, reactions: [] }),
  ];
  const afterPhoneSwitch = reconcileUpdatedMessage(
    windowMsgs,
    makeMsg({
      msg_id: "m10",
      ts: 5000,
      reactions: [
        { emoji: "❤", count: 1, me: true },
        { emoji: "😮", count: 1, me: false },
      ],
    }),
  );
  assert.equal(afterPhoneSwitch.length, 2);
  assert.deepEqual(afterPhoneSwitch[0].reactions, [
    { emoji: "❤️", count: 1, me: true },
    { emoji: "😮", count: 1, me: false },
  ]);

  const afterPhoneRemove = reconcileUpdatedMessage(
    afterPhoneSwitch,
    makeMsg({ msg_id: "m10", ts: 5000, reactions: [] }),
  );
  assert.deepEqual(afterPhoneRemove[0].reactions, []);

  // Reaction update for an older unloaded message (ts < oldest loaded) must NOT inject into window
  const olderUnloaded = makeMsg({
    msg_id: "m1-old",
    ts: 1000,
    reactions: [{ emoji: "❤️", count: 1, me: false }],
  });
  assert.equal(reconcileUpdatedMessage(afterPhoneRemove, olderUnloaded), afterPhoneRemove);
});

test("poll deduplication and merge helpers prevent duplicate optimistic poll cards and preserve metadata", () => {
  const fullPoll: PollDto = {
    poll_id: 9001,
    question: "Trưa nay ăn gì?",
    options: [
      { id: 1, content: "Phở", votes: 1, voted: true },
      { id: 2, content: "Cơm tấm", votes: 0, voted: false },
    ],
    allow_multi: true,
    allow_add_option: true,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
  const sparsePoll: PollDto = {
    poll_id: 9001,
    question: "Bình chọn",
    options: [],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };

  // mergePollDto keeps full question/options/flags when incoming payload is sparse
  const mergedSparse = mergePollDto(fullPoll, sparsePoll);
  assert.equal(mergedSparse.question, "Trưa nay ăn gì?");
  assert.equal(mergedSparse.options.length, 2);
  assert.equal(mergedSparse.allow_multi, true);
  assert.equal(mergedSparse.allow_add_option, true);

  // Optimistic poll followed by real server poll replaces synthetic msg_id and preserves options
  const optimisticMsg = makeMsg({
    msg_id: "poll-9001",
    thread_id: "grp-1",
    msg_type: "chat.poll",
    text: "Bình chọn: Trưa nay ăn gì?",
    attachment_name: `teza-poll:${JSON.stringify(fullPoll)}`,
    ts: 10_000,
    is_outgoing: true,
  });
  const realSparseServerMsg = makeMsg({
    msg_id: "srv-778899",
    thread_id: "grp-1",
    msg_type: "chat.poll",
    text: "Bình chọn: Trưa nay ăn gì?",
    attachment_name: `teza-poll:${JSON.stringify(sparsePoll)}`,
    ts: 10_500,
    is_outgoing: true,
  });

  const afterRealArrives = upsertMsg([optimisticMsg], realSparseServerMsg);
  assert.equal(afterRealArrives.length, 1);
  assert.equal(afterRealArrives[0].msg_id, "srv-778899");
  assert.ok(afterRealArrives[0].attachment_name?.includes("Phở"));

  // Reverse race: real server poll already present when local-poll-<id> arrives -> no duplicate
  const localOptimistic = makeMsg({
    ...optimisticMsg,
    msg_id: "local-poll-9001",
  });
  const afterReverseRace = upsertMsg([realSparseServerMsg], localOptimistic);
  assert.equal(afterReverseRace.length, 1);
  assert.equal(afterReverseRace[0].msg_id, "srv-778899");
  assert.ok(afterReverseRace[0].attachment_name?.includes("Cơm tấm"));

  // dedupeOptimisticPollMessages strips synthetic duplicates from loaded history pages
  const deduped = dedupeOptimisticPollMessages([optimisticMsg, localOptimistic, realSparseServerMsg]);
  assert.equal(deduped.length, 1);
  assert.equal(deduped[0].msg_id, "srv-778899");
  assert.ok(deduped[0].attachment_name?.includes("Phở"));

  // applyPollUpdateToMessages updates vote/lock state by poll_id even if msg_id changed
  const lockedPoll: PollDto = {
    ...fullPoll,
    options: [
      { id: 1, content: "Phở", votes: 2, voted: true },
      { id: 2, content: "Cơm tấm", votes: 1, voted: false },
      { id: 3, content: "Bún bò", votes: 1, voted: true },
    ],
    closed: true,
  };
  const updatedList = applyPollUpdateToMessages(deduped, 9001, lockedPoll, "poll-9001");
  assert.equal(updatedList.length, 1);
  assert.equal(updatedList[0].msg_id, "srv-778899");
  assert.ok(updatedList[0].attachment_name?.includes("Bún bò"));
  assert.ok(updatedList[0].attachment_name?.includes('"closed":true'));

  // Sparse incoming with real question & empty options preserves all flags (allow_multi/allow_add_option/is_anonymous)
  const anonFullPoll: PollDto = { ...fullPoll, is_anonymous: true };
  const sparseWithRealQuestion: PollDto = {
    poll_id: 9001,
    question: "Trưa nay ăn gì?",
    options: [],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
  const mergedRealQ = mergePollDto(anonFullPoll, sparseWithRealQuestion);
  assert.equal(mergedRealQ.question, "Trưa nay ăn gì?");
  assert.equal(mergedRealQ.options.length, 2);
  assert.equal(mergedRealQ.allow_multi, true);
  assert.equal(mergedRealQ.allow_add_option, true);
  assert.equal(mergedRealQ.is_anonymous, true);

  // Options-only partial payload (generic question + default false flags) preserves existing question & flags
  const optionsOnlyPartial: PollDto = {
    poll_id: 9001,
    question: "Bình chọn",
    options: [
      { id: 1, content: "Phở", votes: 3, voted: true },
      { id: 2, content: "Cơm tấm", votes: 1, voted: false },
    ],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
  const mergedOptsOnly = mergePollDto(anonFullPoll, optionsOnlyPartial);
  assert.equal(mergedOptsOnly.question, "Trưa nay ăn gì?");
  assert.equal(mergedOptsOnly.options[0].votes, 3);
  assert.equal(mergedOptsOnly.allow_multi, true);
  assert.equal(mergedOptsOnly.allow_add_option, true);
  assert.equal(mergedOptsOnly.is_anonymous, true);

  // History re-sync of an old open creation message never re-opens an already locked poll
  const resyncOpen = mergePollDto(lockedPoll, { ...fullPoll, closed: false });
  assert.equal(resyncOpen.closed, true);
});

test("mergePollDto protects real question from vote-activity status strings", () => {
  const basePoll: PollDto = {
    poll_id: 9101,
    question: "Trưa nay ăn gì?",
    options: [
      { id: 1, content: "Phở", votes: 1, voted: true },
      { id: 2, content: "Bún bò", votes: 0, voted: false },
    ],
    allow_multi: true,
    allow_add_option: true,
    is_anonymous: true,
    expired_time: 1_800_000_000_000,
    closed: false,
  };

  const activityStrings = [
    "đã tham gia bình chọn",
    "An đã tham gia bình chọn",
    "Minh đã bình chọn",
    "đã bình chọn",
    "Lan đã khóa bình chọn",
    "Hùng đã thêm lựa chọn vào bình chọn",
    "voted",
    "Alice voted",
    "Bob voted in the poll",
  ];

  for (const statusText of activityStrings) {
    assert.equal(isPollVoteActivityQuestion(statusText), true, `Expected activity text: ${statusText}`);
    const merged = mergePollDto(basePoll, {
      poll_id: 9101,
      question: statusText,
      options: [
        { id: 1, content: "Phở", votes: 2, voted: true },
        { id: 2, content: "Bún bò", votes: 1, voted: false },
      ],
      allow_multi: false,
      allow_add_option: false,
      is_anonymous: false,
      expired_time: 0,
      closed: false,
    });
    assert.equal(merged.question, "Trưa nay ăn gì?", `Must not overwrite question with "${statusText}"`);
    assert.equal(merged.options[0].votes, 2);
    assert.equal(merged.options[1].votes, 1);
    assert.equal(merged.allow_multi, true);
    assert.equal(merged.allow_add_option, true);
    assert.equal(merged.is_anonymous, true);
    assert.equal(merged.expired_time, 1_800_000_000_000);
  }

  // Genuine user question mentioning voting with "?" is NOT classified as vote activity
  assert.equal(isPollVoteActivityQuestion("Bạn đã bình chọn chưa?"), false);
  assert.equal(isPollVoteActivityQuestion("Trưa nay ăn gì?"), false);

  // Reverse direction: real question upgrades an existing vote-activity placeholder
  const activityExisting: PollDto = {
    ...basePoll,
    question: "An đã tham gia bình chọn",
  };
  const upgraded = mergePollDto(activityExisting, {
    ...basePoll,
    question: "Tối nay đi đâu?",
  });
  assert.equal(upgraded.question, "Tối nay đi đâu?");
});

test("dedupeOptimisticPollMessages collapses multiple real server poll rows into one canonical card", () => {
  const createPoll: PollDto = {
    poll_id: 9202,
    question: "Chọn địa điểm team building",
    options: [
      { id: 1, content: "Đà Lạt", votes: 0, voted: false },
      { id: 2, content: "Phú Quốc", votes: 0, voted: false },
    ],
    allow_multi: true,
    allow_add_option: true,
    is_anonymous: true,
    expired_time: 0,
    closed: false,
  };
  const vote1Poll: PollDto = {
    poll_id: 9202,
    question: "Bình đã tham gia bình chọn",
    options: [
      { id: 1, content: "Đà Lạt", votes: 1, voted: false },
      { id: 2, content: "Phú Quốc", votes: 0, voted: false },
    ],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
  const vote2Poll: PollDto = {
    poll_id: 9202,
    question: "Chi đã bình chọn",
    options: [
      { id: 1, content: "Đà Lạt", votes: 1, voted: false },
      { id: 2, content: "Phú Quốc", votes: 2, voted: true },
    ],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: true,
  };

  const realCreate = makeMsg({
    msg_id: "srv-create-100",
    thread_id: "grp-team",
    sender_id: "u-an",
    sender_name: "An",
    is_outgoing: true,
    msg_type: "chat.poll",
    text: "Bình chọn: Chọn địa điểm team building",
    attachment_name: `teza-poll:${JSON.stringify(createPoll)}`,
    ts: 10_000,
  });
  const chatBetween1 = makeMsg({ msg_id: "m-chat-1", thread_id: "grp-team", text: "Vote đi mn", ts: 11_000 });
  const realVote1 = makeMsg({
    msg_id: "srv-vote-200",
    thread_id: "grp-team",
    sender_id: "u-binh",
    sender_name: "Bình",
    is_outgoing: false,
    msg_type: "chat.poll",
    text: "Bình đã tham gia bình chọn",
    attachment_name: `teza-poll:${JSON.stringify(vote1Poll)}`,
    ts: 12_000,
  });
  const chatBetween2 = makeMsg({ msg_id: "m-chat-2", thread_id: "grp-team", text: "Ok nha", ts: 13_000 });
  const realVote2 = makeMsg({
    msg_id: "srv-vote-300",
    thread_id: "grp-team",
    sender_id: "u-chi",
    sender_name: "Chi",
    is_outgoing: false,
    msg_type: "chat.poll",
    text: "Chi đã bình chọn",
    attachment_name: `teza-poll:${JSON.stringify(vote2Poll)}`,
    ts: 15_000,
  });

  const deduped = dedupeOptimisticPollMessages([
    realCreate,
    chatBetween1,
    realVote1,
    chatBetween2,
    realVote2,
  ]);
  assert.equal(deduped.length, 3);
  assert.deepEqual(
    deduped.map((m) => m.msg_id),
    ["srv-create-100", "m-chat-1", "m-chat-2"],
  );
  // Preserves earliest real message's metadata and position (never jumps to bottom)
  assert.equal(deduped[0].ts, 10_000);
  assert.equal(deduped[0].sender_id, "u-an");
  assert.equal(deduped[0].sender_name, "An");
  assert.equal(deduped[0].is_outgoing, true);

  const merged = parsePollPayload(deduped[0])!;
  assert.equal(merged.poll_id, 9202);
  assert.equal(merged.question, "Chọn địa điểm team building");
  assert.equal(merged.options[0].votes, 1);
  assert.equal(merged.options[1].votes, 2);
  assert.equal(merged.allow_multi, true);
  assert.equal(merged.allow_add_option, true);
  assert.equal(merged.is_anonymous, true);
  assert.equal(merged.closed, true);

  // Out-of-order input still selects earliest real message by timestamp and folds newer votes on top
  const outOfOrder = dedupePollMessages([realVote2, realCreate, realVote1]);
  assert.equal(outOfOrder.length, 1);
  assert.equal(outOfOrder[0].msg_id, "srv-create-100");
  assert.equal(outOfOrder[0].ts, 10_000);
  const outOfOrderPoll = parsePollPayload(outOfOrder[0])!;
  assert.equal(outOfOrderPoll.question, "Chọn địa điểm team building");
  assert.equal(outOfOrderPoll.options[1].votes, 2);
  assert.equal(outOfOrderPoll.closed, true);

  // Different threads sharing same poll_id are kept separate; no-op returns same reference
  const otherThreadPoll = makeMsg({ ...realVote1, msg_id: "srv-other-thread", thread_id: "grp-other" });
  const twoThreads = [realCreate, otherThreadPoll];
  assert.equal(dedupeOptimisticPollMessages(twoThreads), twoThreads);
});

test("upsertMsg and reconcileUpdatedMessage merge real-vs-real poll vote messages in place without duplicating", () => {
  const createPoll: PollDto = {
    poll_id: 9303,
    question: "Họp lúc mấy giờ?",
    options: [
      { id: 1, content: "9:00", votes: 1, voted: true },
      { id: 2, content: "14:00", votes: 0, voted: false },
    ],
    allow_multi: false,
    allow_add_option: true,
    is_anonymous: false,
    expired_time: 0,
    closed: false,
  };
  const realCreate = makeMsg({
    msg_id: "srv-poll-orig",
    thread_id: "grp-1",
    sender_id: "u1",
    sender_name: "An",
    is_outgoing: true,
    msg_type: "chat.poll",
    text: "Bình chọn: Họp lúc mấy giờ?",
    attachment_name: `teza-poll:${JSON.stringify(createPoll)}`,
    ts: 20_000,
  });
  const followUpChat = makeMsg({
    msg_id: "m-after",
    thread_id: "grp-1",
    text: "Mọi người vote sớm nhé",
    ts: 21_000,
  });
  const realVote = makeMsg({
    msg_id: "srv-poll-vote-2",
    thread_id: "grp-1",
    sender_id: "u2",
    sender_name: "Bình",
    is_outgoing: false,
    msg_type: "chat.poll",
    text: "Bình đã tham gia bình chọn",
    attachment_name: `teza-poll:${JSON.stringify({
      poll_id: 9303,
      question: "Bình đã tham gia bình chọn",
      options: [
        { id: 1, content: "9:00", votes: 1, voted: true },
        { id: 2, content: "14:00", votes: 2, voted: false },
      ],
      allow_multi: false,
      allow_add_option: false,
      is_anonymous: false,
      expired_time: 0,
      closed: false,
    } satisfies PollDto)}`,
    ts: 25_000,
  });

  // Live vote with different msg_id merges into existing canonical card in place (no duplicate, no jump)
  const afterVote = upsertMsg([realCreate, followUpChat], realVote);
  assert.equal(afterVote.length, 2);
  assert.deepEqual(
    afterVote.map((m) => m.msg_id),
    ["srv-poll-orig", "m-after"],
  );
  assert.equal(afterVote[0].ts, 20_000);
  assert.equal(afterVote[0].sender_id, "u1");
  assert.equal(afterVote[0].sender_name, "An");
  assert.equal(afterVote[0].is_outgoing, true);
  const pollAfterVote = parsePollPayload(afterVote[0])!;
  assert.equal(pollAfterVote.question, "Họp lúc mấy giờ?");
  assert.equal(pollAfterVote.options[1].votes, 2);
  assert.equal(pollAfterVote.allow_add_option, true);

  // reconcileUpdatedMessage with another real vote/lock msg_id also preserves original card in place
  const realLock = makeMsg({
    msg_id: "srv-poll-lock-3",
    thread_id: "grp-1",
    sender_id: "u1",
    sender_name: "An",
    is_outgoing: true,
    msg_type: "chat.poll",
    text: "An đã khóa bình chọn",
    attachment_name: `teza-poll:${JSON.stringify({
      poll_id: 9303,
      question: "An đã khóa bình chọn",
      options: [],
      allow_multi: false,
      allow_add_option: false,
      is_anonymous: false,
      expired_time: 0,
      closed: true,
    } satisfies PollDto)}`,
    ts: 30_000,
  });
  const afterLock = reconcileUpdatedMessage(afterVote, realLock);
  assert.equal(afterLock.length, 2);
  assert.equal(afterLock[0].msg_id, "srv-poll-orig");
  assert.equal(afterLock[0].ts, 20_000);
  const pollAfterLock = parsePollPayload(afterLock[0])!;
  assert.equal(pollAfterLock.question, "Họp lúc mấy giờ?");
  assert.equal(pollAfterLock.options[1].votes, 2);
  assert.equal(pollAfterLock.closed, true);

  // Out-of-order upsert: older creation card arriving after newer vote keeps newer votes + recovers question
  const outOfOrderUpsert = upsertMsg([realVote], realCreate);
  assert.equal(outOfOrderUpsert.length, 1);
  assert.equal(outOfOrderUpsert[0].msg_id, "srv-poll-vote-2");
  const outOfOrderPoll = parsePollPayload(outOfOrderUpsert[0])!;
  assert.equal(outOfOrderPoll.question, "Họp lúc mấy giờ?");
  assert.equal(outOfOrderPoll.options[1].votes, 2);
  assert.equal(outOfOrderPoll.allow_add_option, true);
});

test("applyPollUpdateToMessages updates all matching rows and dedupes stray duplicates for pollId", () => {
  const createPoll: PollDto = {
    poll_id: 9404,
    question: "Thứ 6 đi ăn lẩu?",
    options: [
      { id: 1, content: "Đi", votes: 1, voted: true },
      { id: 2, content: "Bận", votes: 0, voted: false },
    ],
    allow_multi: false,
    allow_add_option: true,
    is_anonymous: true,
    expired_time: 0,
    closed: false,
  };
  const optimisticRow = makeMsg({
    msg_id: "poll-9404",
    thread_id: "grp-1",
    msg_type: "chat.poll",
    attachment_name: `teza-poll:${JSON.stringify(createPoll)}`,
    ts: 9_900,
    is_outgoing: true,
  });
  const realCreate = makeMsg({
    msg_id: "srv-9404-a",
    thread_id: "grp-1",
    sender_id: "u1",
    sender_name: "An",
    is_outgoing: true,
    msg_type: "chat.poll",
    attachment_name: `teza-poll:${JSON.stringify({ ...createPoll, options: [], allow_add_option: false, is_anonymous: false })}`,
    ts: 10_000,
  });
  const otherMsg = makeMsg({ msg_id: "m-normal", thread_id: "grp-1", text: "ok", ts: 11_000 });
  const strayVoteDup = makeMsg({
    msg_id: "srv-9404-b",
    thread_id: "grp-1",
    sender_id: "u2",
    sender_name: "Bình",
    is_outgoing: false,
    msg_type: "chat.poll",
    attachment_name: `teza-poll:${JSON.stringify({
      poll_id: 9404,
      question: "Bình đã tham gia bình chọn",
      options: [
        { id: 1, content: "Đi", votes: 2, voted: true },
        { id: 2, content: "Bận", votes: 0, voted: false },
      ],
      allow_multi: false,
      allow_add_option: false,
      is_anonymous: false,
      expired_time: 0,
      closed: false,
    } satisfies PollDto)}`,
    ts: 12_000,
  });
  const unrelatedPoll = makeMsg({
    msg_id: "srv-9999",
    thread_id: "grp-1",
    msg_type: "chat.poll",
    attachment_name: `teza-poll:${JSON.stringify({ ...createPoll, poll_id: 9999, question: "Khác" })}`,
    ts: 13_000,
  });

  const serverUpdate: PollDto = {
    poll_id: 9404,
    question: "voted",
    options: [
      { id: 1, content: "Đi", votes: 3, voted: true },
      { id: 2, content: "Bận", votes: 1, voted: false },
    ],
    allow_multi: false,
    allow_add_option: false,
    is_anonymous: false,
    expired_time: 0,
    closed: true,
  };

  const result = applyPollUpdateToMessages(
    [optimisticRow, realCreate, otherMsg, strayVoteDup, unrelatedPoll],
    9404,
    serverUpdate,
    "srv-9404-b",
  );

  assert.equal(result.length, 3);
  assert.deepEqual(
    result.map((m) => m.msg_id),
    ["srv-9404-a", "m-normal", "srv-9999"],
  );
  assert.equal(result[0].ts, 10_000);
  const updatedPoll = parsePollPayload(result[0])!;
  assert.equal(updatedPoll.question, "Thứ 6 đi ăn lẩu?");
  assert.equal(updatedPoll.options[0].votes, 3);
  assert.equal(updatedPoll.options[1].votes, 1);
  assert.equal(updatedPoll.allow_add_option, true);
  assert.equal(updatedPoll.is_anonymous, true);
  assert.equal(updatedPoll.closed, true);

  // Unrelated poll untouched
  assert.equal(parsePollPayload(result[2])!.question, "Khác");
});
