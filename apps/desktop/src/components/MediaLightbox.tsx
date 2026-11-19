import { useEffect, useState } from "react";
import { Download, X, ZoomIn, ZoomOut } from "lucide-react";

interface Props {
  url: string | null;
  title: string;
  onClose: () => void;
}

export function MediaLightbox({ url, title, onClose }: Props) {
  const [zoomed, setZoomed] = useState(false);

  useEffect(() => {
    setZoomed(false);
    if (!url) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [url, onClose]);

  if (!url) return null;

  const canDownload = /^(https:\/\/|data:image\/|blob:)/i.test(url);

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col bg-black/88 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="flex h-14 shrink-0 items-center justify-between px-5 text-white"
        onClick={(e) => e.stopPropagation()}
      >
        <span className="truncate text-sm font-medium">{title}</span>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setZoomed((z) => !z)}
            title={zoomed ? "Thu nhỏ" : "Phóng to"}
            className="rounded-lg p-2 hover:bg-white/10"
          >
            {zoomed ? <ZoomOut className="h-5 w-5" /> : <ZoomIn className="h-5 w-5" />}
          </button>
          {canDownload && (
            <a
              href={url}
              download={title || "litechat-image.png"}
              title="Tải xuống"
              className="rounded-lg p-2 hover:bg-white/10"
            >
              <Download className="h-5 w-5" />
            </a>
          )}
          <button onClick={onClose} title="Đóng (Esc)" className="rounded-lg p-2 hover:bg-white/10">
            <X className="h-5 w-5" />
          </button>
        </div>
      </div>

      <div className="flex flex-1 items-center justify-center overflow-auto p-6">
        <img
          src={url}
          alt={title}
          referrerPolicy="no-referrer"
          onClick={(e) => {
            e.stopPropagation();
            setZoomed((z) => !z);
          }}
          className={`rounded-lg shadow-2xl transition-transform duration-200 ${
            zoomed ? "max-h-none max-w-none cursor-zoom-out scale-125" : "max-h-full max-w-full cursor-zoom-in"
          }`}
        />
      </div>
    </div>
  );
}
