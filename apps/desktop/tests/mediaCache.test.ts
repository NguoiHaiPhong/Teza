import test from "node:test";
import assert from "node:assert/strict";
import {
  classifyDrawerFileAction,
  classifyNonMediaAttachment,
  extractLinkHost,
  isBlobUrl,
  isDataImageUrl,
  isDataMediaUrl,
  isDataUrl,
  isExplicitFileMessageType,
  isExternalLinkMessageType,
  isRemoteMediaUrl,
  normalizeMediaRequestUrl,
  sanitizeUrlForLog,
} from "../src/utils/mediaCache.ts";

test("normalizeMediaRequestUrl trims, unescapes slashes, expands //, and preserves data/blob", () => {
  assert.equal(normalizeMediaRequestUrl(null), "");
  assert.equal(normalizeMediaRequestUrl("   "), "");
  assert.equal(
    normalizeMediaRequestUrl("  https:\\/\\/f10-zpc.zdn.vn\\/a\\/b.jpg?k=1  "),
    "https://f10-zpc.zdn.vn/a/b.jpg?k=1",
  );
  assert.equal(
    normalizeMediaRequestUrl("\\/\\/s120-ava-talk.zadn.vn\\/avatar.png"),
    "https://s120-ava-talk.zadn.vn/avatar.png",
  );
  assert.equal(
    normalizeMediaRequestUrl("//f10-zpc.zdn.vn/photo.jpg"),
    "https://f10-zpc.zdn.vn/photo.jpg",
  );
  assert.equal(
    normalizeMediaRequestUrl("HTTP://f10-zpc.zdn.vn/legacy.jpg"),
    "http://f10-zpc.zdn.vn/legacy.jpg",
  );
  assert.equal(
    normalizeMediaRequestUrl("HTTPS://f10-zpc.zdn.vn/photo.jpg"),
    "https://f10-zpc.zdn.vn/photo.jpg",
  );

  const dataUrl = "DATA:IMAGE/PNG;base64,a\\/b+c==";
  assert.equal(normalizeMediaRequestUrl(` ${dataUrl} `), dataUrl);

  const blobUrl = "BLOB:http://localhost:1420/550e8400-e29b-41d4-a716-446655440000";
  assert.equal(normalizeMediaRequestUrl(blobUrl), blobUrl);
});

test("URL scheme predicates handle case-insensitive data/blob/http/https/protocol-relative safely", () => {
  assert.equal(isRemoteMediaUrl("https://f10-zpc.zdn.vn/a.jpg"), true);
  assert.equal(isRemoteMediaUrl("http://f10-zpc.zdn.vn/a.jpg"), true);
  assert.equal(isRemoteMediaUrl("//f10-zpc.zdn.vn/a.jpg"), true);
  assert.equal(isRemoteMediaUrl("https:\\/\\/f10-zpc.zdn.vn\\/a.jpg"), true);
  assert.equal(isRemoteMediaUrl("data:image/png;base64,AA=="), false);
  assert.equal(isRemoteMediaUrl("blob:https://app.local/123"), false);
  assert.equal(isRemoteMediaUrl("javascript:alert(1)"), false);
  assert.equal(isRemoteMediaUrl("file:///C:/secret.txt"), false);

  assert.equal(isDataUrl("DATA:application/pdf;base64,AA=="), true);
  assert.equal(isDataMediaUrl("DATA:IMAGE/WEBP;base64,AA=="), true);
  assert.equal(isDataMediaUrl("Data:Audio/aac;base64,AA=="), true);
  assert.equal(isDataMediaUrl("data:video/mp4;base64,AA=="), true);
  assert.equal(isDataMediaUrl("data:text/plain;base64,AA=="), false);
  assert.equal(isDataImageUrl("DATA:image/jpeg;base64,AA=="), true);
  assert.equal(isDataImageUrl("data:audio/mpeg;base64,AA=="), false);
  assert.equal(isBlobUrl("BLOB:tauri://localhost/abc"), true);
  assert.equal(isBlobUrl("https://example.com/blob"), false);
});

