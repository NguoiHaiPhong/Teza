import test from "node:test";
import assert from "node:assert/strict";
import {
  extFromMime,
  extractAttachmentFromClipboard,
  type ClipboardDataLike,
  type ClipboardFileEntryLike,
} from "../src/utils/clipboardParse.ts";

function mockClipboard(opts: {
  items?: Array<{ kind: string; file: ClipboardFileEntryLike | null }>;
  files?: ClipboardFileEntryLike[];
  data?: Record<string, string>;
}): ClipboardDataLike<ClipboardFileEntryLike> {
  return {
    items: (opts.items ?? []).map((it) => ({
      kind: it.kind,
      getAsFile: () => it.file,
    })),
    files: opts.files ?? [],
    getData: (fmt: string) => opts.data?.[fmt] ?? "",
  };
}

test("extFromMime maps common clipboard image MIME types accurately", () => {
  assert.equal(extFromMime("image/png"), "png");
  assert.equal(extFromMime("image/jpeg"), "jpg");
  assert.equal(extFromMime("IMAGE/JPG"), "jpg");
  assert.equal(extFromMime("image/webp"), "webp");
  assert.equal(extFromMime("image/gif"), "gif");
  assert.equal(extFromMime("image/bmp"), "bmp");
  assert.equal(extFromMime("application/octet-stream"), "png");
});

test("extractAttachmentFromClipboard prefers text when Excel/Word also puts snapshot image", () => {
  const excelPaste = mockClipboard({
    items: [
      { kind: "string", file: null },
      { kind: "file", file: { name: "image.png", type: "image/png" } },
    ],
    data: { "text/plain": "Cell A1\tCell B1" },
  });
  assert.equal(extractAttachmentFromClipboard(excelPaste), null);

  const emptyNameSnapshot = mockClipboard({
    items: [{ kind: "file", file: { name: "", type: "image/png" } }],
    data: { "text/plain": "Copied paragraph" },
  });
  assert.equal(extractAttachmentFromClipboard(emptyNameSnapshot), null);
});

test("extractAttachmentFromClipboard attaches pure screenshots, named photos, files fallback, and explorer docs", () => {
  const screenshot = mockClipboard({
    items: [{ kind: "file", file: { name: "image.png", type: "image/png" } }],
    data: { "text/plain": "   " },
  });
  assert.deepEqual(extractAttachmentFromClipboard(screenshot), {
    name: "image.png",
    type: "image/png",
  });

  const filesFallback = mockClipboard({
    items: [],
    files: [{ name: "scan.webp", type: "image/webp" }],
  });
  assert.deepEqual(extractAttachmentFromClipboard(filesFallback), {
    name: "scan.webp",
    type: "image/webp",
  });

  const nonImageDoc = mockClipboard({
    items: [{ kind: "file", file: { name: "report.pdf", type: "application/pdf" } }],
    data: { "text/plain": "report.pdf" },
  });
  assert.deepEqual(extractAttachmentFromClipboard(nonImageDoc), {
    name: "report.pdf",
    type: "application/pdf",
  });

  const uriImage = mockClipboard({
    items: [{ kind: "file", file: { name: "image.png", type: "image/png" } }],
    data: {
      "text/plain": "https://cdn.example/photo.png",
      "text/uri-list": "https://cdn.example/photo.png",
    },
  });
  assert.deepEqual(extractAttachmentFromClipboard(uriImage), {
    name: "image.png",
    type: "image/png",
  });

  assert.equal(extractAttachmentFromClipboard(null), null);
});
