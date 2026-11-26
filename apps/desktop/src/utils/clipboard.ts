import { invoke } from "@tauri-apps/api/core";
import { bridge, isTauri } from "../bridge";
import { isBlobUrl, isDataUrl, normalizeMediaRequestUrl } from "./mediaCache";

const BASE64_CHUNK_SIZE = 0x8000;

export function uint8ArrayToBase64(bytes: Uint8Array): string {
  if (bytes.byteLength === 0) return "";
  const chunks: string[] = [];
  for (let offset = 0; offset < bytes.byteLength; offset += BASE64_CHUNK_SIZE) {
    const slice = bytes.subarray(offset, Math.min(offset + BASE64_CHUNK_SIZE, bytes.byteLength));
    chunks.push(String.fromCharCode(...slice));
  }
  return btoa(chunks.join(""));
}

function execCommandCopyText(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  ta.style.left = "-9999px";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  } finally {
    ta.remove();
  }
  return ok;
}

export async function copyTextReliable(text: string): Promise<void> {
  if (isTauri()) {
    try {
      await invoke("clipboard_copy_text", { text });
      return;
    } catch {
      // Fallback below on non-Windows platforms
    }
  }
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Fallback below
    }
  }
  if (!execCommandCopyText(text)) {
    throw new Error("Không sao chép được văn bản vào clipboard");
  }
}

async function decodeUrlToCanvas(url: string): Promise<HTMLCanvasElement> {
  const normalized = normalizeMediaRequestUrl(url);
  if (!normalized) {
    throw new Error("Không có địa chỉ ảnh hợp lệ để sao chép");
  }
  const safeSrc =
    isDataUrl(normalized) || isBlobUrl(normalized)
      ? normalized
      : await bridge.fetchImageDataUrl(normalized);

  const img = await new Promise<HTMLImageElement>((resolve, reject) => {
    const el = new Image();
    if (/^https?:/i.test(safeSrc)) {
      el.crossOrigin = "anonymous";
    }
    el.onload = () => resolve(el);
    el.onerror = () => reject(new Error("Không tải được dữ liệu ảnh để sao chép"));
    el.src = safeSrc;
  });

  const w = img.naturalWidth || img.width;
  const h = img.naturalHeight || img.height;
  if (!w || !h) {
    throw new Error("Ảnh không có kích thước hợp lệ");
  }
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    throw new Error("Không khởi tạo được bộ đệm sao chép ảnh");
  }
  ctx.drawImage(img, 0, 0);
  return canvas;
}

function canvasToPngBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("Không chuyển đổi được ảnh PNG"))),
      "image/png",
    );
  });
}

export async function copyImageReliable(url: string): Promise<void> {
  if (!isTauri() && navigator.clipboard?.write && typeof ClipboardItem !== "undefined") {
    // Pass Promise<Blob> synchronously so browser preserves transient user activation.
    const blobPromise = decodeUrlToCanvas(url).then(canvasToPngBlob);
    await navigator.clipboard.write([new ClipboardItem({ "image/png": blobPromise })]);
    return;
  }

  const canvas = await decodeUrlToCanvas(url);
  if (isTauri()) {
    try {
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Thiếu context canvas");
      const imageData = ctx.getImageData(0, 0, canvas.width, canvas.height);
      const rgba = new Uint8Array(
        imageData.data.buffer,
        imageData.data.byteOffset,
        imageData.data.byteLength,
      );
      await invoke("clipboard_copy_rgba_image_b64", {
        width: canvas.width,
        height: canvas.height,
        rgbaBase64: uint8ArrayToBase64(rgba),
      });
      return;
    } catch (nativeErr) {
      if (navigator.clipboard?.write && typeof ClipboardItem !== "undefined") {
        const blob = await canvasToPngBlob(canvas);
        await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
        return;
      }
      throw nativeErr;
    }
  }

  const blob = await canvasToPngBlob(canvas);
  await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
}
