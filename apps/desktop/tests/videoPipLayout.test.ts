import test from "node:test";
import assert from "node:assert/strict";
import {
  applyPipDrag,
  applyPipPreset,
  applyPipResize,
  clampPipRect,
  computeDefaultPipRect,
  detectActivePipPreset,
  getNextPipPreset,
  PIP_DEFAULT_BOTTOM_CLEARANCE,
  PIP_MARGIN,
  PIP_MAX_HEIGHT,
  PIP_MAX_WIDTH,
  PIP_MIN_HEIGHT,
  PIP_MIN_WIDTH,
  PIP_PRESETS,
  type PipRect,
  type ResizeHandle,
} from "../src/utils/videoPipLayout.ts";

function assertWithinViewport(rect: PipRect, vw: number, vh: number, margin = PIP_MARGIN) {
  assert.ok(Number.isFinite(rect.x), "x must be finite");
  assert.ok(Number.isFinite(rect.y), "y must be finite");
  assert.ok(Number.isFinite(rect.width), "width must be finite");
  assert.ok(Number.isFinite(rect.height), "height must be finite");
  assert.ok(rect.x >= margin, `x (${rect.x}) must be >= ${margin}`);
  assert.ok(rect.y >= margin, `y (${rect.y}) must be >= ${margin}`);
  assert.ok(rect.x + rect.width <= vw - margin, `right (${rect.x + rect.width}) must be <= ${vw - margin}`);
  assert.ok(rect.y + rect.height <= vh - margin, `bottom (${rect.y + rect.height}) must be <= ${vh - margin}`);
}

test("PiP layout constants match expected specification", () => {
  assert.equal(PIP_MIN_WIDTH, 260);
  assert.equal(PIP_MIN_HEIGHT, 180);
  assert.equal(PIP_MAX_WIDTH, 860);
  assert.equal(PIP_MAX_HEIGHT, 640);
  assert.equal(PIP_MARGIN, 14);
  assert.deepEqual(PIP_PRESETS, {
    sm: { width: 320, height: 220 },
    md: { width: 440, height: 300 },
    lg: { width: 600, height: 400 },
  });
});

test("computeDefaultPipRect places floating frame at bottom-right above composer", () => {
  // Desktop viewport (>= 1024x680) uses medium preset
  const desktop = computeDefaultPipRect(1280, 800);
  assert.deepEqual(desktop, {
    x: 1280 - PIP_MARGIN - PIP_PRESETS.md.width,
    y: 800 - PIP_DEFAULT_BOTTOM_CLEARANCE - PIP_PRESETS.md.height,
    width: PIP_PRESETS.md.width,
    height: PIP_PRESETS.md.height,
  });
  assertWithinViewport(desktop, 1280, 800);

  // Compact viewport uses small preset
  const compact = computeDefaultPipRect(900, 620);
  assert.equal(compact.width, PIP_PRESETS.sm.width);
  assert.equal(compact.height, PIP_PRESETS.sm.height);
  assert.equal(compact.x, 900 - PIP_MARGIN - PIP_PRESETS.sm.width);
  assert.equal(compact.y, 620 - PIP_DEFAULT_BOTTOM_CLEARANCE - PIP_PRESETS.sm.height);
  assertWithinViewport(compact, 900, 620);

  // Tiny viewport clamps safely without overflow
  const tiny = computeDefaultPipRect(240, 160);
  assertWithinViewport(tiny, 240, 160);
  assert.equal(tiny.width, 240 - PIP_MARGIN * 2);
  assert.equal(tiny.height, 160 - PIP_MARGIN * 2);

  // Invalid / non-finite viewport falls back safely
  const fallback = computeDefaultPipRect(Number.NaN, 0);
  assertWithinViewport(fallback, 1280, 800);
});

