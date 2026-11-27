import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  Circle,
  Copy,
  Crop,
  Download,
  Grid3x3,
  Highlighter,
  Loader2,
  Monitor,
  MoveUpRight,
  Paperclip,
  Pencil,
  Redo2,
  RotateCcw,
  Send,
  Square,
  Trash2,
  Type,
  Undo2,
  X,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { MonitorInfoDto, PendingAttachment } from "../types";
import { copyImageReliable } from "../utils/clipboard";
import {
  type AnnotationOp,
  buildScreenshotFileName,
  clampCropRect,
  type CropHandle,
  type CropRect,
  type EditorTool,
  normalizeRect,
  renderAnnotatedCanvas,
  resizeCropRect,
} from "../utils/screenshotEditor";
import { isImeComposingEvent } from "../utils/searchNormalize";

interface Props {
  open: boolean;
  isLocked?: boolean;
  sourceUrl: string;
  initialMode: "crop" | "annotate";
  monitors: MonitorInfoDto[];
  activeMonitorIndex: number | null;
  hideWindowOnCapture: boolean;
  capturing?: boolean;
  canRecapture?: boolean;
  onToggleHideWindow: (val: boolean) => void;
  onRecapture?: (opts: { monitorIndex: number | null; captureAll: boolean }) => void;
  onClose: () => void;
  onAttach: (
    attachment: PendingAttachment,
    caption: string,
  ) => boolean | void | Promise<boolean | void>;
  onSend: (attachment: PendingAttachment, caption: string) => Promise<void | unknown> | void;
  onError: (err: unknown) => void;
}

interface AnnotationHistory {
  ops: AnnotationOp[];
  past: AnnotationOp[][];
  future: AnnotationOp[][];
}

const EMPTY_HISTORY: AnnotationHistory = {
  ops: [],
  past: [],
  future: [],
};

const MAX_HISTORY_DEPTH = 100;

function clampTextCoordsToView(
  x: number,
  y: number,
  text: string,
  fontSize: number,
  view: CropRect,
  canvas?: HTMLCanvasElement | null,
): { x: number; y: number } {
  if (view.w <= 0 || view.h <= 0) return { x, y };
  const lines = text.split(/\r?\n/);
  const lineHeight = Math.round(fontSize * 1.24);
  const totalHeight = Math.max(lineHeight, lines.length * lineHeight);
  let maxWidth = 0;
  const ctx = canvas?.getContext("2d");
  if (ctx) {
    ctx.save();
    ctx.font = `700 ${fontSize}px "Segoe UI", system-ui, -apple-system, sans-serif`;
    for (const line of lines) {
      maxWidth = Math.max(maxWidth, ctx.measureText(line).width);
    }
    ctx.restore();
  } else {
    for (const line of lines) {
      maxWidth = Math.max(maxWidth, line.length * fontSize * 0.6);
    }
  }
  const pad = Math.max(4, Math.round(fontSize * 0.18));
  const minX = view.x + pad;
  const maxX = Math.max(minX, view.x + view.w - maxWidth - pad);
  const minY = view.y + pad;
  const maxY = Math.max(minY, view.y + view.h - totalHeight - pad);
  return {
    x: Math.max(minX, Math.min(maxX, x)),
    y: Math.max(minY, Math.min(maxY, y)),
  };
}

const COLORS = ["#ef4444", "#facc15", "#22c55e", "#3b82f6", "#ffffff", "#111827"] as const;

const SIZE_PRESETS = [
  { id: "sm", label: "S", stroke: 3, wide: 14, font: 18 },
  { id: "md", label: "M", stroke: 5, wide: 22, font: 26 },
  { id: "lg", label: "L", stroke: 9, wide: 34, font: 38 },
] as const;

const HANDLES: { id: CropHandle; cursor: string; posClass: string }[] = [
  { id: "nw", cursor: "nwse-resize", posClass: "-left-1.5 -top-1.5" },
  { id: "n", cursor: "ns-resize", posClass: "left-1/2 -top-1.5 -translate-x-1/2" },
  { id: "ne", cursor: "nesw-resize", posClass: "-right-1.5 -top-1.5" },
  { id: "e", cursor: "ew-resize", posClass: "-right-1.5 top-1/2 -translate-y-1/2" },
  { id: "se", cursor: "nwse-resize", posClass: "-bottom-1.5 -right-1.5" },
  { id: "s", cursor: "ns-resize", posClass: "-bottom-1.5 left-1/2 -translate-x-1/2" },
  { id: "sw", cursor: "nesw-resize", posClass: "-bottom-1.5 -left-1.5" },
  { id: "w", cursor: "ew-resize", posClass: "-left-1.5 top-1/2 -translate-y-1/2" },
];

