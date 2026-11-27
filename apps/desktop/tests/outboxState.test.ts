import test from "node:test";
import assert from "node:assert/strict";
import type { MessageDto, PendingAttachment } from "../src/types.ts";
import {
  MAX_STAGED_ATTACHMENTS,
  appendStagedAttachments,
  buildAttachmentSendBatch,
  collectAllOutboxEntries,
  collectRevocableAttachmentUrls,
  diffRevocableAttachmentUrls,
  findOutboxEntry,
  getOutboxMessages,
  markOutboxFailed,
  mergeOutboxIntoMessages,
  removeOutboxEntriesForThread,
  removeOutboxEntry,
  removeStagedAttachmentAt,
  summarizeThreadOutbox,
  upsertOutboxEntry,
  type OutboxByThread,
} from "../src/utils/outboxState.ts";

function makeMsg(overrides: Partial<MessageDto> & { msg_id: string; thread_id: string }): MessageDto {
  return {
    sender_id: "me",
    sender_name: "Bạn",
    text: "hello",
    msg_type: "chat.text",
    ts: 1_700_000_000_000,
    is_outgoing: true,
    is_recalled: false,
    is_pinned: false,
    quote: null,
    reactions: [],
    attachment_url: null,
    attachment_name: null,
    ...overrides,
  };
}

function makePathAtt(path: string, fileName?: string, isImage = false): PendingAttachment {
  return {
    kind: "path",
    path,
    fileName: fileName ?? path.split(/[\\/]/).pop() ?? "file.bin",
    sizeBytes: 1024,
    isImage,
  };
}

function makeRawAtt(
  fileName: string,
  previewUrl: string | null,
  isImage = true,
): PendingAttachment {
  return {
    kind: "raw",
    file: { name: fileName, size: 2048, type: isImage ? "image/png" : "application/pdf" } as File,
    fileName,
    sizeBytes: 2048,
    isImage,
    previewUrl,
  };
}

test("thread switch during in-flight send restores sending bubble and preserves failed Retry/Discard payload", () => {
  const quoteTarget = makeMsg({
    msg_id: "srv-1",
    thread_id: "thread-A",
    sender_id: "peer-1",
    sender_name: "Minh",
    text: "Gửi báo cáo giúp mình nhé",
    is_outgoing: false,
    ts: 1_700_000_001_000,
  });

  const optText = makeMsg({
    msg_id: "opt-text-1",
    thread_id: "thread-A",
    text: "Mình gửi ngay đây",
    ts: 1_700_000_005_000,
    quote: {
      msg_id: quoteTarget.msg_id,
      sender_name: quoteTarget.sender_name,
      snippet: quoteTarget.text,
    },
    send_status: "sending",
  });

  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, { msg: optText, quoteTarget });

  // User switches to thread B, then switches back to thread A while still sending.
  const sqliteMessagesA = [quoteTarget];
  const mergedWhileSending = mergeOutboxIntoMessages("thread-A", sqliteMessagesA, outbox);
  assert.equal(mergedWhileSending.length, 2);
  assert.equal(mergedWhileSending[1].msg_id, "opt-text-1");
  assert.equal(mergedWhileSending[1].send_status, "sending");

  // User switches to thread B and the send in thread A fails over the network.
  const failedRes = markOutboxFailed(outbox, "thread-A", "opt-text-1", "Network timeout");
  outbox = failedRes.next;
  assert.equal(failedRes.updated?.msg.send_status, "failed");
  assert.equal(failedRes.updated?.msg.send_error, "Network timeout");
  assert.equal(failedRes.updated?.quoteTarget?.msg_id, "srv-1");

  // Thread summary surfaces the failed state for sidebar indicators.
  const summary = summarizeThreadOutbox(outbox, "thread-A");
  assert.equal(summary.failedCount, 1);
  assert.equal(summary.sendingCount, 0);
  assert.equal(summary.latestFailed?.msg_id, "opt-text-1");

  // Returning to thread A reloads SQLite messages and merges the failed entry back in.
  const mergedAfterFailure = mergeOutboxIntoMessages("thread-A", sqliteMessagesA, outbox);
  assert.equal(mergedAfterFailure.length, 2);
  assert.equal(mergedAfterFailure[1].msg_id, "opt-text-1");
  assert.equal(mergedAfterFailure[1].send_status, "failed");
  assert.equal(mergedAfterFailure[1].send_error, "Network timeout");
  assert.equal(mergedAfterFailure[1].quote?.msg_id, "srv-1");

  // Discard removes the entry cleanly and clears empty thread keys.
  const discardRes = removeOutboxEntry(outbox, "opt-text-1");
  outbox = discardRes.next;
  assert.equal(discardRes.removed?.msg.msg_id, "opt-text-1");
  assert.equal(outbox["thread-A"], undefined);
  assert.deepEqual(mergeOutboxIntoMessages("thread-A", sqliteMessagesA, outbox), sqliteMessagesA);

  // Late failure after discard/logout must be a safe no-op and never resurrect the message.
  const lateFail = markOutboxFailed(outbox, "thread-A", "opt-text-1", "Late timeout");
  assert.equal(lateFail.updated, null);
  assert.equal(lateFail.next, outbox);
});

