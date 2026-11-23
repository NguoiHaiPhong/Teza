import test from "node:test";
import assert from "node:assert/strict";
import {
  applyDownloadProgress,
  computeDownloadPercent,
  formatDownloadBytes,
  shouldFocusComposerOnReply,
  type DownloadProgressItem,
} from "../src/utils/downloadState.ts";
import type { DownloadProgressDto } from "../src/types.ts";

function makeEv(overrides: Partial<DownloadProgressDto>): DownloadProgressDto {
  return {
    id: "dl-1",
    file_name: "Check NAD Change PN.xlsx",
    stage: "downloading",
    downloaded_bytes: 0,
    total_bytes: 1024,
    saved_path: null,
    can_open_directly: false,
    error: null,
    ...overrides,
  };
}

test("formatDownloadBytes formats 0, B, KB, MB, and GB accurately", () => {
  assert.equal(formatDownloadBytes(0), "0 B");
  assert.equal(formatDownloadBytes(-12), "0 B");
  assert.equal(formatDownloadBytes(Number.NaN), "0 B");
  assert.equal(formatDownloadBytes(512), "512 B");
  assert.equal(formatDownloadBytes(1536), "1.5 KB");
  assert.equal(formatDownloadBytes(2.5 * 1024 * 1024), "2.5 MB");
  assert.equal(formatDownloadBytes(1.25 * 1024 * 1024 * 1024), "1.25 GB");
});

test("computeDownloadPercent handles unknown totals and clamps to 0..100", () => {
  assert.equal(computeDownloadPercent(500, null), null);
  assert.equal(computeDownloadPercent(500, 0), null);
  assert.equal(computeDownloadPercent(500, -100), null);
  assert.equal(computeDownloadPercent(0, 1000), 0);
  assert.equal(computeDownloadPercent(-50, 1000), 0);
  assert.equal(computeDownloadPercent(256, 1024), 25);
  assert.equal(computeDownloadPercent(2048, 1024), 100);
});

test("applyDownloadProgress upserts downloading/saving and removes terminal stages", () => {
  let list: DownloadProgressItem[] = [];

  list = applyDownloadProgress(list, makeEv({ id: "dl-1", downloaded_bytes: 0, total_bytes: 1000 }));
  assert.equal(list.length, 1);
  assert.equal(list[0].stage, "downloading");
  assert.equal(list[0].downloadedBytes, 0);

  list = applyDownloadProgress(
    list,
    makeEv({ id: "dl-1", downloaded_bytes: 600, total_bytes: 1000 }),
  );
  assert.equal(list.length, 1);
  assert.equal(list[0].downloadedBytes, 600);

  list = applyDownloadProgress(
    list,
    makeEv({ id: "dl-2", file_name: "spec.pdf", downloaded_bytes: 120, total_bytes: null }),
  );
  assert.equal(list.length, 2);

  list = applyDownloadProgress(
    list,
    makeEv({ id: "dl-1", stage: "saving", downloaded_bytes: 1000, total_bytes: 1000 }),
  );
  assert.equal(list.find((i) => i.id === "dl-1")?.stage, "saving");

  list = applyDownloadProgress(
    list,
    makeEv({ id: "dl-1", stage: "done", saved_path: "C:\\Downloads\\Check.xlsx", can_open_directly: true }),
  );
  assert.deepEqual(
    list.map((i) => i.id),
    ["dl-2"],
  );

  list = applyDownloadProgress(list, makeEv({ id: "dl-2", stage: "cancelled" }));
  assert.equal(list.length, 0);

  list = applyDownloadProgress(list, makeEv({ id: "dl-3", downloaded_bytes: 10, total_bytes: 100 }));
  list = applyDownloadProgress(list, makeEv({ id: "dl-3", stage: "error", error: "network" }));
  assert.equal(list.length, 0);
});

test("shouldFocusComposerOnReply focuses only when selecting reply or re-triggering signal", () => {
  // Initial idle state
  assert.equal(shouldFocusComposerOnReply(null, null, 0, 0), false);
  // New reply selected
  assert.equal(shouldFocusComposerOnReply(null, "m1", 0, 1), true);
  assert.equal(shouldFocusComposerOnReply(null, "m1", 0, 0), true);
  // Switching to a different reply message
  assert.equal(shouldFocusComposerOnReply("m1", "m2", 1, 1), true);
  // Re-clicking Reply on the same message bumps focusSignal
  assert.equal(shouldFocusComposerOnReply("m1", "m1", 1, 2), true);
  // Unchanged re-render must not steal focus
  assert.equal(shouldFocusComposerOnReply("m1", "m1", 2, 2), false);
  // Clearing reply or switching threads (signal unchanged) must not steal focus
  assert.equal(shouldFocusComposerOnReply("m1", null, 2, 2), false);
});
