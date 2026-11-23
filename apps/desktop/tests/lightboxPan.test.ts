import test from "node:test";
import assert from "node:assert/strict";
import {
  clampLightboxPan,
  clampLightboxZoom,
  LIGHTBOX_MAX_ZOOM,
  LIGHTBOX_MIN_ZOOM,
  zoomAroundPoint,
} from "../src/utils/lightboxPan.ts";

test("clampLightboxZoom enforces min/max bounds and 2-decimal precision", () => {
  assert.equal(clampLightboxZoom(0.1), LIGHTBOX_MIN_ZOOM);
  assert.equal(clampLightboxZoom(10), LIGHTBOX_MAX_ZOOM);
  assert.equal(clampLightboxZoom(1.754), 1.75);
  assert.equal(clampLightboxZoom(Number.NaN), 1);
});

test("clampLightboxPan resets to origin at <=1x and clamps overflow in 0 and 90 deg", () => {
  assert.deepEqual(clampLightboxPan(120, -80, 1, 1000, 800, 800, 600, 0), { x: 0, y: 0 });
  assert.deepEqual(clampLightboxPan(50, -50, 0.75, 1000, 800, 800, 600, 0), { x: 0, y: 0 });

  // At 2x with 800x600 image inside 1000x800 stage:
  // effW = 1600 -> maxX = (1600-1000)/2 + 24 = 324; effH = 1200 -> maxY = (1200-800)/2 + 24 = 224
  assert.deepEqual(clampLightboxPan(500, -400, 2, 1000, 800, 800, 600, 0), { x: 324, y: -224 });
  assert.deepEqual(clampLightboxPan(150, 100, 2, 1000, 800, 800, 600, 0), { x: 150, y: 100 });

  // Rotated 90 deg swaps 800x600 -> 600x800:
  // effW = 1200 -> maxX = (1200-1000)/2 + 24 = 124; effH = 1600 -> maxY = (1600-800)/2 + 24 = 424
  assert.deepEqual(clampLightboxPan(300, -500, 2, 1000, 800, 800, 600, 90), { x: 124, y: -424 });

  // Fallback when image dimensions are not yet measured (0x0)
  assert.deepEqual(clampLightboxPan(800, -800, 2, 1000, 800, 0, 0, 0), { x: 500, y: -400 });
});

test("zoomAroundPoint keeps cursor anchor point stationary across zoom steps", () => {
  const nextPan = zoomAroundPoint({ x: 0, y: 0 }, 1, 2, 120, -60);
  assert.deepEqual(nextPan, { x: -120, y: 60 });

  // Verify cursor point relative to transformed center remains identical:
  // (cursor - pan) / zoom: (120 - (-120)) / 2 == 120 == (120 - 0) / 1
  assert.equal((120 - nextPan.x) / 2, 120);
  assert.equal((-60 - nextPan.y) / 2, -60);

  // Reset to origin when zooming back to <=1x
  assert.deepEqual(zoomAroundPoint(nextPan, 2, 1, 120, -60), { x: 0, y: 0 });
});