test("attachment outbox preserves payload across thread switch and clears cleanly on background success", () => {
  const att = makeRawAtt("invoice.png", "blob:composer-preview");
  const optAtt = makeMsg({
    msg_id: "opt-att-1",
    thread_id: "thread-A",
    text: "Hóa đơn tháng 11",
    msg_type: "chat.photo",
    ts: 1_700_000_010_000,
    attachment_url: "blob:outbox-owned-1",
    attachment_name: "invoice.png",
    send_status: "sending",
  });

  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, {
    msg: optAtt,
    attachment: {
      att,
      caption: "Hóa đơn tháng 11",
      ownedPreviewUrl: "blob:outbox-owned-1",
      rootMsgId: "root-42",
    },
  });

  // Fail while user is in thread B
  outbox = markOutboxFailed(outbox, "thread-A", "opt-att-1", "Upload 503").next;
  const restored = findOutboxEntry(outbox, "opt-att-1", "thread-A");
  assert.ok(restored);
  assert.equal(restored.attachment?.caption, "Hóa đơn tháng 11");
  assert.equal(restored.attachment?.ownedPreviewUrl, "blob:outbox-owned-1");
  assert.equal(restored.attachment?.rootMsgId, "root-42");

  // Succeed in background -> remove from outbox; returning to thread A shows only real server msg
  const removedRes = removeOutboxEntry(outbox, "opt-att-1", "thread-A");
  outbox = removedRes.next;
  assert.equal(removedRes.removed?.attachment?.ownedPreviewUrl, "blob:outbox-owned-1");

  const realServerMsg = makeMsg({
    msg_id: "srv-att-99",
    thread_id: "thread-A",
    text: "Hóa đơn tháng 11",
    msg_type: "chat.photo",
    ts: 1_700_000_011_000,
    attachment_url: "https://cdn.example/invoice.png",
    attachment_name: "invoice.png",
  });
  const merged = mergeOutboxIntoMessages("thread-A", [realServerMsg], outbox);
  assert.equal(merged.length, 1);
  assert.equal(merged[0].msg_id, "srv-att-99");
});

test("mergeOutboxIntoMessages avoids duplicating reconciled sending messages but never drops failed duplicates", () => {
  const baseTs = 1_700_000_050_000;
  const oldSent = makeMsg({
    msg_id: "srv-old",
    thread_id: "t1",
    text: "ok",
    ts: baseTs - 20_000,
  });
  const freshReal = makeMsg({
    msg_id: "srv-fresh",
    thread_id: "t1",
    text: "ok",
    ts: baseTs + 400,
  });

  const sendingSameText = makeMsg({
    msg_id: "opt-sending-1",
    thread_id: "t1",
    text: "ok",
    ts: baseTs,
    send_status: "sending",
  });
  const secondSendingSameText = makeMsg({
    msg_id: "opt-sending-2",
    thread_id: "t1",
    text: "ok",
    ts: baseTs + 100,
    send_status: "sending",
  });
  const failedSameText = makeMsg({
    msg_id: "opt-failed-1",
    thread_id: "t1",
    text: "ok",
    ts: baseTs + 200,
    send_status: "failed",
    send_error: "Offline",
  });

  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, { msg: sendingSameText });
  outbox = upsertOutboxEntry(outbox, { msg: secondSendingSameText });
  outbox = upsertOutboxEntry(outbox, { msg: failedSameText });

  // Only srv-fresh matches sendingSameText (srv-old is too old -> must not match).
  // secondSendingSameText remains visible because srv-fresh was already paired once.
  // failedSameText MUST NEVER be dropped.
  const merged = mergeOutboxIntoMessages("t1", [oldSent, freshReal], outbox);
  const ids = merged.map((m) => m.msg_id);
  assert.deepEqual(ids, ["srv-old", "opt-sending-2", "opt-failed-1", "srv-fresh"]);
});

test("mergeOutboxIntoMessages upgrades stale sending copy in baseMessages when outbox is failed", () => {
  const sendingCopy = makeMsg({
    msg_id: "opt-1",
    thread_id: "t1",
    text: "Ping",
    ts: 1000,
    send_status: "sending",
  });
  let outbox: OutboxByThread = upsertOutboxEntry({}, { msg: sendingCopy });
  outbox = markOutboxFailed(outbox, "t1", "opt-1", "500").next;

  const merged = mergeOutboxIntoMessages("t1", [sendingCopy], outbox);
  assert.equal(merged.length, 1);
  assert.equal(merged[0].send_status, "failed");
  assert.equal(merged[0].send_error, "500");
});