test("clampPipRect enforces min/max size, viewport bounds, and margin limits", () => {
  // Below minimum size & negative coords
  const tooSmall = clampPipRect({ x: -120, y: -50, width: 100, height: 80 }, 1280, 800);
  assert.deepEqual(tooSmall, {
    x: PIP_MARGIN,
    y: PIP_MARGIN,
    width: PIP_MIN_WIDTH,
    height: PIP_MIN_HEIGHT,
  });

  // Above maximum size & far offscreen right/bottom
  const tooLarge = clampPipRect({ x: 5000, y: 4000, width: 2000, height: 1500 }, 1440, 900);
  assert.deepEqual(tooLarge, {
    x: 1440 - PIP_MARGIN - PIP_MAX_WIDTH,
    y: 900 - PIP_MARGIN - PIP_MAX_HEIGHT,
    width: PIP_MAX_WIDTH,
    height: PIP_MAX_HEIGHT,
  });

  // Medium viewport smaller than PIP_MAX_WIDTH/HEIGHT clamps to viewport minus margins
  const viewportBound = clampPipRect({ x: 200, y: 200, width: 860, height: 640 }, 700, 500);
  assert.deepEqual(viewportBound, {
    x: PIP_MARGIN,
    y: PIP_MARGIN,
    width: 700 - PIP_MARGIN * 2,
    height: 500 - PIP_MARGIN * 2,
  });

  // Non-finite fields recover cleanly
  const nanRect = clampPipRect(
    { x: Number.NaN, y: Number.POSITIVE_INFINITY, width: Number.NaN, height: Number.NEGATIVE_INFINITY },
    1280,
    800,
  );
  assertWithinViewport(nanRect, 1280, 800);

  // Ultra-tiny viewport (< 2 * PIP_MARGIN) never produces negative dimensions or NaN
  const ultraTiny = clampPipRect({ x: 5, y: 5, width: 300, height: 200 }, 20, 20);
  assertWithinViewport(ultraTiny, 20, 20, 0);
  assert.deepEqual(ultraTiny, { x: 0, y: 0, width: 20, height: 20 });
});

test("applyPipDrag moves rect accurately and clamps to viewport margins", () => {
  const start: PipRect = { x: 500, y: 300, width: 400, height: 280 };

  assert.deepEqual(applyPipDrag(start, 60, -40, 1280, 800), {
    x: 560,
    y: 260,
    width: 400,
    height: 280,
  });

  // Push way past top-left
  assert.deepEqual(applyPipDrag(start, -2000, -2000, 1280, 800), {
    x: PIP_MARGIN,
    y: PIP_MARGIN,
    width: 400,
    height: 280,
  });

  // Push way past bottom-right
  assert.deepEqual(applyPipDrag(start, 2000, 2000, 1280, 800), {
    x: 1280 - PIP_MARGIN - 400,
    y: 800 - PIP_MARGIN - 280,
    width: 400,
    height: 280,
  });

  // Non-finite deltas leave position unchanged
  assert.deepEqual(applyPipDrag(start, Number.NaN, Number.POSITIVE_INFINITY, 1280, 800), start);
});

