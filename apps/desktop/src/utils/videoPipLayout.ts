/**
 * Pure geometry helpers for the resizable & draggable floating video call PiP
 * frame inside Teza. Zero DOM dependencies so every calculation can be unit
 * tested directly under Node.
 */

export const PIP_MIN_WIDTH = 260;
export const PIP_MIN_HEIGHT = 180;
export const PIP_MAX_WIDTH = 860;
export const PIP_MAX_HEIGHT = 640;
export const PIP_MARGIN = 14;

/** Vertical clearance reserved at the bottom so default PiP sits above the chat composer. */
export const PIP_DEFAULT_BOTTOM_CLEARANCE = 84;

export type PipPresetKey = "sm" | "md" | "lg";

export const PIP_PRESETS: Readonly<Record<PipPresetKey, Readonly<{ width: number; height: number }>>> = {
  sm: { width: 320, height: 220 },
  md: { width: 440, height: 300 },
  lg: { width: 600, height: 400 },
};

export interface PipRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type ResizeHandle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

interface ResolvedViewportBounds {
  vw: number;
  vh: number;
  marginX: number;
  marginY: number;
  minW: number;
  maxW: number;
  minH: number;
  maxH: number;
}

const FALLBACK_VIEWPORT_W = 1280;
const FALLBACK_VIEWPORT_H = 800;

function clamp(value: number, min: number, max: number): number {
  if (value < min) return min;
  if (value > max) return max;
  return value;
}

function resolveViewportBounds(viewportW: number, viewportH: number): ResolvedViewportBounds {
  const vw = Number.isFinite(viewportW) && viewportW > 0 ? viewportW : FALLBACK_VIEWPORT_W;
  const vh = Number.isFinite(viewportH) && viewportH > 0 ? viewportH : FALLBACK_VIEWPORT_H;

  const marginX = vw > PIP_MARGIN * 2 ? PIP_MARGIN : 0;
  const marginY = vh > PIP_MARGIN * 2 ? PIP_MARGIN : 0;

  const availW = Math.max(1, Math.floor(vw - marginX * 2));
  const availH = Math.max(1, Math.floor(vh - marginY * 2));

  const maxW = Math.max(1, Math.min(PIP_MAX_WIDTH, availW));
  const maxH = Math.max(1, Math.min(PIP_MAX_HEIGHT, availH));
  const minW = Math.min(PIP_MIN_WIDTH, maxW);
  const minH = Math.min(PIP_MIN_HEIGHT, maxH);

  return { vw, vh, marginX, marginY, minW, maxW, minH, maxH };
}

/**
 * Clamp a PiP rectangle so its size stays within `[min, max]` and viewport
 * bounds, and its position never leaves the visible viewport margins.
 */
export function clampPipRect(rect: PipRect, viewportW: number, viewportH: number): PipRect {
  const { vw, vh, marginX, marginY, minW, maxW, minH, maxH } = resolveViewportBounds(
    viewportW,
    viewportH,
  );

  const rawW = Number.isFinite(rect.width) ? rect.width : PIP_PRESETS.md.width;
  const rawH = Number.isFinite(rect.height) ? rect.height : PIP_PRESETS.md.height;

  const width = Math.round(clamp(rawW, minW, maxW));
  const height = Math.round(clamp(rawH, minH, maxH));

  const maxX = Math.max(marginX, Math.floor(vw - marginX - width));
  const maxY = Math.max(marginY, Math.floor(vh - marginY - height));

  const rawX = Number.isFinite(rect.x) ? rect.x : maxX;
  const rawY = Number.isFinite(rect.y) ? rect.y : maxY;

  const x = Math.round(clamp(rawX, marginX, maxX));
  const y = Math.round(clamp(rawY, marginY, maxY));

  return { x, y, width, height };
}

/**
 * Compute the default floating PiP placement: bottom-right with margin,
 * comfortably above the chat composer bar.
 */
export function computeDefaultPipRect(viewportW: number, viewportH: number): PipRect {
  const { vw, vh, marginX, marginY } = resolveViewportBounds(viewportW, viewportH);
  const preset = vw >= 1024 && vh >= 680 ? PIP_PRESETS.md : PIP_PRESETS.sm;
  const bottomClearance = Math.max(marginY, PIP_DEFAULT_BOTTOM_CLEARANCE);

  return clampPipRect(
    {
      x: vw - marginX - preset.width,
      y: vh - bottomClearance - preset.height,
      width: preset.width,
      height: preset.height,
    },
    vw,
    vh,
  );
}

/**
 * Apply a pointer drag delta `(dx, dy)` to `start` while keeping the entire
 * PiP frame inside the viewport margins.
 */
export function applyPipDrag(
  start: PipRect,
  dx: number,
  dy: number,
  viewportW: number,
  viewportH: number,
): PipRect {
  const safeDx = Number.isFinite(dx) ? dx : 0;
  const safeDy = Number.isFinite(dy) ? dy : 0;

  return clampPipRect(
    {
      x: start.x + safeDx,
      y: start.y + safeDy,
      width: start.width,
      height: start.height,
    },
    viewportW,
    viewportH,
  );
}

