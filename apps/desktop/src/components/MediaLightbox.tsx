import { useCallback, useEffect, useRef, useState } from "react";
import {
  Check,
  ChevronLeft,
  ChevronRight,
  Copy,
  Download,
  RotateCw,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import { copyImageReliable } from "../utils/clipboard";
import {
  clampLightboxPan,
  clampLightboxZoom,
  LIGHTBOX_MAX_ZOOM,
  LIGHTBOX_MIN_ZOOM,
  zoomAroundPoint,
} from "../utils/lightboxPan";
import {
  isBlobUrl,
  isDataImageUrl,
  isRemoteMediaUrl,
  normalizeMediaRequestUrl,
} from "../utils/mediaCache";
import { isImeComposingEvent } from "../utils/searchNormalize";
import { RelayImage } from "./RelayImage";

async function blobUrlToDataUrl(blobUrl: string): Promise<string> {
  try {
    const res = await fetch(blobUrl);
    if (!res.ok) {
      throw new Error(`HTTP ${res.status}`);
    }
    const blob = await res.blob();
    return await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        if (typeof reader.result === "string" && reader.result.length > 0) {
          resolve(reader.result);
        } else {
          reject(new Error("Không chuyển đổi được dữ liệu ảnh cục bộ"));
        }
      };
      reader.onerror = () => reject(new Error("Không đọc được dữ liệu ảnh cục bộ"));
      reader.readAsDataURL(blob);
    });
  } catch {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const el = new Image();
      el.onload = () => resolve(el);
      el.onerror = () => reject(new Error("Không đọc được ảnh cục bộ để lưu"));
      el.src = blobUrl;
    });
    const w = img.naturalWidth || img.width;
    const h = img.naturalHeight || img.height;
    if (!w || !h) {
      throw new Error("Ảnh cục bộ không có kích thước hợp lệ");
    }
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      throw new Error("Không khởi tạo được bộ đệm ảnh");
    }
    ctx.drawImage(img, 0, 0);
    return canvas.toDataURL("image/png");
  }
}

export interface LightboxItem {
  url: string;
  title: string;
  threadId?: string;
  msgId?: string;
}

interface Props {
  items: LightboxItem[];
  activeIndex: number | null;
  isLocked?: boolean;
  onIndexChange: (index: number) => void;
  onClose: () => void;
  onNotify?: (msg: string) => void;
}

