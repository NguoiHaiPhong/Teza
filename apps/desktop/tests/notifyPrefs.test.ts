import test from "node:test";
import assert from "node:assert/strict";
import type { MessageDto, ThreadDto } from "../src/types.ts";
import {
  computeTotalUnreadBadge,
  DEFAULT_NOTIFY_PREFS,
  formatNotificationContent,
  isMessageMentioningMe,
  isThreadCurrentlyMuted,
  normalizeNotifyPrefs,
  shouldNotifyMessage,
} from "../src/utils/notifyPrefs.ts";

function makeMsg(overrides: Partial<MessageDto>): MessageDto {
  return {
    msg_id: "m1",
    thread_id: "t1",
    sender_id: "u2",
    sender_name: "An",
    text: "Xin chao",
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
    title: "Minh",
    avatar: "",
    kind: "direct",
    last_preview: "",
    last_ts: 1_700_000_000_000,
    unread_count: 1,
    is_pinned: false,
    is_muted: false,
    ...overrides,
  };
}

test("normalizeNotifyPrefs restores defaults and clamps invalid values", () => {
  const res = normalizeNotifyPrefs({
    durationSec: 99,
    position: "center" as unknown as "bottom-right",
    previewMode: "full",
    notifyGroup: false,
  });
  assert.equal(res.durationSec, DEFAULT_NOTIFY_PREFS.durationSec);
  assert.equal(res.position, "bottom-right");
  assert.equal(res.notifyGroup, false);
  assert.equal(res.notifyDirect, true);
});

test("isThreadCurrentlyMuted respects forever mute and timed mute expiry", () => {
  const now = 1_700_000_100_000;
  assert.equal(isThreadCurrentlyMuted(makeThread({ is_muted: false }), now), false);
  assert.equal(
    isThreadCurrentlyMuted(makeThread({ is_muted: true, muted_until_ts: 0 }), now),
    true,
  );
  assert.equal(
    isThreadCurrentlyMuted(makeThread({ is_muted: true, muted_until_ts: now + 60_000 }), now),
    true,
  );
  assert.equal(
    isThreadCurrentlyMuted(makeThread({ is_muted: true, muted_until_ts: now - 1 }), now),
    false,
  );
});

test("shouldNotifyMessage filters direct/group, focus state, and muted mention override", () => {
  const dm = makeThread({ id: "u2", kind: "direct", is_muted: false });
  const grpMuted = makeThread({ id: "g1", title: "Nhom Dev", kind: "group", is_muted: true, muted_until_ts: 0 });
  const cloud = makeThread({ id: "cloud-me", kind: "cloud" });

  // Cloud and outgoing never notify
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ is_outgoing: true }),
      thread: dm,
      isActiveThread: false,
      windowFocused: false,
      prefs: DEFAULT_NOTIFY_PREFS,
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: false, playSound: false },
  );
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "cloud-me" }),
      thread: cloud,
      isActiveThread: false,
      windowFocused: false,
      prefs: DEFAULT_NOTIFY_PREFS,
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: false, playSound: false },
  );

  // Disabled notifyDirect blocks 1-1 notifications
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "u2" }),
      thread: dm,
      isActiveThread: false,
      windowFocused: false,
      prefs: { ...DEFAULT_NOTIFY_PREFS, notifyDirect: false },
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: false, playSound: false },
  );

  // Muted group blocks normal messages but allows @mention or reply to me
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "g1", text: "Hop chieu nay" }),
      thread: grpMuted,
      isActiveThread: false,
      windowFocused: false,
      prefs: DEFAULT_NOTIFY_PREFS,
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: false, playSound: false },
  );
  assert.equal(isMessageMentioningMe(makeMsg({ text: "Nho @Tuan check PR" }), "me", "Tuan"), true);
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "g1", text: "Nho @Tuan check PR" }),
      thread: grpMuted,
      isActiveThread: false,
      windowFocused: false,
      prefs: DEFAULT_NOTIFY_PREFS,
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: true, playSound: true },
  );

  // When window focused on another chat: sound plays, popup respects notifyWhenFocused
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "u2" }),
      thread: dm,
      isActiveThread: false,
      windowFocused: true,
      prefs: { ...DEFAULT_NOTIFY_PREFS, notifyWhenFocused: false },
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: false, playSound: true },
  );
  assert.deepEqual(
    shouldNotifyMessage({
      msg: makeMsg({ thread_id: "u2" }),
      thread: dm,
      isActiveThread: false,
      windowFocused: true,
      prefs: { ...DEFAULT_NOTIFY_PREFS, notifyWhenFocused: true },
      myUid: "me",
      myName: "Tuan",
    }),
    { showPopup: true, playSound: true },
  );
});

