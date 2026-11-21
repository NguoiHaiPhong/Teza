export type EditorTool =
  | "crop"
  | "arrow"
  | "rect"
  | "ellipse"
  | "pen"
  | "highlighter"
  | "mosaic"
  | "text";

export type CropHandle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "move";

export interface CropRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Point {
  x: number;
  y: number;
}

export type AnnotationOp =
  | {
      kind: "pen" | "highlighter";
      color: string;
      size: number;
      points: Point[];
    }
  | {
      kind: "arrow" | "rect" | "ellipse";
      color: string;
      size: number;
      x1: number;
      y1: number;
      x2: number;
      y2: number;
    }
  | {
      kind: "mosaic";
      size: number;
      x1: number;
      y1: number;
      x2: number;
      y2: number;
    }
  | {
      kind: "text";
      color: string;
      fontSize: number;
      x: number;
      y: number;
      text: string;
    };

function clampNum(v: number, lo: number, hi: number): number {
  if (!Number.isFinite(v)) return lo;
  if (hi < lo) return lo;
  return Math.max(lo, Math.min(hi, v));
}

export function normalizeRect(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  maxW: number,
  maxH: number,
): CropRect {
  const sx1 = clampNum(x1, 0, maxW);
  const sy1 = clampNum(y1, 0, maxH);
  const sx2 = clampNum(x2, 0, maxW);
  const sy2 = clampNum(y2, 0, maxH);
  const left = Math.round(Math.min(sx1, sx2));
  const top = Math.round(Math.min(sy1, sy2));
  const right = Math.round(Math.max(sx1, sx2));
  const bottom = Math.round(Math.max(sy1, sy2));
  return {
    x: left,
    y: top,
    w: Math.max(0, right - left),
    h: Math.max(0, bottom - top),
  };
}

export function clampCropRect(
  rect: CropRect,
  maxW: number,
  maxH: number,
  minSize = 1,
): CropRect {
  if (maxW <= 0 || maxH <= 0) {
    return { x: 0, y: 0, w: 0, h: 0 };
  }
  const effMinW = Math.min(minSize, maxW);
  const effMinH = Math.min(minSize, maxH);
  const w = Math.round(clampNum(rect.w, effMinW, maxW));
  const h = Math.round(clampNum(rect.h, effMinH, maxH));
  const x = Math.round(clampNum(rect.x, 0, maxW - w));
  const y = Math.round(clampNum(rect.y, 0, maxH - h));
  return { x, y, w, h };
}

export function resizeCropRect(
  start: CropRect,
  handle: CropHandle,
  dx: number,
  dy: number,
  maxW: number,
  maxH: number,
  minSize = 16,
): CropRect {
  if (maxW <= 0 || maxH <= 0) {
    return { x: 0, y: 0, w: 0, h: 0 };
  }
  if (handle === "move") {
    return {
      x: Math.round(clampNum(start.x + dx, 0, Math.max(0, maxW - start.w))),
      y: Math.round(clampNum(start.y + dy, 0, Math.max(0, maxH - start.h))),
      w: Math.round(start.w),
      h: Math.round(start.h),
    };
  }

  const effMinW = Math.min(minSize, maxW);
  const effMinH = Math.min(minSize, maxH);
  let left = start.x;
  let top = start.y;
  let right = start.x + start.w;
  let bottom = start.y + start.h;

  if (handle.includes("w")) {
    left = clampNum(start.x + dx, 0, right - effMinW);
  }
  if (handle.includes("e")) {
    right = clampNum(start.x + start.w + dx, left + effMinW, maxW);
  }
  if (handle.includes("n")) {
    top = clampNum(start.y + dy, 0, bottom - effMinH);
  }
  if (handle.includes("s")) {
    bottom = clampNum(start.y + start.h + dy, top + effMinH, maxH);
  }

  const rx = Math.round(left);
  const ry = Math.round(top);
  return {
    x: rx,
    y: ry,
    w: Math.max(effMinW, Math.round(right) - rx),
    h: Math.max(effMinH, Math.round(bottom) - ry),
  };
}

