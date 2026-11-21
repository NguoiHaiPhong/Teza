import test from "node:test";
import assert from "node:assert/strict";
import type { MessageDto } from "../src/types.ts";
import {
  formatReminderCountdown,
  isPollMessage,
  isReminderMessage,
  parsePollPayload,
  parseReminderPayload,
} from "../src/utils/richMessage.ts";

function makeMsg(overrides: Partial<MessageDto>): MessageDto {
  return {
    msg_id: "m1",
    thread_id: "t1",
    sender_id: "u1",
    sender_name: "An",
    text: "",
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

test("parseReminderPayload parses metadata and rescues legacy reminder cached as image", () => {
  const modern = makeMsg({
    msg_type: "chat.reminder",
    text: "Nhắc hẹn: Nộp báo cáo",
    attachment_url: null,
    attachment_name:
      'teza-reminder:{"title":"Nộp báo cáo","startTime":1700003600000,"repeat":2,"reminderId":"r1"}',
  });
  assert.equal(isReminderMessage(modern), true);
  assert.equal(isPollMessage(modern), false);
  const parsed = parseReminderPayload(modern);
  assert.deepEqual(parsed, {
    title: "Nộp báo cáo",
    startTime: 1700003600000,
    repeat: 2,
    reminderId: "r1",
  });

  const legacy = makeMsg({
    msg_type: "chat.reminder",
    text: "[Nhắc hẹn]",
    attachment_url: "https://stc-chat.zdn.vn/images/calendar_thumb.png",
    attachment_name: null,
    ts: 1_700_000_000_000,
  });
  assert.equal(isReminderMessage(legacy), true);
  assert.equal(parseReminderPayload(legacy)?.title, "Nhắc hẹn");

  const ecardLegacy = makeMsg({
    msg_type: "chat.ecard",
    text: "Thứ Bảy, 21 tháng 11 lúc 15:00",
    attachment_url: "https://cdn.example/upload/media/2018/8/3/reminder_2_1533262255293_116971.png",
    attachment_name: "⏰ uống nước để lấy năng lượng...",
    ts: new Date(2026, 10, 21, 14, 0, 0).getTime(),
  });
  assert.equal(isReminderMessage(ecardLegacy), true);
  const parsedEcard = parseReminderPayload(ecardLegacy);
  assert.ok(parsedEcard);
  assert.equal(parsedEcard.title, "uống nước để lấy năng lượng...");
  assert.equal(parsedEcard.scheduleText, "Thứ Bảy, 21 tháng 11 lúc 15:00");
  assert.equal(new Date(parsedEcard.startTime).getDate(), 21);
  assert.equal(new Date(parsedEcard.startTime).getMonth(), 10);
  assert.equal(new Date(parsedEcard.startTime).getHours(), 15);
  assert.equal(new Date(parsedEcard.startTime).getMinutes(), 0);
});

test("parsePollPayload parses structured poll metadata cleanly", () => {
  const pollMsg = makeMsg({
    msg_id: "99",
    msg_type: "chat.poll",
    text: "Bình chọn: Địa điểm họp",
    attachment_name:
      'teza-poll:{"poll_id":42,"question":"Địa điểm họp","options":[{"id":1,"content":"Phòng A","votes":3,"voted":true}],"allow_multi":true,"allow_add_option":false,"is_anonymous":false,"expired_time":0,"closed":false}',
  });
  assert.equal(isPollMessage(pollMsg), true);
  const poll = parsePollPayload(pollMsg);
  assert.ok(poll);
  assert.equal(poll.poll_id, 42);
  assert.equal(poll.allow_multi, true);
  assert.equal(poll.options.length, 1);
  assert.equal(poll.options[0].votes, 3);
});

test("formatReminderCountdown formats relative time accurately across locales", () => {
  const now = 1_700_000_000_000;
  assert.equal(formatReminderCountdown(now + 45 * 60_000, now, "vi"), "Còn 45 phút");
  assert.equal(formatReminderCountdown(now + 90 * 60_000, now, "en"), "In 1h 30m");
  assert.equal(formatReminderCountdown(now - 120_000, now, "ko"), "지남");
});
