export const LIGHTBOX_MIN_ZOOM = 0.5;
export const LIGHTBOX_MAX_ZOOM = 4;

export function clampLightboxZoom(z: number): number {
  if (!Number.isFinite(z)) return 1;
  return Math.max(LIGHTBOX_MIN_ZOOM, Math.min(LIGHTBOX_MAX_ZOOM, Math.round(z * 100) / 100));
}

export function clampLightboxPan(
  panX: number,
  panY: number,
  zoom: number,
  stageW: number,
  stageH: number,
  imgW: number,
  imgH: number,
  rotationDeg = 0,
): { x: number; y: number } {
  if (!Number.isFinite(zoom) || zoom <= 1) {
    return { x: 0, y: 0 };
  }

  const rotatedQuarterTurn = Math.abs(rotationDeg) % 180 === 90;
  const baseW = rotatedQuarterTurn ? imgH : imgW;
  const baseH = rotatedQuarterTurn ? imgW : imgH;

  let maxX: number;
  let maxY: number;

  if (baseW > 0 && baseH > 0) {
    const effW = baseW * zoom;
    const effH = baseH * zoom;
    maxX = effW > stageW ? Math.ceil((effW - stageW) / 2) + 24 : 0;
    maxY = effH > stageH ? Math.ceil((effH - stageH) / 2) + 24 : 0;
  } else {
    maxX = Math.max(0, Math.round(((zoom - 1) * stageW) / 2));
    maxY = Math.max(0, Math.round(((zoom - 1) * stageH) / 2));
  }

  const safeX = Number.isFinite(panX) ? panX : 0;
  const safeY = Number.isFinite(panY) ? panY : 0;

  return {
    x: Math.max(-maxX, Math.min(maxX, Math.round(safeX))),
    y: Math.max(-maxY, Math.min(maxY, Math.round(safeY))),
  };
}

export function zoomAroundPoint(
  pan: { x: number; y: number },
  oldZoom: number,
  newZoom: number,
  cursorOffsetX: number,
  cursorOffsetY: number,
): { x: number; y: number } {
  if (!Number.isFinite(newZoom) || newZoom <= 1 || !Number.isFinite(oldZoom) || oldZoom <= 0) {
    return { x: 0, y: 0 };
  }
  const ratio = newZoom / oldZoom;
  return {
    x: cursorOffsetX - (cursorOffsetX - pan.x) * ratio,
    y: cursorOffsetY - (cursorOffsetY - pan.y) * ratio,
  };
}
