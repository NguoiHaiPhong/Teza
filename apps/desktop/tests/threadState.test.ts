import test from "node:test";
import assert from "node:assert/strict";
import type { MessageDto, ThreadDto } from "../src/types.ts";
import { bumpThreadList, compareThreads, upsertMsg } from "../src/utils/threadState.ts";

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

test("compareThreads orders unread first, then newest timestamp, then pinned tie-break", () => {
  const readOld = makeThread({ id: "a", last_ts: 100, unread_count: 0, is_pinned: false });
  const readNew = makeThread({ id: "b", last_ts: 200, unread_count: 0, is_pinned: false });
  const readSamePinned = makeThread({ id: "c", last_ts: 200, unread_count: 0, is_pinned: true });
  const unreadOld = makeThread({ id: "d", last_ts: 50, unread_count: 1, is_pinned: false });

  const sorted = [readOld, readNew, readSamePinned, unreadOld].sort(compareThreads);
  assert.deepEqual(
    sorted.map((t) => t.id),
    ["d", "c", "b", "a"],
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
