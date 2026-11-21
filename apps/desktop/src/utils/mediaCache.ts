import { useEffect, useState } from "react";
import { bridge, isTauri } from "../bridge.ts";

let relayMediaEnabled = false;
const listeners = new Set<(v: boolean) => void>();
const MAX_CACHE_ITEMS = 400;
const resolvedCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string>>();

export function isDataUrl(raw?: string | null): boolean {
  return /^data:/i.test((raw ?? "").trim());
}

export function isDataMediaUrl(raw?: string | null): boolean {
  return /^data:(?:image|audio|video)\//i.test((raw ?? "").trim());
}

export function isDataImageUrl(raw?: string | null): boolean {
  return /^data:image\//i.test((raw ?? "").trim());
}

export function isBlobUrl(raw?: string | null): boolean {
  return /^blob:/i.test((raw ?? "").trim());
}

/**
 * Normalize media/file URLs before rendering or forwarding to Rust:
 * trims whitespace, unescapes JSON `\/`, expands protocol-relative `//` to `https://`,
 * and canonicalizes `http://` / `https://` scheme casing while preserving `data:`/`blob:`.
 */
export function normalizeMediaRequestUrl(raw?: string | null): string {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) return "";
  if (isDataUrl(trimmed) || isBlobUrl(trimmed)) {
    return trimmed;
  }
  const unescaped = trimmed.replace(/\\\//g, "/");
  if (unescaped.startsWith("//")) {
    return `https://${unescaped.slice(2)}`;
  }
  if (/^https:\/\//i.test(unescaped)) {
    return `https://${unescaped.slice(8)}`;
  }
  if (/^http:\/\//i.test(unescaped)) {
    return `http://${unescaped.slice(7)}`;
  }
  return unescaped;
}

export function isRemoteMediaUrl(raw?: string | null): boolean {
  const normalized = normalizeMediaRequestUrl(raw);
  return /^https?:\/\//i.test(normalized);
}

export function sanitizeUrlForLog(raw?: string | null): string {
  const normalized = normalizeMediaRequestUrl(raw);
  if (!normalized) return "(empty)";
  if (isDataUrl(normalized)) return "data:...";
  if (isBlobUrl(normalized)) return "blob:...";
  try {
    const parsed = new URL(normalized);
    return `${parsed.protocol}//${parsed.host}`;
  } catch {
    return "(invalid-url)";
  }
}

export function extractLinkHost(raw?: string | null): string {
  const normalized = normalizeMediaRequestUrl(raw);
  if (!isRemoteMediaUrl(normalized)) return "";
  try {
    return new URL(normalized).host;
  } catch {
    return "";
  }
}

export function isExplicitFileMessageType(msgType?: string | null): boolean {
  const t = (msgType ?? "").trim().toLowerCase();
  return t.includes("file") || t === "19" || t === "share.file";
}

export function isExternalLinkMessageType(msgType?: string | null): boolean {
  const t = (msgType ?? "").trim().toLowerCase();
  return (
    t === "chat.link" ||
    t === "link" ||
    t.endsWith(".link") ||
    t === "chat.location" ||
    t === "chat.location.new" ||
    t === "location" ||
    t === "chat.recommended" ||
    t === "recommended" ||
    t === "chat.ecard" ||
    t === "ecard"
  );
}

export type NonMediaAttachmentAction = "save_file" | "file_no_url" | "open_external" | "none";

/**
 * Classify how a non-media (non-image/video/voice/sticker/rich-card) message attachment
 * should behave in MessageBubble: explicit file messages save locally, while link/location/
 * recommended/ecard or arbitrary non-file URLs open in the external browser instead of
 * invoking `saveRemoteFile`.
 */
export function classifyNonMediaAttachment(msg: {
  msg_type?: string | null;
  attachment_url?: string | null;
}): NonMediaAttachmentAction {
  const url = normalizeMediaRequestUrl(msg.attachment_url);
  if (isExternalLinkMessageType(msg.msg_type)) {
    return isRemoteMediaUrl(url) ? "open_external" : "none";
  }
  if (isExplicitFileMessageType(msg.msg_type)) {
    if (isRemoteMediaUrl(url) || isDataUrl(url)) {
      return "save_file";
    }
    return "file_no_url";
  }
  if (isRemoteMediaUrl(url)) {
    return "open_external";
  }
  return "none";
}

export type DrawerFileAction = "save_file" | "open_external" | "jump";

/**
 * Classify click action in the Right Info Drawer "Files" tab.
 */