test("removeOutboxEntriesForThread and collectAllOutboxEntries isolate threads cleanly", () => {
  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, {
    msg: makeMsg({ msg_id: "opt-a1", thread_id: "A", ts: 200, send_status: "sending" }),
  });
  outbox = upsertOutboxEntry(outbox, {
    msg: makeMsg({ msg_id: "opt-a0", thread_id: "A", ts: 100, send_status: "failed" }),
  });
  outbox = upsertOutboxEntry(outbox, {
    msg: makeMsg({ msg_id: "opt-b1", thread_id: "B", ts: 150, send_status: "sending" }),
  });

  assert.deepEqual(
    getOutboxMessages(outbox, "A").map((m) => m.msg_id),
    ["opt-a0", "opt-a1"],
  );
  assert.equal(collectAllOutboxEntries(outbox).length, 3);

  const cleared = removeOutboxEntriesForThread(outbox, "A");
  assert.equal(cleared.removed.length, 2);
  assert.equal(cleared.next["A"], undefined);
  assert.equal(cleared.next["B"]?.length, 1);
});

test("appendStagedAttachments appends multi-file drops, deduplicates disk paths, and enforces cap with blob revocation", () => {
  const firstDrop = [
    makePathAtt("C:\\Work\\Report.pdf"),
    makePathAtt("C:\\Work\\Photo.png", "Photo.png", true),
  ];
  const r1 = appendStagedAttachments([], firstDrop);
  assert.equal(r1.next.length, 2);
  assert.equal(r1.addedCount, 2);
  assert.equal(r1.duplicateCount, 0);
  assert.equal(r1.overflowCount, 0);

  // Re-dropping same Windows path with different case/slashes deduplicates while keeping new files
  const secondDrop = [
    makePathAtt("c:/work/report.pdf"),
    makeRawAtt("pasted.png", "blob:paste-1"),
    makePathAtt("/tmp/CaseFile.txt"),
    makePathAtt("/tmp/casefile.txt"),
  ];
  const r2 = appendStagedAttachments(r1.next, secondDrop);
  assert.equal(r2.addedCount, 3);
  assert.equal(r2.duplicateCount, 1);
  assert.equal(r2.next.length, 5);

  // Cap enforcement (default 20, test with custom cap 3 too)
  const capRes = appendStagedAttachments(
    [makeRawAtt("1.png", "blob:1"), makeRawAtt("2.png", "blob:2")],
    [
      makeRawAtt("3.png", "blob:3"),
      makeRawAtt("4.png", "blob:4"),
      makeRawAtt("5.png", "data:image/png;base64,AAAA"),
    ],
    3,
  );
  assert.equal(capRes.next.length, 3);
  assert.equal(capRes.addedCount, 1);
  assert.equal(capRes.overflowCount, 2);
  assert.deepEqual(capRes.rejectedUrls, ["blob:4"]);
  assert.equal(MAX_STAGED_ATTACHMENTS, 20);
});

test("removeStagedAttachmentAt and diffRevocableAttachmentUrls return exact blob URLs to revoke", () => {
  const items: PendingAttachment[] = [
    makeRawAtt("a.png", "blob:a"),
    makePathAtt("C:\\a.pdf"),
    makeRawAtt("b.png", "blob:b"),
  ];

  const removed0 = removeStagedAttachmentAt(items, 0);
  assert.equal(removed0.next.length, 2);
  assert.deepEqual(removed0.revokedUrls, ["blob:a"]);

  const removedPath = removeStagedAttachmentAt(removed0.next, 0);
  assert.equal(removedPath.next.length, 1);
  assert.deepEqual(removedPath.revokedUrls, []);

  assert.deepEqual(collectRevocableAttachmentUrls(items), ["blob:a", "blob:b"]);
  assert.deepEqual(diffRevocableAttachmentUrls(items, [items[2]]), ["blob:a"]);
  assert.deepEqual(diffRevocableAttachmentUrls(items, []), ["blob:a", "blob:b"]);
});

test("buildAttachmentSendBatch attaches caption only to the first file without duplicating across N files", () => {
  const items: PendingAttachment[] = [
    makePathAtt("C:\\Docs\\1.pdf"),
    makePathAtt("C:\\Docs\\2.pdf"),
    makeRawAtt("3.png", "blob:3"),
  ];

  const batch = buildAttachmentSendBatch(items, "Gửi 3 tài liệu họp");
  assert.equal(batch.length, 3);
  assert.equal(batch[0].caption, "Gửi 3 tài liệu họp");
  assert.equal(batch[1].caption, "");
  assert.equal(batch[2].caption, "");
  assert.equal(batch[0].att, items[0]);
  assert.equal(batch[2].att, items[2]);
});

