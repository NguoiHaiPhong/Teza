import { useCallback, useEffect, useState } from "react";
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

export interface LightboxItem {
  url: string;
  title: string;
}

interface Props {
  items: LightboxItem[];
  activeIndex: number | null;
  onIndexChange: (index: number) => void;
  onClose: () => void;
  onNotify?: (msg: string) => void;
}

async function imageUrlToPngBlob(url: string): Promise<Blob> {
  const dataUrl = await bridge.fetchImageDataUrl(url);
  const img = await new Promise<HTMLImageElement>((resolve, reject) => {
    const el = new Image();
    el.crossOrigin = "anonymous";
    el.onload = () => resolve(el);
    el.onerror = () => reject(new Error("Không tải được dữ liệu ảnh để sao chép"));
    el.src = dataUrl;
  });
  const canvas = document.createElement("canvas");
  canvas.width = img.naturalWidth || img.width;
  canvas.height = img.naturalHeight || img.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Không khởi tạo được bộ đệm sao chép ảnh");
  ctx.drawImage(img, 0, 0);
  return await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("Không chuyển đổi được ảnh PNG"))),
      "image/png",
    );
  });
}

export function MediaLightbox({
  items,
  activeIndex,
  onIndexChange,
  onClose,
  onNotify,
}: Props) {
  const [zoom, setZoom] = useState(1);
  const [rotation, setRotation] = useState(0);
  const [copied, setCopied] = useState(false);
  const [saving, setSaving] = useState(false);

  const valid = activeIndex !== null && activeIndex >= 0 && activeIndex < items.length;
  const current = valid ? items[activeIndex] : null;
  const hasPrev = valid && activeIndex > 0;
  const hasNext = valid && activeIndex < items.length - 1;

  const goPrev = useCallback(() => {
    if (hasPrev && activeIndex !== null) onIndexChange(activeIndex - 1);
  }, [hasPrev, activeIndex, onIndexChange]);

  const goNext = useCallback(() => {
    if (hasNext && activeIndex !== null) onIndexChange(activeIndex + 1);
  }, [hasNext, activeIndex, onIndexChange]);

  useEffect(() => {
    setZoom(1);
    setRotation(0);
    setCopied(false);
  }, [activeIndex, current?.url]);

  useEffect(() => {
    if (!valid) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft") goPrev();
      else if (e.key === "ArrowRight") goNext();
      else if (e.key === "+" || e.key === "=") setZoom((z) => Math.min(3, +(z + 0.25).toFixed(2)));
      else if (e.key === "-") setZoom((z) => Math.max(0.5, +(z - 0.25).toFixed(2)));
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [valid, onClose, goPrev, goNext]);

  if (!current) return null;

  const canExport = /^(https:\/\/|data:image\/|blob:)/i.test(current.url);

  const handleCopyImage = async () => {
    if (!canExport || copied) return;
    try {
      const blob = await imageUrlToPngBlob(current.url);
      await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
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
      await bridge.saveRemoteFile(current.url, current.title || "litechat-image.png");
    } catch (err) {
      onNotify?.(String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col bg-black/90 backdrop-blur-sm"
      onClick={onClose}
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
            onClick={() => setZoom((z) => Math.max(0.5, +(z - 0.25).toFixed(2)))}
            title="Thu nhỏ (-)"
            className="rounded-lg p-2 hover:bg-white/10"
          >
            <ZoomOut className="h-5 w-5" />
          </button>
          <span className="w-11 text-center text-xs font-mono text-white/75">
            {Math.round(zoom * 100)}%
          </span>
          <button
            type="button"
            onClick={() => setZoom((z) => Math.min(3, +(z + 0.25).toFixed(2)))}
            title="Phóng to (+)"
            className="rounded-lg p-2 hover:bg-white/10"
          >
            <ZoomIn className="h-5 w-5" />
          </button>
          <button
            type="button"
            onClick={() => setRotation((r) => (r + 90) % 360)}
            title="Xoay 90°"
            className="rounded-lg p-2 hover:bg-white/10"
          >
            <RotateCw className="h-5 w-5" />
          </button>
          {canExport && (
            <>
              <button
                type="button"
                onClick={() => void handleCopyImage()}
                title="Sao chép hình ảnh vào clipboard"
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
                title="Lưu ảnh về máy..."
                className="rounded-lg p-2 hover:bg-white/10 disabled:opacity-50"
              >
                <Download className="h-5 w-5" />
              </button>
            </>
          )}
          <button onClick={onClose} title="Đóng (Esc)" className="rounded-lg p-2 hover:bg-white/10">
            <X className="h-5 w-5" />
          </button>
        </div>
      </div>

      <div
        className="relative flex flex-1 items-center justify-center overflow-hidden p-6"
        onWheel={(e) => {
          e.stopPropagation();
          setZoom((z) =>
            e.deltaY < 0
              ? Math.min(3, +(z + 0.15).toFixed(2))
              : Math.max(0.5, +(z - 0.15).toFixed(2)),
          );
        }}
      >
        {hasPrev && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              goPrev();
            }}
            title="Ảnh trước (←)"
            className="absolute left-4 z-10 rounded-full bg-black/55 p-3 text-white transition hover:bg-black/80"
          >
            <ChevronLeft className="h-6 w-6" />
          </button>
        )}

        <img
          src={current.url}
          alt={current.title}
          referrerPolicy="no-referrer"
          onClick={(e) => {
            e.stopPropagation();
            setZoom((z) => (z > 1 ? 1 : 1.75));
          }}
          style={{
            transform: `scale(${zoom}) rotate(${rotation}deg)`,
          }}
          className={`max-h-full max-w-full rounded-lg shadow-2xl transition-transform duration-150 ${
            zoom > 1 ? "cursor-zoom-out" : "cursor-zoom-in"
          }`}
        />

        {hasNext && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              goNext();
            }}
            title="Ảnh tiếp theo (→)"
            className="absolute right-4 z-10 rounded-full bg-black/55 p-3 text-white transition hover:bg-black/80"
          >
            <ChevronRight className="h-6 w-6" />
          </button>
        )}
      </div>
    </div>
  );
}