export function classifyDrawerFileAction(msg: {
  msg_type?: string | null;
  attachment_url?: string | null;
}): DrawerFileAction {
  const url = normalizeMediaRequestUrl(msg.attachment_url);
  if (!url) return "jump";
  if (isExternalLinkMessageType(msg.msg_type)) {
    return isRemoteMediaUrl(url) ? "open_external" : "jump";
  }
  const t = (msg.msg_type ?? "").trim().toLowerCase();
  const isDownloadableType =
    isExplicitFileMessageType(msg.msg_type) || t.includes("voice") || t === "3";
  if (isDownloadableType) {
    return isRemoteMediaUrl(url) || isDataUrl(url) ? "save_file" : "jump";
  }
  if (isRemoteMediaUrl(url)) {
    return "open_external";
  }
  return "jump";
}

export function setRelayMediaRouting(enabled: boolean) {
  if (relayMediaEnabled === enabled) return;
  relayMediaEnabled = enabled;
  listeners.forEach((fn) => fn(enabled));
}

export function isRelayMediaRouting(): boolean {
  return relayMediaEnabled;
}

export async function fetchCachedMediaDataUrl(url: string): Promise<string> {
  const clean = normalizeMediaRequestUrl(url);
  if (!clean || isDataMediaUrl(clean) || isBlobUrl(clean)) {
    return clean;
  }
  const cached = resolvedCache.get(clean);
  if (cached) {
    resolvedCache.delete(clean);
    resolvedCache.set(clean, cached);
    return cached;
  }
  const existing = inFlight.get(clean);
  if (existing) return existing;

  const pending = bridge
    .fetchImageDataUrl(clean)
    .then((dataUrl) => {
      if (resolvedCache.size >= MAX_CACHE_ITEMS) {
        const oldest = resolvedCache.keys().next().value;
        if (oldest) resolvedCache.delete(oldest);
      }
      resolvedCache.set(clean, dataUrl);
      return dataUrl;
    })
    .finally(() => {
      inFlight.delete(clean);
    });

  inFlight.set(clean, pending);
  return pending;
}

export function useResolvedMediaUrl(
  rawUrl: string | null | undefined,
  forceFetch = false,
): {
  src: string;
  loading: boolean;
  failed: boolean;
  requestFallback: () => boolean;
} {
  const clean = normalizeMediaRequestUrl(rawUrl);
  const isRemoteHttp = isTauri() && isRemoteMediaUrl(clean);
  const isPlainHttp = isTauri() && /^http:\/\//i.test(clean);
  const [relayActive, setRelayActive] = useState(relayMediaEnabled);
  const [fallbackTried, setFallbackTried] = useState(false);
  const shouldFetchViaRust =
    isRemoteHttp && (relayActive || forceFetch || fallbackTried || isPlainHttp);
  const [resolved, setResolved] = useState<string>(() => {
    if (!shouldFetchViaRust) return clean;
    return resolvedCache.get(clean) || "";
  });
  const [loading, setLoading] = useState<boolean>(
    () => shouldFetchViaRust && !resolvedCache.has(clean),
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    listeners.add(setRelayActive);
    return () => {
      listeners.delete(setRelayActive);
    };
  }, []);

  useEffect(() => {
    setFallbackTried(false);
    setFailed(false);
  }, [clean]);

  useEffect(() => {
    if (!shouldFetchViaRust) {
      setResolved(clean);
      setLoading(false);
      setFailed(false);
      return;
    }
    const hit = resolvedCache.get(clean);
    if (hit) {
      setResolved(hit);
      setLoading(false);
      setFailed(false);
      return;
    }
    let cancelled = false;
    setResolved("");
    setLoading(true);
    setFailed(false);
    void fetchCachedMediaDataUrl(clean)
      .then((dataUrl) => {
        if (!cancelled) {
          setResolved(dataUrl);
          setLoading(false);
        }
      })
      .catch((err) => {
        console.warn(
          `[Teza media] Failed to fetch ${sanitizeUrlForLog(clean)}:`,
          err instanceof Error ? err.message : String(err),
        );
        if (!cancelled) {
          setResolved("");
          setLoading(false);
          setFailed(true);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [clean, shouldFetchViaRust]);

  return {
    src: resolved,
    loading,
    failed,
    requestFallback: () => {
      if (isRemoteHttp && !fallbackTried && !shouldFetchViaRust) {
        setFallbackTried(true);
        return true;
      }
      return false;
    },
  };
}