test("mergeOutboxIntoMessages splices out sending bubble when WS server echo arrives before RPC resolves", () => {
  const baseTs = 1_700_000_090_000;
  const optText1 = makeMsg({
    msg_id: "opt-text-ws-1",
    thread_id: "t-ws",
    text: "Đã nhận báo cáo",
    ts: baseTs,
    send_status: "sending",
  });
  const optText2 = makeMsg({
    msg_id: "opt-text-ws-2",
    thread_id: "t-ws",
    text: "Đã nhận báo cáo",
    ts: baseTs + 100,
    send_status: "sending",
  });
  const optFailed = makeMsg({
    msg_id: "opt-text-failed",
    thread_id: "t-ws",
    text: "Đã nhận báo cáo",
    ts: baseTs + 200,
    send_status: "failed",
    send_error: "Timeout",
  });
  const wsEchoText = makeMsg({
    msg_id: "srv-ws-100",
    thread_id: "t-ws",
    text: "Đã nhận báo cáo",
    ts: baseTs + 350,
  });

  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, { msg: optText1 });
  outbox = upsertOutboxEntry(outbox, { msg: optText2 });
  outbox = upsertOutboxEntry(outbox, { msg: optFailed });

  // baseMessages already contains BOTH the optimistic bubbles and the incoming WS echo
  const merged = mergeOutboxIntoMessages("t-ws", [optText1, optText2, optFailed, wsEchoText], outbox);
  assert.deepEqual(
    merged.map((m) => m.msg_id),
    ["opt-text-ws-2", "opt-text-failed", "srv-ws-100"],
  );

  // Attachment sending bubble already present in baseMessages is also spliced out on WS echo
  const optAtt = makeMsg({
    msg_id: "opt-att-ws-1",
    thread_id: "t-ws-att",
    msg_type: "chat.photo",
    text: "Ảnh chụp màn hình",
    attachment_name: "screen.png",
    attachment_url: "blob:local-preview-1",
    ts: baseTs,
    send_status: "sending",
  });
  const wsEchoAtt = makeMsg({
    msg_id: "srv-ws-att-1",
    thread_id: "t-ws-att",
    msg_type: "chat.photo",
    text: "Ảnh chụp màn hình",
    attachment_name: "screen.png",
    attachment_url: "https://cdn.example.invalid/screen.png",
    ts: baseTs + 500,
  });
  const attOutbox = upsertOutboxEntry({}, {
    msg: optAtt,
    attachment: {
      att: makeRawAtt("screen.png", "blob:local-preview-1"),
      caption: "Ảnh chụp màn hình",
      ownedPreviewUrl: "blob:local-preview-1",
    },
  });
  const mergedAtt = mergeOutboxIntoMessages("t-ws-att", [optAtt, wsEchoAtt], attOutbox);
  assert.deepEqual(
    mergedAtt.map((m) => m.msg_id),
    ["srv-ws-att-1"],
  );
});

test("discarding a queued sending attachment removes only that outbox entry so queued upload gate skips it", () => {
  const att1 = makePathAtt("C:\\Docs\\one.pdf");
  const att2 = makePathAtt("C:\\Docs\\two.pdf");
  let outbox: OutboxByThread = {};
  outbox = upsertOutboxEntry(outbox, {
    msg: makeMsg({
      msg_id: "opt-q-1",
      thread_id: "t-q",
      msg_type: "chat.file",
      attachment_name: "one.pdf",
      ts: 1000,
      send_status: "sending",
    }),
    attachment: { att: att1, caption: "", ownedPreviewUrl: null },
  });
  outbox = upsertOutboxEntry(outbox, {
    msg: makeMsg({
      msg_id: "opt-q-2",
      thread_id: "t-q",
      msg_type: "chat.file",
      attachment_name: "two.pdf",
      ts: 1001,
      send_status: "sending",
    }),
    attachment: { att: att2, caption: "", ownedPreviewUrl: null },
  });

  // Discard queued file #2 before its upload task starts
  const discarded = removeOutboxEntry(outbox, "opt-q-2", "t-q");
  outbox = discarded.next;
  assert.equal(discarded.removed?.msg.msg_id, "opt-q-2");
  assert.equal(findOutboxEntry(outbox, "opt-q-2", "t-q"), null);
  assert.equal(findOutboxEntry(outbox, "opt-q-1", "t-q")?.msg.msg_id, "opt-q-1");
});
