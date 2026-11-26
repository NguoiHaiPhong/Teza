import { useEffect, useState } from "react";
import { bridge, isTauri } from "../bridge.ts";

let relayMediaEnabled = false;
const listeners = new Set<(v: boolean) => void>();
const MAX_CACHE_ITEMS = 400;
const resolvedCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string>>();
let mediaProtocolToken: string | null = null;
let mediaProtocolTokenPromise: Promise<string | null> | null = null;

export type TezaMediaKind = "image" | "video" | "audio" | "file";

export interface BuildTezaMediaUrlOptions {
  threadId?: string;
  msgId?: string;
  platform?: string;
}

export interface ResolvedMediaOptions {
  kind?: TezaMediaKind;
  threadId?: string;
  msgId?: string;
}

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

export function isTezaMediaUrl(raw?: string | null): boolean {
  const trimmed = (raw ?? "").trim();
  return /^(?:https?:\/\/teza-media\.localhost\/|teza-media:\/\/)/i.test(trimmed);
}

function isWindowsPlatform(platformOverride?: string): boolean {
  const raw = (platformOverride ?? "").trim().toLowerCase();
  if (raw) {
    if (raw === "darwin" || raw.includes("mac") || raw.includes("linux")) {
      return false;
    }
    return raw.startsWith("win") || raw.includes("windows");
  }
  if (typeof navigator !== "undefined") {
    const uaDataPlatform = (
      navigator as Navigator & { userAgentData?: { platform?: string } }
    ).userAgentData?.platform;
    if (typeof uaDataPlatform === "string" && uaDataPlatform.trim().length > 0) {
      const p = uaDataPlatform.trim().toLowerCase();
      if (p === "darwin" || p.includes("mac") || p.includes("linux")) return false;
      return p.startsWith("win") || p.includes("windows");
    }
    if (typeof navigator.platform === "string" && navigator.platform.trim().length > 0) {
      const p = navigator.platform.trim().toLowerCase();
      if (p === "darwin" || p.includes("mac") || p.includes("linux")) return false;
      return p.startsWith("win") || p.includes("windows");
    }
    if (typeof navigator.userAgent === "string" && navigator.userAgent.length > 0) {
      return /windows/i.test(navigator.userAgent);
    }
  }
  const nodePlatform = (globalThis as { process?: { platform?: string } }).process?.platform;
  if (typeof nodePlatform === "string") {
    return nodePlatform === "win32";
  }
  return false;
}

export function getCachedMediaProtocolToken(): string | null {
  return mediaProtocolToken;
}

export async function getMediaProtocolToken(): Promise<string | null> {
  if (!isTauri()) return null;
  if (mediaProtocolToken) return mediaProtocolToken;
  if (mediaProtocolTokenPromise) return mediaProtocolTokenPromise;

  const pending = bridge
    .mediaGetProtocolToken()
    .then((token) => {
      const trimmed = typeof token === "string" ? token.trim() : "";
      if (trimmed) {
        mediaProtocolToken = trimmed;
        return trimmed;
      }
      mediaProtocolTokenPromise = null;
      return null;
    })
    .catch((err) => {
      console.warn(
        "[Teza media] Failed to fetch media protocol token:",
        err instanceof Error ? err.message : String(err),
      );
      mediaProtocolTokenPromise = null;
      return null;
    });

  mediaProtocolTokenPromise = pending;
  return pending;
}

/**
 * Convert an `http://` or `https://` media URL into a `teza-media` local vault
 * protocol URL with session token auth. Passes `data:` and `blob:` through
 * unchanged, avoids double-wrapping existing `teza-media` URLs, and rejects
 * non-http schemes (e.g. `file://`, `javascript:`, raw OS paths) for safety.
 */