test("applyPipResize anchors opposite edge across all 8 handles including min/max and viewport clamps", () => {
  const vw = 1280;
  const vh = 800;
  const start: PipRect = { x: 400, y: 250, width: 400, height: 260 };
  const startRight = start.x + start.width; // 800
  const startBottom = start.y + start.height; // 510

  // 1. East ("e"): left edge stays fixed
  assert.deepEqual(applyPipResize(start, "e", 100, 50, vw, vh), {
    x: 400,
    y: 250,
    width: 500,
    height: 260,
  });
  // Clamped at min width
  const eMin = applyPipResize(start, "e", -500, 0, vw, vh);
  assert.equal(eMin.x, start.x);
  assert.equal(eMin.width, PIP_MIN_WIDTH);
  // Clamped at max width / right margin (1280 - 14 - 400 = 866 > 860 -> capped by PIP_MAX_WIDTH)
  const eMax = applyPipResize(start, "e", 900, 0, vw, vh);
  assert.equal(eMax.x, start.x);
  assert.equal(eMax.width, PIP_MAX_WIDTH);
  // Clamped by right viewport margin when near right edge
  const nearRight: PipRect = { x: 900, y: 250, width: 300, height: 260 };
  const eViewport = applyPipResize(nearRight, "e", 500, 0, vw, vh);
  assert.equal(eViewport.x, nearRight.x);
  assert.equal(eViewport.x + eViewport.width, vw - PIP_MARGIN);

  // 2. South ("s"): top edge stays fixed
  assert.deepEqual(applyPipResize(start, "s", 80, 90, vw, vh), {
    x: 400,
    y: 250,
    width: 400,
    height: 350,
  });
  const sMin = applyPipResize(start, "s", 0, -500, vw, vh);
  assert.equal(sMin.y, start.y);
  assert.equal(sMin.height, PIP_MIN_HEIGHT);
  // Clamped by bottom viewport margin (800 - 14 - 250 = 536 < 640)
  const sMax = applyPipResize(start, "s", 0, 900, vw, vh);
  assert.equal(sMax.y, start.y);
  assert.equal(sMax.y + sMax.height, vh - PIP_MARGIN);

  // 3. West ("w"): RIGHT edge stays strictly anchored!
  const wGrow = applyPipResize(start, "w", -100, 50, vw, vh);
  assert.deepEqual(wGrow, { x: 300, y: 250, width: 500, height: 260 });
  assert.equal(wGrow.x + wGrow.width, startRight);

  const wMin = applyPipResize(start, "w", 500, 0, vw, vh);
  assert.equal(wMin.width, PIP_MIN_WIDTH);
  assert.equal(wMin.x, startRight - PIP_MIN_WIDTH);
  assert.equal(wMin.x + wMin.width, startRight);

  // West hits left viewport margin (startRight = 800, max avail left = 800 - 14 = 786 < 860)
  const wViewport = applyPipResize(start, "w", -900, 0, vw, vh);
  assert.equal(wViewport.x, PIP_MARGIN);
  assert.equal(wViewport.x + wViewport.width, startRight);

  // West hits PIP_MAX_WIDTH when right edge is far enough right
  const farRightStart: PipRect = { x: 860, y: 250, width: 400, height: 260 }; // right = 1260
  const wMax = applyPipResize(farRightStart, "w", -1200, 0, vw, vh);
  assert.equal(wMax.width, PIP_MAX_WIDTH);
  assert.equal(wMax.x + wMax.width, farRightStart.x + farRightStart.width);

  // 4. North ("n"): BOTTOM edge stays strictly anchored!
  const nGrow = applyPipResize(start, "n", 75, -80, vw, vh);
  assert.deepEqual(nGrow, { x: 400, y: 170, width: 400, height: 340 });
  assert.equal(nGrow.y + nGrow.height, startBottom);

  const nMin = applyPipResize(start, "n", 0, 500, vw, vh);
  assert.equal(nMin.height, PIP_MIN_HEIGHT);
  assert.equal(nMin.y, startBottom - PIP_MIN_HEIGHT);
  assert.equal(nMin.y + nMin.height, startBottom);

  // North hits top viewport margin (startBottom = 510 -> max avail = 496 < 640)
  const nViewport = applyPipResize(start, "n", 0, -900, vw, vh);
  assert.equal(nViewport.y, PIP_MARGIN);
  assert.equal(nViewport.y + nViewport.height, startBottom);

  // North hits PIP_MAX_HEIGHT when bottom edge is low enough
  const lowStart: PipRect = { x: 400, y: 480, width: 400, height: 300 }; // bottom = 780
  const nMax = applyPipResize(lowStart, "n", 0, -900, vw, vh);
  assert.equal(nMax.height, PIP_MAX_HEIGHT);
  assert.equal(nMax.y + nMax.height, lowStart.y + lowStart.height);

  // 5. North-West ("nw"): BOTH right & bottom stay anchored across min & max clamps
  const nwShrink = applyPipResize(start, "nw", 999, 999, vw, vh);
  assert.equal(nwShrink.width, PIP_MIN_WIDTH);
  assert.equal(nwShrink.height, PIP_MIN_HEIGHT);
  assert.equal(nwShrink.x + nwShrink.width, startRight);
  assert.equal(nwShrink.y + nwShrink.height, startBottom);

  const nwGrow = applyPipResize(start, "nw", -999, -999, vw, vh);
  assert.equal(nwGrow.x, PIP_MARGIN);
  assert.equal(nwGrow.y, PIP_MARGIN);
  assert.equal(nwGrow.x + nwGrow.width, startRight);
  assert.equal(nwGrow.y + nwGrow.height, startBottom);

  // 6. North-East ("ne"): left & bottom stay anchored
  const neShrink = applyPipResize(start, "ne", -999, 999, vw, vh);
  assert.equal(neShrink.x, start.x);
  assert.equal(neShrink.width, PIP_MIN_WIDTH);
  assert.equal(neShrink.height, PIP_MIN_HEIGHT);
  assert.equal(neShrink.y + neShrink.height, startBottom);

  const neGrow = applyPipResize(start, "ne", 999, -999, vw, vh);
  assert.equal(neGrow.x, start.x);
  assert.equal(neGrow.width, PIP_MAX_WIDTH);
  assert.equal(neGrow.y, PIP_MARGIN);
  assert.equal(neGrow.y + neGrow.height, startBottom);

  // 7. South-West ("sw"): right & top stay anchored
  const swShrink = applyPipResize(start, "sw", 999, -999, vw, vh);
  assert.equal(swShrink.y, start.y);
  assert.equal(swShrink.width, PIP_MIN_WIDTH);
  assert.equal(swShrink.height, PIP_MIN_HEIGHT);
  assert.equal(swShrink.x + swShrink.width, startRight);

  const swGrow = applyPipResize(start, "sw", -999, 999, vw, vh);
  assert.equal(swGrow.y, start.y);
  assert.equal(swGrow.x, PIP_MARGIN);
  assert.equal(swGrow.x + swGrow.width, startRight);
  assert.equal(swGrow.y + swGrow.height, vh - PIP_MARGIN);

  // 8. South-East ("se"): left & top stay anchored
  const seShrink = applyPipResize(start, "se", -999, -999, vw, vh);
  assert.equal(seShrink.x, start.x);
  assert.equal(seShrink.y, start.y);
  assert.equal(seShrink.width, PIP_MIN_WIDTH);
  assert.equal(seShrink.height, PIP_MIN_HEIGHT);

  const seGrow = applyPipResize(start, "se", 999, 999, vw, vh);
  assert.equal(seGrow.x, start.x);
  assert.equal(seGrow.y, start.y);
  assert.equal(seGrow.width, PIP_MAX_WIDTH);
  assert.equal(seGrow.y + seGrow.height, vh - PIP_MARGIN);

  // Sanity: every handle stays inside viewport under extreme deltas
  const handles: ResizeHandle[] = ["n", "s", "e", "w", "ne", "nw", "se", "sw"];
  for (const handle of handles) {
    assertWithinViewport(applyPipResize(start, handle, 5000, 5000, vw, vh), vw, vh);
    assertWithinViewport(applyPipResize(start, handle, -5000, -5000, vw, vh), vw, vh);
    assert.deepEqual(
      applyPipResize(start, handle, Number.NaN, Number.NaN, vw, vh),
      start,
    );
  }
});