export function ScreenshotEditorModal({
  open,
  isLocked = false,
  sourceUrl,
  initialMode,
  monitors,
  activeMonitorIndex,
  hideWindowOnCapture,
  capturing = false,
  canRecapture = false,
  onToggleHideWindow,
  onRecapture,
  onClose,
  onAttach,
  onSend,
  onError,
}: Props) {
  const { t } = useI18n();

  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const textInputRef = useRef<HTMLInputElement | null>(null);
  const noticeTimerRef = useRef<number | null>(null);
  const drawDragRef = useRef<
    | null
    | { kind: "crop-draw"; startX: number; startY: number }
    | { kind: "crop-handle"; handle: CropHandle; startX: number; startY: number; startRect: CropRect }
    | { kind: "op" }
  >(null);

  const [imgEl, setImgEl] = useState<HTMLImageElement | null>(null);
  const [natSize, setNatSize] = useState({ w: 0, h: 0 });
  const [tool, setTool] = useState<EditorTool>(initialMode === "crop" ? "crop" : "arrow");
  const [color, setColor] = useState<string>(COLORS[0]);
  const [sizeIdx, setSizeIdx] = useState<number>(1);
  const [history, setHistory] = useState<AnnotationHistory>(EMPTY_HISTORY);
  const ops = history.ops;
  const canUndo = history.past.length > 0;
  const canRedo = history.future.length > 0;
  const [activeOp, setActiveOp] = useState<AnnotationOp | null>(null);
  const activeOpRef = useRef<AnnotationOp | null>(null);
  const updateActiveOp = (next: AnnotationOp | null) => {
    activeOpRef.current = next;
    setActiveOp(next);
  };
  const [appliedCrop, setAppliedCrop] = useState<CropRect | null>(null);
  const [cropDraft, setCropDraft] = useState<CropRect | null>(null);
  const [textDraft, setTextDraft] = useState<{
    natX: number;
    natY: number;
    pctX: number;
    pctY: number;
    value: string;
  } | null>(null);
  const [caption, setCaption] = useState("");
  const [busyAction, setBusyAction] = useState<"copy" | "save" | "attach" | "send" | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  const showTransientNotice = useCallback((msg: string) => {
    setNotice(msg);
    if (noticeTimerRef.current !== null) {
      window.clearTimeout(noticeTimerRef.current);
    }
    noticeTimerRef.current = window.setTimeout(() => {
      noticeTimerRef.current = null;
      setNotice(null);
    }, 2600);
  }, []);

  useEffect(() => {
    return () => {
      if (noticeTimerRef.current !== null) {
        window.clearTimeout(noticeTimerRef.current);
        noticeTimerRef.current = null;
      }
    };
  }, []);

  const pushAnnotationOp = useCallback((op: AnnotationOp) => {
    setHistory((prev) => ({
      ops: [...prev.ops, op],
      past: [...prev.past.slice(-(MAX_HISTORY_DEPTH - 1)), prev.ops],
      future: [],
    }));
  }, []);

  // Reset state whenever modal opens or source capture image changes
  useEffect(() => {
    if (!open || !sourceUrl) return;
    setImgEl(null);
    setNatSize({ w: 0, h: 0 });
    setHistory(EMPTY_HISTORY);
    updateActiveOp(null);
    setAppliedCrop(null);
    setCropDraft(null);
    setTextDraft(null);
    setNotice(null);
    setConfirmDiscard(false);
    setTool(initialMode === "crop" ? "crop" : "arrow");

    let cancelled = false;
    const img = new Image();
    img.onload = () => {
      if (cancelled) return;
      setImgEl(img);
      setNatSize({ w: img.naturalWidth, h: img.naturalHeight });
    };
    img.onerror = () => {
      if (!cancelled) {
        onError("Không thể tải ảnh để chỉnh sửa");
      }
    };
    img.src = sourceUrl;
    return () => {
      cancelled = true;
    };
  }, [open, sourceUrl, initialMode, onError]);

  useEffect(() => {
    if (!open) {
      setCaption("");
      setTextDraft(null);
      setConfirmDiscard(false);
    }
  }, [open]);

  const activeView: CropRect = useMemo(() => {
    if (natSize.w <= 0 || natSize.h <= 0) {
      return { x: 0, y: 0, w: 0, h: 0 };
    }
    if (tool === "crop" || !appliedCrop || appliedCrop.w < 4 || appliedCrop.h < 4) {
      return { x: 0, y: 0, w: natSize.w, h: natSize.h };
    }
    return clampCropRect(appliedCrop, natSize.w, natSize.h, 4);
  }, [natSize, tool, appliedCrop]);

  const resScale = useMemo(() => {
    const longest = Math.max(natSize.w, natSize.h, 1);
    return Math.max(1, Math.min(2.6, longest / 1150));
  }, [natSize]);

  // Redraw working canvas cleanly whenever image, view crop, or ops change
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !imgEl || activeView.w <= 0 || activeView.h <= 0) return;
    if (canvas.width !== activeView.w || canvas.height !== activeView.h) {
      canvas.width = activeView.w;
      canvas.height = activeView.h;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const list = activeOp ? [...ops, activeOp] : ops;
    const cropArg =
      activeView.x === 0 &&
      activeView.y === 0 &&
      activeView.w === natSize.w &&
      activeView.h === natSize.h
        ? null
        : activeView;
    renderAnnotatedCanvas(ctx, imgEl, activeView.w, activeView.h, list, cropArg);
  }, [imgEl, activeView, natSize, ops, activeOp]);

  const selectTool = useCallback(
    (next: EditorTool) => {
      setTextDraft(null);
      if (tool === "crop" && next !== "crop") {
        if (cropDraft && cropDraft.w >= 8 && cropDraft.h >= 8) {
          setAppliedCrop(clampCropRect(cropDraft, natSize.w, natSize.h, 8));
        }
      } else if (next === "crop" && tool !== "crop" && appliedCrop) {
        setCropDraft(appliedCrop);
      }
      setTool(next);
    },
    [tool, cropDraft, appliedCrop, natSize],
  );

  const applyCropSelection = useCallback(() => {
    if (!cropDraft || cropDraft.w < 8 || cropDraft.h < 8) return;
    const clamped = clampCropRect(cropDraft, natSize.w, natSize.h, 8);
    setAppliedCrop(clamped);
    setCropDraft(clamped);
    setTool("arrow");
  }, [cropDraft, natSize]);

  const handleResetCrop = useCallback(() => {
    setAppliedCrop(null);
    setCropDraft(null);
  }, []);

  const handleUndo = useCallback(() => {
    setHistory((prev) => {
      if (prev.past.length === 0) return prev;
      const previousOps = prev.past[prev.past.length - 1];
      return {
        ops: previousOps,
        past: prev.past.slice(0, -1),
        future: [...prev.future, prev.ops],
      };
    });
  }, []);

  const handleRedo = useCallback(() => {
    setHistory((prev) => {
      if (prev.future.length === 0) return prev;
      const nextOps = prev.future[prev.future.length - 1];
      return {
        ops: nextOps,
        past: [...prev.past.slice(-(MAX_HISTORY_DEPTH - 1)), prev.ops],
        future: prev.future.slice(0, -1),
      };
    });
  }, []);

  const handleClearAll = useCallback(() => {
    setHistory((prev) => {
      if (prev.ops.length === 0) return prev;
      return {
        ops: [],
        past: [...prev.past.slice(-(MAX_HISTORY_DEPTH - 1)), prev.ops],
        future: [],
      };
    });
  }, []);

  const requestClose = useCallback(() => {
    if (busyAction !== null) return;
    const pendingTextCount = textDraft && textDraft.value.trim() ? 1 : 0;
    if (ops.length + pendingTextCount > 0) {
      setConfirmDiscard(true);
      return;
    }
    onClose();
  }, [busyAction, ops.length, textDraft, onClose]);

  const commitTextDraft = useCallback(() => {
    if (!textDraft) return;
    const trimmed = textDraft.value.trim();
    if (trimmed) {
      const preset = SIZE_PRESETS[sizeIdx] ?? SIZE_PRESETS[1];
      const fontSize = Math.round(preset.font * resScale);
      const clamped = clampTextCoordsToView(
        textDraft.natX,
        textDraft.natY,
        trimmed,
        fontSize,
        activeView,
        canvasRef.current,
      );
      const op: AnnotationOp = {
        kind: "text",
        color,
        fontSize,
        x: clamped.x,
        y: clamped.y,
        text: trimmed,
      };
      pushAnnotationOp(op);
    }
    setTextDraft(null);
  }, [textDraft, sizeIdx, resScale, activeView, color, pushAnnotationOp]);

  const effectiveExportRect = useCallback((): CropRect => {
    if (tool === "crop" && cropDraft && cropDraft.w >= 8 && cropDraft.h >= 8) {
      return clampCropRect(cropDraft, natSize.w, natSize.h, 8);
    }
    if (appliedCrop && appliedCrop.w >= 8 && appliedCrop.h >= 8) {
      return clampCropRect(appliedCrop, natSize.w, natSize.h, 8);
    }
    return { x: 0, y: 0, w: natSize.w, h: natSize.h };
  }, [tool, cropDraft, appliedCrop, natSize]);

  const exportPng = useCallback(async (): Promise<{
    file: File;
    dataUrl: string;
    fileName: string;
    sizeBytes: number;
  }> => {
    if (!imgEl || natSize.w <= 0 || natSize.h <= 0) {
      throw new Error("Ảnh chưa sẵn sàng để xuất");
    }
    const rect = effectiveExportRect();
    const pendingFontSize = Math.round((SIZE_PRESETS[sizeIdx] ?? SIZE_PRESETS[1]).font * resScale);
    const pendingTrimmed = textDraft?.value.trim() ?? "";
    const pendingOps =
      textDraft && pendingTrimmed
        ? (() => {
            const clamped = clampTextCoordsToView(
              textDraft.natX,
              textDraft.natY,
              pendingTrimmed,
              pendingFontSize,
              rect,
              canvasRef.current,
            );
            return [
              ...ops,
              {
                kind: "text" as const,
                color,
                fontSize: pendingFontSize,
                x: clamped.x,
                y: clamped.y,
                text: pendingTrimmed,
              },
            ];
          })()
        : ops;

    const off = document.createElement("canvas");
    off.width = Math.max(1, rect.w);
    off.height = Math.max(1, rect.h);
    const ctx = off.getContext("2d");
    if (!ctx) throw new Error("Không khởi tạo được canvas xuất ảnh");

    const cropArg =
      rect.x === 0 && rect.y === 0 && rect.w === natSize.w && rect.h === natSize.h
        ? null
        : rect;
    renderAnnotatedCanvas(ctx, imgEl, off.width, off.height, pendingOps, cropArg);

    const blob = await new Promise<Blob>((resolve, reject) => {
      off.toBlob(
        (b) => (b ? resolve(b) : reject(new Error("Không thể đóng gói PNG"))),
        "image/png",
      );
    });
    const dataUrl = off.toDataURL("image/png");
    const fileName = buildScreenshotFileName();
    const file = new File([blob], fileName, { type: "image/png" });
    return { file, dataUrl, fileName, sizeBytes: file.size };
  }, [imgEl, natSize, textDraft, ops, color, sizeIdx, resScale, effectiveExportRect]);

  const handleCopy = useCallback(async () => {
    if (busyAction || !imgEl) return;
    setBusyAction("copy");
    try {
      const { dataUrl } = await exportPng();
      await copyImageReliable(dataUrl);
      showTransientNotice(t("screenshotCopiedNotice"));
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  }, [busyAction, imgEl, exportPng, showTransientNotice, t, onError]);

  const handleSave = useCallback(async () => {
    if (busyAction || !imgEl) return;
    setBusyAction("save");
    try {
      const { dataUrl, fileName } = await exportPng();
      const savedPath = await bridge.saveRemoteFile(dataUrl, fileName);
      if (savedPath) {
        showTransientNotice(t("screenshotSavedNotice"));
      }
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  }, [busyAction, imgEl, exportPng, showTransientNotice, t, onError]);

  const handleAttachOnly = useCallback(async () => {
    if (busyAction || !imgEl) return;
    setBusyAction("attach");
    try {
      const { file, dataUrl, fileName, sizeBytes } = await exportPng();
      const attached = await onAttach(
        {
          kind: "raw",
          file,
          fileName,
          sizeBytes,
          isImage: true,
          previewUrl: dataUrl,
        },
        caption.trim(),
      );
      if (attached === false) return;
      onClose();
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  }, [busyAction, imgEl, exportPng, onAttach, caption, onClose, onError]);

  const handleSendNow = useCallback(async () => {
    if (busyAction || !imgEl) return;
    setBusyAction("send");
    try {
      const { file, dataUrl, fileName, sizeBytes } = await exportPng();
      const sent = await onSend(
        {
          kind: "raw",
          file,
          fileName,
          sizeBytes,
          isImage: true,
          previewUrl: dataUrl,
        },
        caption.trim(),
      );
      if (sent === false) return;
      onClose();
    } catch (err) {
      onError(err);
    } finally {
      setBusyAction(null);
    }
  }, [busyAction, imgEl, exportPng, onSend, caption, onClose, onError]);

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked) return;
      if (isImeComposingEvent(e)) return;
      const inInput =
        document.activeElement instanceof HTMLInputElement ||
        document.activeElement instanceof HTMLTextAreaElement;

      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (confirmDiscard) {
          setConfirmDiscard(false);
        } else if (textDraft) {
          setTextDraft(null);
        } else {
          requestClose();
        }
        return;
      }

      if (confirmDiscard) return;

      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        void handleSendNow();
        return;
      }

      if (inInput) return;

      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z") {
        e.preventDefault();
        handleUndo();
      } else if (
        (e.ctrlKey || e.metaKey) &&
        (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z"))
      ) {
        e.preventDefault();
        handleRedo();
      } else if (
        e.key === "Enter" &&
        tool === "crop" &&
        cropDraft &&
        cropDraft.w >= 8 &&
        cropDraft.h >= 8
      ) {
        e.preventDefault();
        applyCropSelection();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [
    open,
    isLocked,
    confirmDiscard,
    textDraft,
    tool,
    cropDraft,
    requestClose,
    handleUndo,
    handleRedo,
    applyCropSelection,
    handleSendNow,
  ]);

  const toCanvasCoords = (clientX: number, clientY: number) => {
    const canvas = canvasRef.current;
    if (!canvas) return { viewX: 0, viewY: 0, natX: 0, natY: 0, pctX: 0, pctY: 0 };
    const rect = canvas.getBoundingClientRect();
    const rx = rect.width > 0 ? Math.max(0, Math.min(1, (clientX - rect.left) / rect.width)) : 0;
    const ry = rect.height > 0 ? Math.max(0, Math.min(1, (clientY - rect.top) / rect.height)) : 0;
    const viewX = rx * activeView.w;
    const viewY = ry * activeView.h;
    return {
      viewX,
      viewY,
      natX: viewX + activeView.x,
      natY: viewY + activeView.y,
      pctX: rx * 100,
      pctY: ry * 100,
    };
  };

  const handleCanvasPointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!imgEl || e.button !== 0) return;
    if (textDraft) {
      commitTextDraft();
      return;
    }
    const { natX, natY, pctX, pctY } = toCanvasCoords(e.clientX, e.clientY);

    if (tool === "text") {
      e.preventDefault();
      const clampedPctX = Math.min(82, Math.max(2, pctX));
      const clampedPctY = Math.min(88, Math.max(2, pctY));
      const clampedNatX = activeView.x + (clampedPctX / 100) * activeView.w;
      const clampedNatY = activeView.y + (clampedPctY / 100) * activeView.h;
      setTextDraft({
        natX: clampedNatX,
        natY: clampedNatY,
        pctX: clampedPctX,
        pctY: clampedPctY,
        value: "",
      });
      setTimeout(() => textInputRef.current?.focus(), 20);
      return;
    }

    if (tool === "crop") {
      e.currentTarget.setPointerCapture(e.pointerId);
      drawDragRef.current = { kind: "crop-draw", startX: natX, startY: natY };
      setCropDraft({ x: Math.round(natX), y: Math.round(natY), w: 0, h: 0 });
      return;
    }

    e.currentTarget.setPointerCapture(e.pointerId);
    const preset = SIZE_PRESETS[sizeIdx] ?? SIZE_PRESETS[1];
    const strokeW = Math.round(preset.stroke * resScale);
    const wideW = Math.round(preset.wide * resScale);
    drawDragRef.current = { kind: "op" };

    if (tool === "pen" || tool === "highlighter") {
      updateActiveOp({
        kind: tool,
        color,
        size: tool === "highlighter" ? wideW : strokeW,
        points: [{ x: natX, y: natY }],
      });
    } else if (tool === "mosaic") {
      updateActiveOp({
        kind: "mosaic",
        size: wideW,
        x1: natX,
        y1: natY,
        x2: natX,
        y2: natY,
      });
    } else {
      updateActiveOp({
        kind: tool,
        color,
        size: strokeW,
        x1: natX,
        y1: natY,
        x2: natX,
        y2: natY,
      });
    }
  };

  const handleCanvasPointerMove = (e: React.PointerEvent<HTMLElement>) => {
    const drag = drawDragRef.current;
    if (!drag) return;
    const { natX, natY } = toCanvasCoords(e.clientX, e.clientY);

    if (drag.kind === "crop-draw") {
      setCropDraft(normalizeRect(drag.startX, drag.startY, natX, natY, natSize.w, natSize.h));
      return;
    }
    if (drag.kind === "crop-handle") {
      setCropDraft(
        resizeCropRect(
          drag.startRect,
          drag.handle,
          natX - drag.startX,
          natY - drag.startY,
          natSize.w,
          natSize.h,
          16,
        ),
      );
      return;
    }
    if (drag.kind === "op") {
      const prev = activeOpRef.current;
      if (!prev || prev.kind === "text") return;
      if (prev.kind === "pen" || prev.kind === "highlighter") {
        updateActiveOp({ ...prev, points: [...prev.points, { x: natX, y: natY }] });
      } else if (
        prev.kind === "arrow" ||
        prev.kind === "rect" ||
        prev.kind === "ellipse" ||
        prev.kind === "mosaic"
      ) {
        updateActiveOp({ ...prev, x2: natX, y2: natY });
      }
    }
  };

  const finishCanvasDrag = () => {
    const drag = drawDragRef.current;
    drawDragRef.current = null;
    if (!drag) return;
    if (drag.kind === "crop-draw") {
      setCropDraft((prev) => (prev && prev.w >= 8 && prev.h >= 8 ? prev : null));
      return;
    }
    if (drag.kind === "op") {
      const done = activeOpRef.current;
      updateActiveOp(null);
      if (!done) return;
      if (done.kind === "pen" || done.kind === "highlighter") {
        pushAnnotationOp(done);
      } else if (
        done.kind === "arrow" ||
        done.kind === "rect" ||
        done.kind === "ellipse" ||
        done.kind === "mosaic"
      ) {
        const dist = Math.hypot(done.x2 - done.x1, done.y2 - done.y1);
        if (dist >= 3) {
          pushAnnotationOp(done);
        }
      }
    }
  };

  const startCropHandleDrag = (handle: CropHandle, e: React.PointerEvent<HTMLElement>) => {
    if (!cropDraft) return;
    e.preventDefault();
    e.stopPropagation();
    const { natX, natY } = toCanvasCoords(e.clientX, e.clientY);
    e.currentTarget.setPointerCapture(e.pointerId);
    drawDragRef.current = {
      kind: "crop-handle",
      handle,
      startX: natX,
      startY: natY,
      startRect: cropDraft,
    };
  };

  if (!open) return null;

  const displayDims =
    tool === "crop" && cropDraft && cropDraft.w >= 8 && cropDraft.h >= 8
      ? { w: cropDraft.w, h: cropDraft.h }
      : { w: activeView.w || natSize.w, h: activeView.h || natSize.h };

  const toolButtons: { id: EditorTool; label: string; icon: typeof Crop }[] = [
    { id: "crop", label: t("toolCrop"), icon: Crop },
    { id: "arrow", label: t("toolArrow"), icon: MoveUpRight },
    { id: "rect", label: t("toolRect"), icon: Square },
    { id: "ellipse", label: t("toolEllipse"), icon: Circle },
    { id: "pen", label: t("toolPen"), icon: Pencil },
    { id: "highlighter", label: t("toolHighlighter"), icon: Highlighter },
    { id: "mosaic", label: t("toolMosaic"), icon: Grid3x3 },
    { id: "text", label: t("toolText"), icon: Type },
  ];

  const hasValidCropDraft = tool === "crop" && cropDraft && cropDraft.w >= 8 && cropDraft.h >= 8;

  return (
    <div
      className="fixed inset-0 z-[70] flex flex-col bg-black/90 text-slate-100 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      aria-label={t("screenshotEditorTitle")}
    >
      {/* Top Header */}
      <div
        className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-2.5"
        style={{ background: "var(--bg-sidebar)", borderColor: "var(--border-subtle)" }}
      >
        <div className="flex flex-wrap items-center gap-2.5">
          <span className="text-sm font-semibold">{t("screenshotEditorTitle")}</span>
          {displayDims.w > 0 && (
            <span
              className="rounded-md px-2 py-0.5 text-[11px] font-medium tabular-nums"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              {displayDims.w} × {displayDims.h} px
            </span>
          )}
          {canRecapture && monitors.length > 1 && onRecapture && (
            <div
              className="flex items-center gap-1 rounded-lg p-0.5"
              style={{ background: "var(--bg-surface)" }}
            >
              {monitors.map((m) => {
                const active = activeMonitorIndex === m.index;
                return (
                  <button
                    key={m.index}
                    type="button"
                    disabled={capturing}
                    onClick={() => onRecapture({ monitorIndex: m.index, captureAll: false })}
                    className="flex items-center gap-1 rounded-md px-2 py-1 text-[11.5px] font-medium transition disabled:opacity-50"
                    style={{
                      background: active ? "var(--accent)" : "transparent",
                      color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                    }}
                  >
                    <Monitor className="h-3 w-3" />
                    <span>{m.label}</span>
                  </button>
                );
              })}
              <button
                type="button"
                disabled={capturing}
                onClick={() => onRecapture({ monitorIndex: null, captureAll: true })}
                className="rounded-md px-2 py-1 text-[11.5px] font-medium transition disabled:opacity-50"
                style={{
                  background: activeMonitorIndex === null ? "var(--accent)" : "transparent",
                  color: activeMonitorIndex === null ? "var(--text-on-accent)" : "var(--text-secondary)",
                }}
              >
                {t("screenshotAllMonitors")}
              </button>
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {canRecapture && onRecapture && (
            <>
              <label
                className="flex cursor-pointer items-center gap-1.5 text-xs select-none"
                style={{ color: "var(--text-secondary)" }}
              >
                <input
                  type="checkbox"
                  checked={hideWindowOnCapture}
                  onChange={(e) => onToggleHideWindow(e.target.checked)}
                  className="h-3.5 w-3.5 cursor-pointer accent-[var(--accent)]"
                />
                <span>{t("screenshotHideWindowLabel")}</span>
              </label>
              <button
                type="button"
                disabled={capturing}
                onClick={() =>
                  onRecapture({
                    monitorIndex: activeMonitorIndex,
                    captureAll: activeMonitorIndex === null && monitors.length > 1,
                  })
                }
                className="hover-surface flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium transition disabled:opacity-50"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {capturing ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Crop className="h-3.5 w-3.5" />
                )}
                <span>{t("screenshotRecaptureBtn")}</span>
              </button>
            </>
          )}
          <button
            type="button"
            disabled={busyAction !== null}
            onClick={requestClose}
            className="hover-surface rounded-lg p-1.5 transition disabled:opacity-40"
            style={{ color: "var(--text-secondary)" }}
            title={t("close")}
          >
            <X className="h-5 w-5" />
          </button>
        </div>
      </div>

      {/* Tool Strip */}
      <div
        className="flex flex-wrap items-center justify-center gap-2 border-b px-4 py-2"
        style={{ background: "rgba(20, 20, 24, 0.92)", borderColor: "var(--border-subtle)" }}
      >
        <div className="flex items-center gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
          {toolButtons.map((btn) => {
            const Icon = btn.icon;
            const active = tool === btn.id;
            return (
              <button
                key={btn.id}
                type="button"
                title={btn.label}
                onClick={() => selectTool(btn.id)}
                className="flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs font-medium transition"
                style={{
                  background: active ? "var(--accent)" : "transparent",
                  color: active ? "var(--text-on-accent)" : "var(--text-primary)",
                }}
              >
                <Icon className="h-4 w-4" />
                <span className="hidden xl:inline">{btn.label}</span>
              </button>
            );
          })}
        </div>

        {/* Color Swatches */}
        <div className="flex items-center gap-1.5 rounded-xl px-2 py-1.5" style={{ background: "var(--bg-surface)" }}>
          {COLORS.map((swatch) => (
            <button
              key={swatch}
              type="button"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => setColor(swatch)}
              className="h-5 w-5 rounded-full transition"
              style={{
                background: swatch,
                border: swatch === "#111827" ? "1px solid rgba(255,255,255,0.4)" : "1px solid rgba(0,0,0,0.25)",
                boxShadow:
                  color === swatch
                    ? "0 0 0 2px #111827, 0 0 0 4px var(--accent)"
                    : undefined,
                transform: color === swatch ? "scale(1.1)" : undefined,
              }}
              aria-label={swatch}
            />
          ))}
        </div>

        {/* Stroke Sizes */}
        <div className="flex items-center gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
          {SIZE_PRESETS.map((sz, idx) => (
            <button
              key={sz.id}
              type="button"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => setSizeIdx(idx)}
              className="rounded-lg px-2 py-1 text-xs font-semibold transition"
              style={{
                background: sizeIdx === idx ? "var(--bg-hover)" : "transparent",
                color: sizeIdx === idx ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              {sz.label}
            </button>
          ))}
        </div>

        {/* Undo / Redo / Clear / Reset Crop */}
        <div className="flex items-center gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
          <button
            type="button"
            disabled={!canUndo}
            onClick={handleUndo}
            title={t("screenshotUndoTitle")}
            className="hover-surface rounded-lg p-1.5 transition disabled:opacity-35"
            style={{ color: "var(--text-primary)" }}
          >
            <Undo2 className="h-4 w-4" />
          </button>
          <button
            type="button"
            disabled={!canRedo}
            onClick={handleRedo}
            title={t("screenshotRedoTitle")}
            className="hover-surface rounded-lg p-1.5 transition disabled:opacity-35"
            style={{ color: "var(--text-primary)" }}
          >
            <Redo2 className="h-4 w-4" />
          </button>
          <button
            type="button"
            disabled={ops.length === 0}
            onClick={handleClearAll}
            title={t("screenshotClearAnnotationsTitle")}
            className="hover-surface rounded-lg p-1.5 transition disabled:opacity-35"
            style={{ color: "var(--text-primary)" }}
          >
            <Trash2 className="h-4 w-4" />
          </button>
          {(appliedCrop || cropDraft) && (
            <button
              type="button"
              onClick={handleResetCrop}
              title={t("screenshotResetCropBtn")}
              className="hover-surface flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition"
              style={{ color: "var(--accent)" }}
            >
              <RotateCcw className="h-3.5 w-3.5" />
              <span>{t("screenshotResetCropBtn")}</span>
            </button>
          )}
        </div>
      </div>

      {/* Main Canvas Stage */}
      <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden p-4 select-none">
        {!imgEl ? (
          <div className="flex items-center gap-2 text-sm text-slate-300">
            <Loader2 className="h-5 w-5 animate-spin" />
            <span>{t("loading")}</span>
          </div>
        ) : (
          <div
            className="relative flex max-h-full max-w-full items-center justify-center shadow-2xl"
            style={{
              aspectRatio: `${activeView.w} / ${activeView.h}`,
              maxHeight: "100%",
              maxWidth: "100%",
            }}
          >
            <canvas
              ref={canvasRef}
              onPointerDown={handleCanvasPointerDown}
              onPointerMove={handleCanvasPointerMove}
              onPointerUp={finishCanvasDrag}
              onPointerCancel={finishCanvasDrag}
              className="block max-h-full max-w-full touch-none rounded border border-white/15 bg-black"
              style={{
                width: "100%",
                height: "100%",
                cursor: tool === "text" ? "text" : "crosshair",
              }}
            />

            {/* Interactive Crop Selection Overlay */}
            {tool === "crop" && natSize.w > 0 && natSize.h > 0 && (
              <>
                {/* Layer 1: Clipped outer dark mask */}
                <div className="pointer-events-none absolute inset-0 overflow-hidden rounded">
                  {!cropDraft ? (
                    <div className="flex h-full w-full items-start justify-center pt-4">
                      <span className="rounded-full bg-black/70 px-3 py-1 text-xs text-slate-200 backdrop-blur">
                        {t("toolCrop")} — {natSize.w} × {natSize.h}
                      </span>
                    </div>
                  ) : (
                    <div
                      className="pointer-events-none absolute"
                      style={{
                        left: `${(cropDraft.x / natSize.w) * 100}%`,
                        top: `${(cropDraft.y / natSize.h) * 100}%`,
                        width: `${(cropDraft.w / natSize.w) * 100}%`,
                        height: `${(cropDraft.h / natSize.h) * 100}%`,
                        boxShadow: "0 0 0 9999px rgba(0, 0, 0, 0.56)",
                      }}
                    />
                  )}
                </div>

                {/* Layer 2: Unclipped crop frame, resize handles, and floating toolbar */}
                {cropDraft && (
                  <div className="pointer-events-none absolute inset-0">
                    <div
                      className="pointer-events-auto absolute border-2"
                      onPointerDown={(e) => startCropHandleDrag("move", e)}
                      onPointerMove={handleCanvasPointerMove}
                      onPointerUp={finishCanvasDrag}
                      onPointerCancel={finishCanvasDrag}
                      style={{
                        left: `${(cropDraft.x / natSize.w) * 100}%`,
                        top: `${(cropDraft.y / natSize.h) * 100}%`,
                        width: `${(cropDraft.w / natSize.w) * 100}%`,
                        height: `${(cropDraft.h / natSize.h) * 100}%`,
                        borderColor: "var(--accent)",
                        cursor: "move",
                      }}
                    >
                      {/* Rule-of-thirds grid */}
                      <div className="pointer-events-none absolute inset-0 grid grid-cols-3 grid-rows-3 opacity-35">
                        {Array.from({ length: 9 }).map((_, idx) => (
                          <div key={idx} className="border border-white/40" />
                        ))}
                      </div>

                      {/* 8 Resize Handles (outside overflow-hidden so edges never clip) */}
                      {cropDraft.w >= 8 &&
                        cropDraft.h >= 8 &&
                        HANDLES.map((h) => (
                          <div
                            key={h.id}
                            onPointerDown={(e) => startCropHandleDrag(h.id, e)}
                            onPointerMove={handleCanvasPointerMove}
                            onPointerUp={finishCanvasDrag}
                            onPointerCancel={finishCanvasDrag}
                            className={`absolute z-10 h-3 w-3 rounded-sm border border-black/60 bg-white shadow ${h.posClass}`}
                            style={{ cursor: h.cursor }}
                          />
                        ))}

                      {/* Floating Dimensions + Quick Crop Actions clamped inside stage */}
                      {cropDraft.w >= 8 && cropDraft.h >= 8 && (() => {
                        const nearTop = cropDraft.y / natSize.h <= 0.14;
                        const roomBelow = (cropDraft.y + cropDraft.h) / natSize.h < 0.86;
                        const vPosClass = !nearTop
                          ? "-top-10"
                          : roomBelow
                            ? "-bottom-10"
                            : "top-2";
                        const hPosClass =
                          cropDraft.x / natSize.w > 0.52
                            ? nearTop && !roomBelow
                              ? "right-2"
                              : "right-0"
                            : nearTop && !roomBelow
                              ? "left-2"
                              : "left-0";
                        return (
                          <div
                            onPointerDown={(e) => e.stopPropagation()}
                            className={`absolute z-20 flex items-center gap-1.5 whitespace-nowrap rounded-lg px-2 py-1 text-[11px] shadow-xl ${vPosClass} ${hPosClass}`}
                            style={{
                              background: "rgba(18,18,22,0.92)",
                              border: "1px solid var(--border-subtle)",
                            }}
                          >
                            <span className="tabular-nums text-slate-200">
                              {cropDraft.w} × {cropDraft.h}
                            </span>
                            <button
                              type="button"
                              onClick={applyCropSelection}
                              className="tf-btn flex items-center gap-1 rounded px-2 py-0.5 font-semibold"
                            >
                              <Check className="h-3 w-3" />
                              <span>{t("screenshotApplyCropBtn")}</span>
                            </button>
                            <button
                              type="button"
                              onClick={handleResetCrop}
                              className="hover-surface rounded px-1.5 py-0.5 text-slate-300"
                            >
                              {t("screenshotFullImageBtn")}
                            </button>
                          </div>
                        );
                      })()}
                    </div>
                  </div>
                )}
              </>
            )}

            {/* Inline Text Placement Input */}
            {textDraft && (
              <div
                className="absolute z-20"
                style={{
                  left: `${textDraft.pctX}%`,
                  top: `${textDraft.pctY}%`,
                }}
              >
                <input
                  ref={textInputRef}
                  value={textDraft.value}
                  onChange={(e) =>
                    setTextDraft((prev) => (prev ? { ...prev, value: e.target.value } : prev))
                  }
                  onKeyDown={(e) => {
                    if (isImeComposingEvent(e)) return;
                    if (e.key === "Enter") {
                      e.preventDefault();
                      commitTextDraft();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      setTextDraft(null);
                    }
                  }}
                  onBlur={() => commitTextDraft()}
                  placeholder={t("screenshotTextPlaceholder")}
                  className="min-w-[180px] rounded-lg border px-2.5 py-1 font-bold shadow-xl outline-none"
                  style={{
                    background: "rgba(15, 15, 20, 0.88)",
                    borderColor: "var(--accent)",
                    color,
                    fontSize:
                      SIZE_PRESETS[sizeIdx]?.id === "sm"
                        ? "13px"
                        : SIZE_PRESETS[sizeIdx]?.id === "lg"
                          ? "19px"
                          : "15px",
                  }}
                />
              </div>
            )}
          </div>
        )}

        {notice && (
          <div
            className="pointer-events-none absolute bottom-6 left-1/2 -translate-x-1/2 rounded-full px-4 py-1.5 text-xs font-medium shadow-lg"
            style={{ background: "rgba(16, 185, 129, 0.92)", color: "#052e16" }}
          >
            {notice}
          </div>
        )}
      </div>

      {/* Bottom Action Bar */}
      <div
        className="flex flex-wrap items-center justify-between gap-2.5 border-t px-4 py-3"
        style={{ background: "var(--bg-sidebar)", borderColor: "var(--border-subtle)" }}
      >
        <input
          value={caption}
          onChange={(e) => setCaption(e.target.value)}
          placeholder={t("screenshotCaptionPlaceholder")}
          className="min-w-[220px] flex-1 rounded-xl px-3.5 py-2 text-sm outline-none"
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
            color: "var(--text-primary)",
          }}
        />

        <div className="flex flex-wrap items-center gap-2">
          {hasValidCropDraft && (
            <button
              type="button"
              onClick={applyCropSelection}
              className="hover-surface flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
                color: "var(--accent)",
              }}
            >
              <Check className="h-4 w-4" />
              <span>{t("screenshotApplyCropBtn")}</span>
            </button>
          )}
          <button
            type="button"
            disabled={!imgEl || busyAction !== null}
            onClick={() => void handleCopy()}
            className="hover-surface flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-medium transition disabled:opacity-50"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            {busyAction === "copy" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Copy className="h-4 w-4" />
            )}
            <span>{t("screenshotCopyBtn")}</span>
          </button>
          <button
            type="button"
            disabled={!imgEl || busyAction !== null}
            onClick={() => void handleSave()}
            className="hover-surface flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-medium transition disabled:opacity-50"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            {busyAction === "save" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Download className="h-4 w-4" />
            )}
            <span>{t("screenshotSaveBtn")}</span>
          </button>
          <button
            type="button"
            disabled={!imgEl || busyAction !== null}
            onClick={() => void handleAttachOnly()}
            className="hover-surface flex items-center gap-1.5 rounded-xl px-3 py-2 text-xs font-medium transition disabled:opacity-50"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            {busyAction === "attach" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Paperclip className="h-4 w-4" />
            )}
            <span>{t("screenshotAttachBtn")}</span>
          </button>
          <button
            type="button"
            disabled={!imgEl || busyAction !== null}
            onClick={() => void handleSendNow()}
            className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold transition disabled:opacity-50"
          >
            {busyAction === "send" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Send className="h-4 w-4" />
            )}
            <span>{t("screenshotSendBtn")}</span>
          </button>
        </div>
      </div>

      {confirmDiscard && (
        <div
          className="fixed inset-0 z-[75] flex items-center justify-center bg-black/65 p-4 backdrop-blur-xs"
          onClick={() => setConfirmDiscard(false)}
        >
          <div
            role="alertdialog"
            aria-modal="true"
            aria-label={t("screenshotDiscardTitle")}
            onClick={(e) => e.stopPropagation()}
            className="w-full max-w-sm space-y-3 rounded-2xl p-5 shadow-2xl"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            <h3 className="text-[15px] font-semibold">{t("screenshotDiscardTitle")}</h3>
            <p className="text-xs leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              {t("screenshotDiscardDesc", {
                count: ops.length + (textDraft && textDraft.value.trim() ? 1 : 0),
              })}
            </p>
            <div className="flex items-center justify-end gap-2 pt-1">
              <button
                type="button"
                onClick={() => setConfirmDiscard(false)}
                className="hover-menu rounded-xl px-3.5 py-2 text-xs font-medium"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--text-primary)",
                }}
              >
                {t("screenshotKeepEditingBtn")}
              </button>
              <button
                type="button"
                onClick={() => {
                  setConfirmDiscard(false);
                  onClose();
                }}
                className="rounded-xl bg-rose-600 px-3.5 py-2 text-xs font-semibold text-white transition hover:bg-rose-500"
              >
                {t("screenshotDiscardConfirmBtn")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