export function buildTezaMediaUrl(
  rawUrl: string,
  kind: TezaMediaKind,
  token: string,
  opts?: BuildTezaMediaUrlOptions,
): string {
  const clean = normalizeMediaRequestUrl(rawUrl);
  if (!clean) return "";
  if (isDataUrl(clean) || isBlobUrl(clean) || isTezaMediaUrl(clean)) {
    return clean;
  }
  if (!isRemoteMediaUrl(clean)) {
    return "";
  }
  try {
    const parsed = new URL(clean);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return "";
    }
  } catch {
    return "";
  }
  const trimmedToken = token.trim();
  if (!trimmedToken) {
    return clean;
  }
  const params: string[] = [
    `url=${encodeURIComponent(clean)}`,
    `kind=${encodeURIComponent(kind)}`,
    `t=${encodeURIComponent(trimmedToken)}`,
  ];
  const tid = opts?.threadId?.trim();
  if (tid) {
    params.push(`tid=${encodeURIComponent(tid)}`);
  }
  const mid = opts?.msgId?.trim();
  if (mid) {
    params.push(`mid=${encodeURIComponent(mid)}`);
  }
  const base = isWindowsPlatform(opts?.platform)
    ? "http://teza-media.localhost/resolve"
    : "teza-media://localhost/resolve";
  return `${base}?${params.join("&")}`;
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
  opts?: ResolvedMediaOptions,
): {
  src: string;
  loading: boolean;
  failed: boolean;
  requestFallback: () => boolean;
} {
  const clean = normalizeMediaRequestUrl(rawUrl);
  const kind: TezaMediaKind = opts?.kind ?? "image";
  const threadId = opts?.threadId;
  const msgId = opts?.msgId;
  const isRemoteHttp = isTauri() && isRemoteMediaUrl(clean) && !isTezaMediaUrl(clean);
  const isPlainHttp = isTauri() && /^http:\/\//i.test(clean) && !isTezaMediaUrl(clean);

  const [relayActive, setRelayActive] = useState(relayMediaEnabled);
  const [fallbackForUrl, setFallbackForUrl] = useState<string | null>(null);
  const [protocolToken, setProtocolToken] = useState<string | null>(() =>
    isRemoteHttp ? getCachedMediaProtocolToken() : null,
  );
  const [protocolTokenReady, setProtocolTokenReady] = useState<boolean>(
    () => !isRemoteHttp || getCachedMediaProtocolToken() !== null,
  );
  const [fetchedEntry, setFetchedEntry] = useState<{ url: string; dataUrl: string } | null>(() => {
    const hit = clean ? resolvedCache.get(clean) : undefined;
    return hit ? { url: clean, dataUrl: hit } : null;
  });
  const [failedUrl, setFailedUrl] = useState<string | null>(null);

  const fallbackTried = fallbackForUrl === clean;
  const useProtocol = isRemoteHttp && Boolean(protocolToken) && !fallbackTried;
  const protocolUrl =
    useProtocol && protocolToken
      ? buildTezaMediaUrl(clean, kind, protocolToken, { threadId, msgId })
      : "";
  const shouldFetchViaRust =
    isRemoteHttp &&
    protocolTokenReady &&
    !useProtocol &&
    (relayActive || forceFetch || fallbackTried || isPlainHttp);

  useEffect(() => {
    listeners.add(setRelayActive);
    return () => {
      listeners.delete(setRelayActive);
    };
  }, []);

  useEffect(() => {
    if (!isRemoteHttp) {
      setProtocolTokenReady(true);
      return;
    }
    const cached = getCachedMediaProtocolToken();
    if (cached) {
      setProtocolToken(cached);
      setProtocolTokenReady(true);
      return;
    }
    let cancelled = false;
    setProtocolTokenReady(false);
    void getMediaProtocolToken().then((tok) => {
      if (!cancelled) {
        setProtocolToken(tok);
        setProtocolTokenReady(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [isRemoteHttp]);

  useEffect(() => {
    if (!shouldFetchViaRust) return;
    const hit = resolvedCache.get(clean);
    if (hit) {
      setFetchedEntry({ url: clean, dataUrl: hit });
      setFailedUrl((prev) => (prev === clean ? null : prev));
      return;
    }
    let cancelled = false;
    setFailedUrl((prev) => (prev === clean ? null : prev));
    void fetchCachedMediaDataUrl(clean)
      .then((dataUrl) => {
        if (!cancelled) {
          setFetchedEntry({ url: clean, dataUrl });
        }
      })
      .catch((err) => {
        console.warn(
          `[Teza media] Failed to fetch ${sanitizeUrlForLog(clean)}:`,
          err instanceof Error ? err.message : String(err),
        );
        if (!cancelled) {
          setFailedUrl(clean);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [clean, shouldFetchViaRust]);

  let src: string;
  let loading = false;
  let failed = false;

  if (!isRemoteHttp) {
    src = clean;
  } else if (!protocolTokenReady) {
    src = "";
    loading = true;
  } else if (useProtocol) {
    src = protocolUrl;
  } else if (!shouldFetchViaRust) {
    src = clean;
  } else {
    const cachedDataUrl =
      fetchedEntry?.url === clean ? fetchedEntry.dataUrl : resolvedCache.get(clean) || "";
    src = cachedDataUrl;
    failed = failedUrl === clean && !cachedDataUrl;
    loading = !cachedDataUrl && !failed;
  }

  return {
    src,
    loading,
    failed,
    requestFallback: () => {
      if (!isRemoteHttp || fallbackTried) {
        return false;
      }
      if (useProtocol) {
        if (kind === "image") {
          setFallbackForUrl(clean);
          return true;
        }
        return false;
      }
      if (!shouldFetchViaRust) {
        setFallbackForUrl(clean);
        return true;
      }
      return false;
    },
  };
}