test("formatNotificationContent and computeTotalUnreadBadge support privacy & badge modes", () => {
  const grp = makeThread({ id: "g1", title: "Nhom Du An", kind: "group" });
  const msg = makeMsg({ thread_id: "g1", sender_name: "An", text: "Da gui bao cao" });

  assert.deepEqual(formatNotificationContent({ msg, thread: grp, previewMode: "full" }), {
    title: "Nhom Du An",
    body: "An: Da gui bao cao",
  });
  assert.deepEqual(formatNotificationContent({ msg, thread: grp, previewMode: "sender_only" }), {
    title: "Nhom Du An",
    body: "Đã gửi một tin nhắn mới",
  });
  assert.deepEqual(formatNotificationContent({ msg, thread: grp, previewMode: "hidden" }), {
    title: "Teza",
    body: "Bạn có tin nhắn mới",
  });

  const now = 1_700_000_100_000;
  const list = [
    makeThread({ id: "a", unread_count: 3, is_muted: false }),
    makeThread({ id: "b", unread_count: 5, is_muted: true, muted_until_ts: 0 }),
    makeThread({ id: "c", unread_count: 2, is_muted: true, muted_until_ts: now - 10 }),
  ];
  assert.equal(
    computeTotalUnreadBadge(list, { badgeMode: "unread_messages", includeMutedInBadge: false }, now),
    5,
  );
  assert.equal(
    computeTotalUnreadBadge(list, { badgeMode: "unread_chats", includeMutedInBadge: false }, now),
    2,
  );
  assert.equal(
    computeTotalUnreadBadge(list, { badgeMode: "unread_chats", includeMutedInBadge: true }, now),
    3,
  );
});

test("isMessageMentioningMe handles boundaries, emails, Vietnamese folding/NFD, @everyone, and @tất cả", () => {
  // Email addresses must NOT trigger @all or user mentions
  assert.equal(
    isMessageMentioningMe(
      makeMsg({ text: "Liên hệ contact@allianz.com hoặc foo@an.com nhé" }),
      "u-an",
      "An",
    ),
    false,
  );
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "Send to team@everyone.org" }), "u-an", "An"),
    false,
  );

  // Prefix word-boundary guard: @Anh must NOT match user named "An"
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "@Anh xem giúp PR nhé" }), "u-an", "An"),
    false,
  );
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "Chào @An, xem giúp nhé" }), "u-an", "An"),
    true,
  );

  // Vietnamese accent-folding and NFD decomposed input match identically
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "@Dao Nu please check" }), "u-nu", "Đào Nụ"),
    true,
  );
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "@Lo\u0302\u0323c xem báo cáo" }), "u-loc", "Lộc"),
    true,
  );
  assert.equal(
    isMessageMentioningMe(makeMsg({ text: "@Lộc xem báo cáo" }), "u-loc", "Lo\u0302\u0323c"),
    true,
  );

  // All-mention aliases: @All, @everyone, @Mọi người, @tất cả, @tat ca
  for (const text of [
    "@All họp 3h nhé",
    "@everyone please join",
    "@Mọi người kiểm tra mail",
    "@tất cả vào phòng họp",
    "@tat ca xem lịch mới",
  ]) {
    assert.equal(
      isMessageMentioningMe(makeMsg({ text }), "u-an", "An"),
      true,
      `expected "${text}" to mention me`,
    );
  }

  // Quoted reply to me still triggers mention override
  assert.equal(
    isMessageMentioningMe(
      makeMsg({
        text: "Ok anh",
        quote: { msg_id: "q1", sender_name: "Bạn", snippet: "Nhớ gửi file" },
      }),
      "u-an",
      "An",
    ),
    true,
  );
});
