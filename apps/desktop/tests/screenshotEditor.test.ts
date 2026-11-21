import assert from "node:assert/strict";
import test from "node:test";
import {
  buildScreenshotFileName,
  clampCropRect,
  computeArrowHead,
  mapAnnotationsAfterCrop,
  normalizeRect,
  resizeCropRect,
  type AnnotationOp,
} from "../src/utils/screenshotEditor.ts";

test("normalizeRect orders reverse drag coordinates and clamps inside image bounds", () => {
  assert.deepEqual(normalizeRect(320, 240, 100, 60, 800, 600), {
    x: 100,
    y: 60,
    w: 220,
    h: 180,
  });
  assert.deepEqual(normalizeRect(-40, 650, 900, -10, 800, 600), {
    x: 0,
    y: 0,
    w: 800,
    h: 600,
  });
});

test("clampCropRect and resizeCropRect enforce bounds across move, corners, and edges", () => {
  assert.deepEqual(clampCropRect({ x: 750, y: 550, w: 120, h: 100 }, 800, 600, 16), {
    x: 680,
    y: 500,
    w: 120,
    h: 100,
  });

  const base = { x: 100, y: 100, w: 200, h: 150 };
  assert.deepEqual(resizeCropRect(base, "move", 700, -150, 800, 600), {
    x: 600,
    y: 0,
    w: 200,
    h: 150,
  });
  assert.deepEqual(resizeCropRect(base, "nw", 250, 250, 800, 600, 20), {
    x: 280,
    y: 230,
    w: 20,
    h: 20,
  });
  assert.deepEqual(resizeCropRect(base, "se", 1000, 1000, 800, 600, 20), {
    x: 100,
    y: 100,
    w: 700,
    h: 500,
  });
  assert.deepEqual(resizeCropRect(base, "e", -190, 50, 800, 600, 20), {
    x: 100,
    y: 100,
    w: 20,
    h: 150,
  });
});

test("computeArrowHead and mapAnnotationsAfterCrop transform shapes cleanly", () => {
  const [tip, wingA, wingB] = computeArrowHead(0, 0, 100, 0, 20);
  assert.deepEqual(tip, { x: 100, y: 0 });
  assert.ok(wingA.x < 100 && wingB.x < 100);
  assert.ok(Math.abs(wingA.y + wingB.y) < 1e-6);

  const ops: AnnotationOp[] = [
    { kind: "pen", color: "#ef4444", size: 4, points: [{ x: 120, y: 80 }, { x: 150, y: 110 }] },
    { kind: "arrow", color: "#facc15", size: 4, x1: 110, y1: 60, x2: 210, y2: 140 },
    { kind: "text", color: "#ffffff", fontSize: 24, x: 130, y: 95, text: "OK" },
  ];
  const shifted = mapAnnotationsAfterCrop(ops, { x: 100, y: 50, w: 300, h: 200 });
  assert.deepEqual(shifted[0], {
    kind: "pen",
    color: "#ef4444",
    size: 4,
    points: [{ x: 20, y: 30 }, { x: 50, y: 60 }],
  });
  assert.deepEqual(shifted[1], {
    kind: "arrow",
    color: "#facc15",
    size: 4,
    x1: 10,
    y1: 10,
    x2: 110,
    y2: 90,
  });
  assert.deepEqual(shifted[2], {
    kind: "text",
    color: "#ffffff",
    fontSize: 24,
    x: 30,
    y: 45,
    text: "OK",
  });
});

test("buildScreenshotFileName formats padded local timestamp", () => {
  const d = new Date(2026, 10, 21, 9, 5, 7);
  assert.equal(buildScreenshotFileName(d), "screenshot_20261121_090507.png");
});