export function computeArrowHead(
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  headLen: number,
): [Point, Point, Point] {
  const angle = Math.atan2(y2 - y1, x2 - x1);
  const spread = Math.PI / 6.5;
  const tip: Point = { x: x2, y: y2 };
  const leftWing: Point = {
    x: x2 - headLen * Math.cos(angle - spread),
    y: y2 - headLen * Math.sin(angle - spread),
  };
  const rightWing: Point = {
    x: x2 - headLen * Math.cos(angle + spread),
    y: y2 - headLen * Math.sin(angle + spread),
  };
  return [tip, leftWing, rightWing];
}

export function mapAnnotationsAfterCrop(ops: AnnotationOp[], crop: CropRect): AnnotationOp[] {
  return ops.map((op) => {
    switch (op.kind) {
      case "pen":
      case "highlighter":
        return {
          ...op,
          points: op.points.map((p) => ({ x: p.x - crop.x, y: p.y - crop.y })),
        };
      case "arrow":
      case "rect":
      case "ellipse":
      case "mosaic":
        return {
          ...op,
          x1: op.x1 - crop.x,
          y1: op.y1 - crop.y,
          x2: op.x2 - crop.x,
          y2: op.y2 - crop.y,
        };
      case "text":
        return {
          ...op,
          x: op.x - crop.x,
          y: op.y - crop.y,
        };
    }
  });
}

export function buildScreenshotFileName(date: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  const stamp =
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}_` +
    `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  return `screenshot_${stamp}.png`;
}

function applyMosaicBlock(
  ctx: CanvasRenderingContext2D,
  canvasW: number,
  canvasH: number,
  x1: number,
  y1: number,
  x2: number,
  y2: number,
  blockSize: number,
) {
  const rect = normalizeRect(x1, y1, x2, y2, canvasW, canvasH);
  if (rect.w < 2 || rect.h < 2) return;
  const imgData = ctx.getImageData(rect.x, rect.y, rect.w, rect.h);
  const data = imgData.data;
  const step = Math.max(8, Math.round(blockSize));

  for (let by = 0; by < rect.h; by += step) {
    const bh = Math.min(step, rect.h - by);
    for (let bx = 0; bx < rect.w; bx += step) {
      const bw = Math.min(step, rect.w - bx);
      let rSum = 0;
      let gSum = 0;
      let bSum = 0;
      let aSum = 0;
      let count = 0;
      for (let py = by; py < by + bh; py += 1) {
        const rowOff = py * rect.w;
        for (let px = bx; px < bx + bw; px += 1) {
          const idx = (rowOff + px) * 4;
          rSum += data[idx];
          gSum += data[idx + 1];
          bSum += data[idx + 2];
          aSum += data[idx + 3];
          count += 1;
        }
      }
      if (count === 0) continue;
      const rAvg = Math.round(rSum / count);
      const gAvg = Math.round(gSum / count);
      const bAvg = Math.round(bSum / count);
      const aAvg = Math.round(aSum / count);
      for (let py = by; py < by + bh; py += 1) {
        const rowOff = py * rect.w;
        for (let px = bx; px < bx + bw; px += 1) {
          const idx = (rowOff + px) * 4;
          data[idx] = rAvg;
          data[idx + 1] = gAvg;
          data[idx + 2] = bAvg;
          data[idx + 3] = aAvg;
        }
      }
    }
  }
  ctx.putImageData(imgData, rect.x, rect.y);
}

function drawFreehand(ctx: CanvasRenderingContext2D, points: Point[]) {
  if (points.length === 0) return;
  ctx.beginPath();
  ctx.moveTo(points[0].x, points[0].y);
  if (points.length === 1) {
    ctx.lineTo(points[0].x + 0.01, points[0].y + 0.01);
  } else {
    for (let i = 1; i < points.length; i += 1) {
      ctx.lineTo(points[i].x, points[i].y);
    }
  }
  ctx.stroke();
}

