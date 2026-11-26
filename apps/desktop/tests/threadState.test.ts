import test from "node:test";
import assert from "node:assert/strict";
import type { ChatFolderDto, MessageDto, ThreadDto } from "../src/types.ts";
import {
  applyThreadReadSync,
  bumpThreadList,
  compareThreads,
  computeLayoutMode,
  countFolderUnread,
  extractFirstUrl,
  filterThreadsByFolder,
  findFirstUnreadMsgId,
  getDisplayBodyText,
  looksLikePhoneQuery,
  moveArrayItem,
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
  assert.equal(looksLikePhoneQuery("2025"), false);
  assert.equal(looksLikePhoneQuery("Minh Anh"), false);
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