test("sanitizeUrlForLog strips paths, queries, fragments, and credentials", () => {
  assert.equal(
    sanitizeUrlForLog("https://user:secret@f10-zpc.zdn.vn/private/doc.pdf?sig=abc123#page=2"),
    "https://f10-zpc.zdn.vn",
  );
  assert.equal(
    sanitizeUrlForLog("http://cdn.example.org:8080/x?token=topsecret"),
    "http://cdn.example.org:8080",
  );
  assert.equal(sanitizeUrlForLog("DATA:image/png;base64,secretpayload"), "data:...");
  assert.equal(sanitizeUrlForLog("blob:http://localhost/uuid-123"), "blob:...");
  assert.equal(sanitizeUrlForLog(""), "(empty)");
  assert.equal(sanitizeUrlForLog("not a url"), "(invalid-url)");

  assert.equal(extractLinkHost("https://docs.example.com/a/b?x=1"), "docs.example.com");
  assert.equal(extractLinkHost("//news.example.vn/post"), "news.example.vn");
  assert.equal(extractLinkHost("data:image/png;base64,AA=="), "");
});

test("classifyNonMediaAttachment routes links/ecards/non-file URLs to open_external and keeps explicit files downloadable", () => {
  assert.equal(isExplicitFileMessageType("chat.file"), true);
  assert.equal(isExplicitFileMessageType("share.file"), true);
  assert.equal(isExplicitFileMessageType("19"), true);
  assert.equal(isExplicitFileMessageType("chat.link"), false);

  assert.equal(isExternalLinkMessageType("chat.link"), true);
  assert.equal(isExternalLinkMessageType("link"), true);
  assert.equal(isExternalLinkMessageType("chat.location"), true);
  assert.equal(isExternalLinkMessageType("chat.location.new"), true);
  assert.equal(isExternalLinkMessageType("chat.recommended"), true);
  assert.equal(isExternalLinkMessageType("chat.ecard"), true);
  assert.equal(isExternalLinkMessageType("chat.file"), false);

  // External link message types must NEVER invoke save_file
  for (const msg_type of [
    "chat.link",
    "link",
    "chat.location",
    "chat.location.new",
    "chat.recommended",
    "chat.ecard",
  ]) {
    assert.equal(
      classifyNonMediaAttachment({ msg_type, attachment_url: "https://example.com/article" }),
      "open_external",
      `${msg_type} should open externally`,
    );
    assert.equal(
      classifyNonMediaAttachment({ msg_type, attachment_url: null }),
      "none",
      `${msg_type} without URL should render nothing`,
    );
  }

  // Legacy non-file message with external href must also open externally, not download
  assert.equal(
    classifyNonMediaAttachment({
      msg_type: "chat.text",
      attachment_url: "https://news.example.org/story",
    }),
    "open_external",
  );

  // Explicit file messages save locally (including http:// and // normalized for Rust upgrade)
  assert.equal(
    classifyNonMediaAttachment({
      msg_type: "chat.file",
      attachment_url: "https://f10-zpc.zdn.vn/spec.pdf",
    }),
    "save_file",
  );
  assert.equal(
    classifyNonMediaAttachment({
      msg_type: "share.file",
      attachment_url: "http://f10-zpc.zdn.vn/legacy.zip",
    }),
    "save_file",
  );
  assert.equal(
    classifyNonMediaAttachment({
      msg_type: "19",
      attachment_url: "//f10-zpc.zdn.vn/archive.7z",
    }),
    "save_file",
  );
  assert.equal(
    classifyNonMediaAttachment({
      msg_type: "chat.file",
      attachment_url: null,
    }),
    "file_no_url",
  );
});

test("classifyDrawerFileAction separates external links from downloadable files and voices", () => {
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.link",
      attachment_url: "https://example.com/page",
    }),
    "open_external",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.location",
      attachment_url: "https://maps.example.com/?q=1,2",
    }),
    "open_external",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.location.new",
      attachment_url: "https://maps.example.com/?q=3,4",
    }),
    "open_external",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.text",
      attachment_url: "http://example.com/legacy-link",
    }),
    "open_external",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.file",
      attachment_url: "https://f10-zpc.zdn.vn/report.docx",
    }),
    "save_file",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.voice",
      attachment_url: "https://f10-zpc.zdn.vn/voice.aac",
    }),
    "save_file",
  );
  assert.equal(
    classifyDrawerFileAction({
      msg_type: "chat.file",
      attachment_url: "",
    }),
    "jump",
  );
});