export function renderAnnotationOp(
  ctx: CanvasRenderingContext2D,
  canvasW: number,
  canvasH: number,
  op: AnnotationOp,
) {
  if (op.kind === "mosaic") {
    applyMosaicBlock(ctx, canvasW, canvasH, op.x1, op.y1, op.x2, op.y2, op.size);
    return;
  }

  ctx.save();
  if (op.kind === "pen" || op.kind === "highlighter") {
    ctx.strokeStyle = op.color;
    ctx.lineWidth = op.size;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    if (op.kind === "highlighter") {
      ctx.globalAlpha = 0.36;
    }
    drawFreehand(ctx, op.points);
    ctx.restore();
    return;
  }

  if (op.kind === "rect") {
    ctx.strokeStyle = op.color;
    ctx.lineWidth = op.size;
    ctx.lineJoin = "miter";
    const left = Math.min(op.x1, op.x2);
    const top = Math.min(op.y1, op.y2);
    const w = Math.abs(op.x2 - op.x1);
    const h = Math.abs(op.y2 - op.y1);
    ctx.strokeRect(left, top, w, h);
    ctx.restore();
    return;
  }

  if (op.kind === "ellipse") {
    ctx.strokeStyle = op.color;
    ctx.lineWidth = op.size;
    const cx = (op.x1 + op.x2) / 2;
    const cy = (op.y1 + op.y2) / 2;
    const rx = Math.max(0.5, Math.abs(op.x2 - op.x1) / 2);
    const ry = Math.max(0.5, Math.abs(op.y2 - op.y1) / 2);
    ctx.beginPath();
    ctx.ellipse(cx, cy, rx, ry, 0, 0, Math.PI * 2);
    ctx.stroke();
    ctx.restore();
    return;
  }

  if (op.kind === "arrow") {
    const dist = Math.hypot(op.x2 - op.x1, op.y2 - op.y1);
    if (dist < 1) {
      ctx.restore();
      return;
    }
    const headLen = Math.min(Math.max(14, op.size * 3.8), Math.max(14, dist * 0.48));
    const angle = Math.atan2(op.y2 - op.y1, op.x2 - op.x1);
    const shaftEndX = op.x2 - Math.cos(angle) * headLen * 0.58;
    const shaftEndY = op.y2 - Math.sin(angle) * headLen * 0.58;

    ctx.strokeStyle = op.color;
    ctx.fillStyle = op.color;
    ctx.lineWidth = op.size;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(op.x1, op.y1);
    ctx.lineTo(shaftEndX, shaftEndY);
    ctx.stroke();

    const [tip, leftWing, rightWing] = computeArrowHead(op.x1, op.y1, op.x2, op.y2, headLen);
    ctx.beginPath();
    ctx.moveTo(tip.x, tip.y);
    ctx.lineTo(leftWing.x, leftWing.y);
    ctx.lineTo(rightWing.x, rightWing.y);
    ctx.closePath();
    ctx.fill();
    ctx.restore();
    return;
  }

  if (op.kind === "text") {
    const lines = op.text.split(/\r?\n/);
    const lineHeight = Math.round(op.fontSize * 1.24);
    ctx.font = `700 ${op.fontSize}px "Segoe UI", system-ui, -apple-system, sans-serif`;
    ctx.textBaseline = "top";
    ctx.lineJoin = "round";
    ctx.lineWidth = Math.max(2, Math.round(op.fontSize * 0.14));
    ctx.strokeStyle =
      op.color.toLowerCase() === "#111827"
        ? "rgba(255,255,255,0.88)"
        : "rgba(0,0,0,0.72)";
    ctx.fillStyle = op.color;
    lines.forEach((line, i) => {
      const ly = op.y + i * lineHeight;
      ctx.strokeText(line, op.x, ly);
      ctx.fillText(line, op.x, ly);
    });
    ctx.restore();
  }
}

export function renderAnnotatedCanvas(
  ctx: CanvasRenderingContext2D,
  sourceImage: HTMLImageElement,
  canvasW: number,
  canvasH: number,
  ops: AnnotationOp[],
  cropRect?: CropRect | null,
) {
  ctx.clearRect(0, 0, canvasW, canvasH);
  const srcW = sourceImage.naturalWidth || canvasW;
  const srcH = sourceImage.naturalHeight || canvasH;
  if (cropRect && cropRect.w > 0 && cropRect.h > 0) {
    ctx.drawImage(
      sourceImage,
      cropRect.x,
      cropRect.y,
      cropRect.w,
      cropRect.h,
      0,
      0,
      canvasW,
      canvasH,
    );
  } else {
    ctx.drawImage(sourceImage, 0, 0, srcW, srcH, 0, 0, canvasW, canvasH);
  }

  const effectiveOps =
    cropRect && cropRect.w > 0 && cropRect.h > 0
      ? mapAnnotationsAfterCrop(ops, cropRect)
      : ops;

  for (const op of effectiveOps) {
    renderAnnotationOp(ctx, canvasW, canvasH, op);
  }
}