export function MediaLightbox({
  items,
  activeIndex,
  isLocked = false,
  onIndexChange,
  onClose,
  onNotify,
}: Props) {
  const { t } = useI18n();
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [dragging, setDragging] = useState(false);
  const [rotation, setRotation] = useState(0);
  const [copied, setCopied] = useState(false);
  const [saving, setSaving] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  const stageRef = useRef<HTMLDivElement | null>(null);
  const zoomRef = useRef(zoom);
  const panRef = useRef(pan);
  const rotationRef = useRef(rotation);
  const suppressClickRef = useRef(false);
  const dragRef = useRef({
    active: false,
    pointerId: -1,
    startX: 0,
    startY: 0,
    startPanX: 0,
    startPanY: 0,
    moved: false,
  });

  zoomRef.current = zoom;
  panRef.current = pan;
  rotationRef.current = rotation;

  const valid = activeIndex !== null && activeIndex >= 0 && activeIndex < items.length;
  const current = valid ? items[activeIndex] : null;
  const hasPrev = valid && activeIndex > 0;
  const hasNext = valid && activeIndex < items.length - 1;

  const getMetrics = useCallback(() => {
    const stage = stageRef.current;
    const img = stage?.querySelector("img") ?? null;
    return {
      stageW: stage?.clientWidth ?? 0,
      stageH: stage?.clientHeight ?? 0,
      imgW: img?.offsetWidth ?? 0,
      imgH: img?.offsetHeight ?? 0,
    };
  }, []);

  const applyZoom = useCallback(
    (targetZoom: number, clientX?: number, clientY?: number) => {
      const oldZoom = zoomRef.current;
      const nextZoom = clampLightboxZoom(targetZoom);
      if (nextZoom <= 1) {
        setZoom(nextZoom);
        setPan({ x: 0, y: 0 });
        return;
      }
      const { stageW, stageH, imgW, imgH } = getMetrics();
      let anchorPan = panRef.current;
      if (typeof clientX === "number" && typeof clientY === "number" && stageRef.current) {
        const rect = stageRef.current.getBoundingClientRect();
        const cursorOffsetX = clientX - (rect.left + rect.width / 2);
        const cursorOffsetY = clientY - (rect.top + rect.height / 2);
        anchorPan = zoomAroundPoint(panRef.current, oldZoom, nextZoom, cursorOffsetX, cursorOffsetY);
      }
      const clamped = clampLightboxPan(
        anchorPan.x,
        anchorPan.y,
        nextZoom,
        stageW,
        stageH,
        imgW,
        imgH,
        rotationRef.current,
      );
      setZoom(nextZoom);
      setPan(clamped);
    },
    [getMetrics],
  );

  const goPrev = useCallback(() => {
    if (hasPrev && activeIndex !== null) onIndexChange(activeIndex - 1);
  }, [hasPrev, activeIndex, onIndexChange]);

  const goNext = useCallback(() => {
    if (hasNext && activeIndex !== null) onIndexChange(activeIndex + 1);
  }, [hasNext, activeIndex, onIndexChange]);

  useEffect(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
    setDragging(false);
    dragRef.current.active = false;
    suppressClickRef.current = false;
    setRotation(0);
    setCopied(false);
    setContextMenu(null);
  }, [activeIndex, current?.url]);

  useEffect(() => {
    if (!contextMenu) return;
    const dismiss = () => setContextMenu(null);
    window.addEventListener("click", dismiss);
    window.addEventListener("resize", dismiss);
    window.addEventListener("blur", dismiss);
    return () => {
      window.removeEventListener("click", dismiss);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("blur", dismiss);
    };
  }, [contextMenu]);

  useEffect(() => {
    if (zoomRef.current <= 1) return;
    const { stageW, stageH, imgW, imgH } = getMetrics();
    setPan((prev) =>
      clampLightboxPan(prev.x, prev.y, zoomRef.current, stageW, stageH, imgW, imgH, rotation),
    );
  }, [rotation, getMetrics]);

  useEffect(() => {
    if (!dragging) return;
    const onPointerMove = (e: PointerEvent) => {
      const drag = dragRef.current;
      if (!drag.active || e.pointerId !== drag.pointerId) return;
      const dx = e.clientX - drag.startX;
      const dy = e.clientY - drag.startY;
      if (!drag.moved && Math.hypot(dx, dy) > 4) {
        drag.moved = true;
      }
      const { stageW, stageH, imgW, imgH } = getMetrics();
      setPan(
        clampLightboxPan(
          drag.startPanX + dx,
          drag.startPanY + dy,
          zoomRef.current,
          stageW,
          stageH,
          imgW,
          imgH,
          rotationRef.current,
        ),
      );
    };
    const finishDrag = (pointerId?: number) => {
      const drag = dragRef.current;
      if (!drag.active) return;
      if (typeof pointerId === "number" && pointerId !== drag.pointerId) return;
      if (drag.moved) {
        suppressClickRef.current = true;
        window.setTimeout(() => {
          suppressClickRef.current = false;
        }, 50);
      }
      drag.active = false;
      setDragging(false);
    };
    const onPointerUp = (e: PointerEvent) => finishDrag(e.pointerId);
    const onBlur = () => finishDrag();

    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
      window.removeEventListener("blur", onBlur);
    };
  }, [dragging, getMetrics]);

  useEffect(() => {
    if (!valid || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || isImeComposingEvent(e)) return;
      if (document.querySelector("[data-teza-download-dialog]")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (contextMenu) {
          setContextMenu(null);
        } else {
          onClose();
        }
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        e.stopPropagation();
        goPrev();
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        e.stopPropagation();
        goNext();
      } else if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        e.stopPropagation();
        applyZoom(zoomRef.current + 0.25);
      } else if (e.key === "-") {
        e.preventDefault();
        e.stopPropagation();
        applyZoom(zoomRef.current - 0.25);
      } else if (e.key === "0") {
        e.preventDefault();
        e.stopPropagation();
        applyZoom(1);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [valid, isLocked, contextMenu, onClose, goPrev, goNext, applyZoom]);

  if (!current) return null;

  const normalizedUrl = normalizeMediaRequestUrl(current.url);
  const canExport =
    isRemoteMediaUrl(normalizedUrl) ||
    isDataImageUrl(normalizedUrl) ||
    isBlobUrl(normalizedUrl);

  const handleCopyImage = async () => {
    if (!canExport || copied) return;
    try {
      await copyImageReliable(normalizedUrl);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch (err) {
      onNotify?.(String(err));
    }
  };

  const handleSaveAs = async () => {
    if (!canExport || saving) return;
    setSaving(true);
    try {
      const targetUrl = isBlobUrl(normalizedUrl)
        ? await blobUrlToDataUrl(normalizedUrl)
        : normalizedUrl;
      await bridge.saveRemoteFile(targetUrl, current.title || "teza-image", undefined, {
        threadId: current.threadId,
        msgId: current.msgId,
      });
    } catch (err) {
      onNotify?.(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      data-teza-lightbox="true"
      className="fixed inset-0 z-[65] flex flex-col bg-black/90 backdrop-blur-sm"
      onClick={() => {
        if (contextMenu) {
          setContextMenu(null);
          return;
        }
        if (suppressClickRef.current) {
          suppressClickRef.current = false;
          return;
        }
        onClose();
      }}
    >
      <div
        className="flex h-14 shrink-0 items-center justify-between gap-3 px-5 text-white"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex min-w-0 items-center gap-2.5">
          {items.length > 1 && activeIndex !== null && (
            <span className="shrink-0 rounded-full bg-white/10 px-2.5 py-0.5 text-xs font-medium text-white/85">
              {activeIndex + 1} / {items.length}
            </span>
          )}
          <span className="truncate text-sm font-medium">{current.title}</span>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <button
            type="button"
            disabled={zoom <= LIGHTBOX_MIN_ZOOM}
            onClick={() => applyZoom(zoom - 0.25)}
            title={t("zoomOutTitle")}
            className="rounded-lg p-2 hover:bg-white/10 disabled:opacity-40"
          >
            <ZoomOut className="h-5 w-5" />
          </button>
          <button
            type="button"
            onClick={() => applyZoom(1)}
            title="100%"
            className="w-12 rounded-md py-1 text-center text-xs font-mono text-white/75 hover:bg-white/10"
          >
            {Math.round(zoom * 100)}%
          </button>
          <button
            type="button"
            disabled={zoom >= LIGHTBOX_MAX_ZOOM}
            onClick={() => applyZoom(zoom + 0.25)}
            title={t("zoomInTitle")}
            className="rounded-lg p-2 hover:bg-white/10 disabled:opacity-40"
          >
            <ZoomIn className="h-5 w-5" />
          </button>
          <button
            type="button"
            onClick={() => setRotation((r) => (r + 90) % 360)}
            title={t("rotateTitle")}
            className="rounded-lg p-2 hover:bg-white/10"
          >
            <RotateCw className="h-5 w-5" />
          </button>
          {canExport && (
            <>
              <button
                type="button"
                onClick={() => void handleCopyImage()}
                title={t("copyImageClipboardTitle")}
                className="rounded-lg p-2 hover:bg-white/10"
              >
                {copied ? (
                  <Check className="h-5 w-5 text-emerald-400" />
                ) : (
                  <Copy className="h-5 w-5" />
                )}
              </button>
              <button
                type="button"
                disabled={saving}
                onClick={() => void handleSaveAs()}
                title={t("saveImageLocalTitle")}
                className="rounded-lg p-2 hover:bg-white/10 disabled:opacity-50"
              >
                <Download className="h-5 w-5" />
              </button>
            </>
          )}
          <button onClick={onClose} title={t("close")} className="rounded-lg p-2 hover:bg-white/10">
            <X className="h-5 w-5" />
          </button>
        </div>
      </div>

      <div
        ref={stageRef}
        className="relative flex flex-1 items-center justify-center overflow-hidden p-6 select-none"
        onWheel={(e) => {
          e.stopPropagation();
          applyZoom(e.deltaY < 0 ? zoom + 0.2 : zoom - 0.2, e.clientX, e.clientY);
        }}
        onPointerDown={(e) => {
          if (zoom <= 1 || e.button !== 0) return;
          if ((e.target as HTMLElement | null)?.closest("button")) return;
          e.preventDefault();
          suppressClickRef.current = false;
          dragRef.current = {
            active: true,
            pointerId: e.pointerId,
            startX: e.clientX,
            startY: e.clientY,
            startPanX: panRef.current.x,
            startPanY: panRef.current.y,
            moved: false,
          };
          setDragging(true);
        }}
      >
        {hasPrev && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              goPrev();
            }}
            title={t("prevPhotoTitle")}
            className="absolute left-4 z-10 rounded-full bg-black/55 p-3 text-white transition hover:bg-black/80"
          >
            <ChevronLeft className="h-6 w-6" />
          </button>
        )}

        <RelayImage
          src={normalizedUrl}
          threadId={current.threadId}
          msgId={current.msgId}
          alt={current.title}
          referrerPolicy="no-referrer"
          draggable={false}
          onContextMenu={(e) => {
            if (!canExport) return;
            e.preventDefault();
            e.stopPropagation();
            setContextMenu({
              x: Math.max(8, Math.min(e.clientX, window.innerWidth - 210)),
              y: Math.max(8, Math.min(e.clientY, window.innerHeight - 108)),
            });
          }}
          onClick={(e) => {
            e.stopPropagation();
            if (contextMenu) {
              setContextMenu(null);
              return;
            }
            if (suppressClickRef.current) {
              suppressClickRef.current = false;
              return;
            }
            if (zoom > 1) {
              applyZoom(1);
            } else {
              applyZoom(2, e.clientX, e.clientY);
            }
          }}
          style={{
            transform: `translate3d(${pan.x}px, ${pan.y}px, 0) scale(${zoom}) rotate(${rotation}deg)`,
          }}
          className={`max-h-full max-w-full rounded-lg shadow-2xl select-none ${
            dragging ? "" : "transition-transform duration-150"
          } ${zoom > 1 ? (dragging ? "cursor-grabbing" : "cursor-grab") : "cursor-zoom-in"}`}
        />

        {contextMenu && canExport && (
          <div
            data-teza-floating-popup="true"
            onClick={(e) => e.stopPropagation()}
            onContextMenu={(e) => e.preventDefault()}
            className="fixed z-[70] w-48 rounded-xl py-1 shadow-2xl"
            style={{
              left: `${contextMenu.x}px`,
              top: `${contextMenu.y}px`,
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
          >
            <button
              type="button"
              disabled={saving}
              onClick={() => {
                setContextMenu(null);
                void handleSaveAs();
              }}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] disabled:opacity-50"
            >
              <Download className="h-3.5 w-3.5" />
              <span>{t("saveImageMenu")}</span>
            </button>
            <button
              type="button"
              onClick={() => {
                setContextMenu(null);
                void handleCopyImage();
              }}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
            >
              <Copy className="h-3.5 w-3.5" />
              <span>{t("copyImage")}</span>
            </button>
          </div>
        )}

        {hasNext && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              goNext();
            }}
            title={t("nextPhotoTitle")}
            className="absolute right-4 z-10 rounded-full bg-black/55 p-3 text-white transition hover:bg-black/80"
          >
            <ChevronRight className="h-6 w-6" />
          </button>
        )}
      </div>
    </div>
  );
}