test("applyPipPreset scales cleanly while anchoring to nearest screen corner", () => {
  const vw = 1280;
  const vh = 800;

  // Bottom-right anchored: right and bottom edges stay pinned as presets change
  const bottomRight = computeDefaultPipRect(vw, vh);
  const brRight = bottomRight.x + bottomRight.width;
  const brBottom = bottomRight.y + bottomRight.height;

  const lg = applyPipPreset(bottomRight, "lg", vw, vh);
  assert.equal(lg.width, PIP_PRESETS.lg.width);
  assert.equal(lg.height, PIP_PRESETS.lg.height);
  assert.equal(lg.x + lg.width, brRight);
  assert.equal(lg.y + lg.height, brBottom);
  assertWithinViewport(lg, vw, vh);

  const sm = applyPipPreset(lg, "sm", vw, vh);
  assert.equal(sm.width, PIP_PRESETS.sm.width);
  assert.equal(sm.height, PIP_PRESETS.sm.height);
  assert.equal(sm.x + sm.width, brRight);
  assert.equal(sm.y + sm.height, brBottom);

  // Top-left anchored: top and left edges stay pinned
  const topLeft: PipRect = { x: 24, y: 24, width: 320, height: 220 };
  const tlLg = applyPipPreset(topLeft, "lg", vw, vh);
  assert.deepEqual(tlLg, {
    x: 24,
    y: 24,
    width: PIP_PRESETS.lg.width,
    height: PIP_PRESETS.lg.height,
  });

  // Small viewport clamps oversized lg preset gracefully
  const smallVp = applyPipPreset({ x: 20, y: 20, width: 320, height: 220 }, "lg", 500, 360);
  assertWithinViewport(smallVp, 500, 360);
  assert.equal(smallVp.width, 500 - PIP_MARGIN * 2);
  assert.equal(smallVp.height, 360 - PIP_MARGIN * 2);
});

test("detectActivePipPreset and getNextPipPreset identify and cycle presets reliably", () => {
  assert.equal(detectActivePipPreset({ x: 0, y: 0, width: 320, height: 220 }), "sm");
  assert.equal(detectActivePipPreset({ x: 0, y: 0, width: 443, height: 298 }), "md");
  assert.equal(detectActivePipPreset({ x: 0, y: 0, width: 600, height: 400 }), "lg");
  assert.equal(detectActivePipPreset({ x: 0, y: 0, width: 512, height: 340 }), null);

  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 320, height: 220 }), "md");
  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 440, height: 300 }), "lg");
  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 600, height: 400 }), "sm");
  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 280, height: 190 }), "md");
  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 490, height: 320 }), "lg");
  assert.equal(getNextPipPreset({ x: 0, y: 0, width: 760, height: 500 }), "sm");
});