/**
 * Apply an interactive resize delta from any of the 8 edge/corner handles.
 * Opposite edges stay strictly anchored when hitting min/max or viewport
 * bounds so the floating window never jumps unexpectedly.
 */
export function applyPipResize(
  start: PipRect,
  handle: ResizeHandle,
  dx: number,
  dy: number,
  viewportW: number,
  viewportH: number,
): PipRect {
  const bounds = resolveViewportBounds(viewportW, viewportH);
  const base = clampPipRect(start, bounds.vw, bounds.vh);

  const safeDx = Number.isFinite(dx) ? dx : 0;
  const safeDy = Number.isFinite(dy) ? dy : 0;

  const hasW = handle.includes("w");
  const hasE = handle.includes("e");
  const hasN = handle.includes("n");
  const hasS = handle.includes("s");

  let x = base.x;
  let width = base.width;
  let y = base.y;
  let height = base.height;

  if (hasE) {
    const maxAllowedW = Math.min(bounds.maxW, Math.max(1, Math.floor(bounds.vw - bounds.marginX - base.x)));
    const minAllowedW = Math.min(bounds.minW, maxAllowedW);
    width = Math.round(clamp(base.width + safeDx, minAllowedW, maxAllowedW));
    x = base.x;
  } else if (hasW) {
    const anchorRight = base.x + base.width;
    const maxAllowedW = Math.min(bounds.maxW, Math.max(1, Math.floor(anchorRight - bounds.marginX)));
    const minAllowedW = Math.min(bounds.minW, maxAllowedW);
    width = Math.round(clamp(base.width - safeDx, minAllowedW, maxAllowedW));
    x = anchorRight - width;
  }

  if (hasS) {
    const maxAllowedH = Math.min(bounds.maxH, Math.max(1, Math.floor(bounds.vh - bounds.marginY - base.y)));
    const minAllowedH = Math.min(bounds.minH, maxAllowedH);
    height = Math.round(clamp(base.height + safeDy, minAllowedH, maxAllowedH));
    y = base.y;
  } else if (hasN) {
    const anchorBottom = base.y + base.height;
    const maxAllowedH = Math.min(bounds.maxH, Math.max(1, Math.floor(anchorBottom - bounds.marginY)));
    const minAllowedH = Math.min(bounds.minH, maxAllowedH);
    height = Math.round(clamp(base.height - safeDy, minAllowedH, maxAllowedH));
    y = anchorBottom - height;
  }

  return { x, y, width, height };
}

/**
 * Resize the PiP frame to a quick size preset (`sm` | `md` | `lg`), anchoring
 * toward whichever horizontal/vertical edge the frame currently sits closest to
 * so resizing grows inward smoothly without drifting offscreen.
 */
export function applyPipPreset(
  current: PipRect,
  preset: PipPresetKey,
  viewportW: number,
  viewportH: number,
): PipRect {
  const bounds = resolveViewportBounds(viewportW, viewportH);
  const base = clampPipRect(current, bounds.vw, bounds.vh);
  const target = PIP_PRESETS[preset];

  const targetW = Math.round(clamp(target.width, bounds.minW, bounds.maxW));
  const targetH = Math.round(clamp(target.height, bounds.minH, bounds.maxH));

  const centerX = base.x + base.width / 2;
  const centerY = base.y + base.height / 2;

  const anchorRight = centerX >= bounds.vw / 2;
  const anchorBottom = centerY >= bounds.vh / 2;

  const nextX = anchorRight ? base.x + base.width - targetW : base.x;
  const nextY = anchorBottom ? base.y + base.height - targetH : base.y;

  return clampPipRect(
    {
      x: nextX,
      y: nextY,
      width: targetW,
      height: targetH,
    },
    bounds.vw,
    bounds.vh,
  );
}

/**
 * Identify whether `rect` currently matches one of the standard `S`/`M`/`L` presets
 * (within a small tolerance) for highlighting the active preset button.
 */
export function detectActivePipPreset(rect: PipRect, tolerance = 6): PipPresetKey | null {
  const keys: readonly PipPresetKey[] = ["sm", "md", "lg"];
  for (const key of keys) {
    const preset = PIP_PRESETS[key];
    if (
      Math.abs(rect.width - preset.width) <= tolerance &&
      Math.abs(rect.height - preset.height) <= tolerance
    ) {
      return key;
    }
  }
  return null;
}

/**
 * Compute the next preset in the `sm -> md -> lg -> sm` cycle based on current size.
 */
export function getNextPipPreset(rect: PipRect): PipPresetKey {
  const active = detectActivePipPreset(rect);
  if (active === "sm") return "md";
  if (active === "md") return "lg";
  if (active === "lg") return "sm";
  if (rect.width < (PIP_PRESETS.sm.width + PIP_PRESETS.md.width) / 2) return "md";
  if (rect.width < (PIP_PRESETS.md.width + PIP_PRESETS.lg.width) / 2) return "lg";
  return "sm";
}
