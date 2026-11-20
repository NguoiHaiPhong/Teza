import { useEffect, useState } from "react";
import { bridge, isTauri } from "../bridge";

let relayMediaEnabled = false;
const listeners = new Set<(v: boolean) => void>();
const MAX_CACHE_ITEMS = 400;
const resolvedCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string>>();

export function setRelayMediaRouting(enabled: boolean) {
  if (relayMediaEnabled === enabled) return;
  relayMediaEnabled = enabled;
  listeners.forEach((fn) => fn(enabled));
}

export function isRelayMediaRouting(): boolean {
  return relayMediaEnabled;
}

export async function fetchCachedMediaDataUrl(url: string): Promise<string> {
  const cached = resolvedCache.get(url);
  if (cached) {
    resolvedCache.delete(url);
    resolvedCache.set(url, cached);
    return cached;
  }
  const existing = inFlight.get(url);
  if (existing) return existing;

  const pending = bridge
    .fetchImageDataUrl(url)
    .then((dataUrl) => {
      if (resolvedCache.size >= MAX_CACHE_ITEMS) {
        const oldest = resolvedCache.keys().next().value;
        if (oldest) resolvedCache.delete(oldest);
      }
      resolvedCache.set(url, dataUrl);
      return dataUrl;
    })
    .finally(() => {
      inFlight.delete(url);
    });

  inFlight.set(url, pending);
  return pending;
}

export function useResolvedMediaUrl(
  rawUrl: string | null | undefined,
  forceFetch = false,
): {
  src: string;
  loading: boolean;
  failed: boolean;
  requestFallback: () => void;
} {
  const clean = rawUrl?.trim() || "";
  const isRemoteHttps = isTauri() && /^https:\/\//i.test(clean);
  const [relayActive, setRelayActive] = useState(relayMediaEnabled);
  const [fallbackTried, setFallbackTried] = useState(false);
  const shouldFetchViaRust = isRemoteHttps && (relayActive || forceFetch || fallbackTried);
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
      .catch(() => {
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
      if (isRemoteHttps && !fallbackTried) {
        setFallbackTried(true);
      }
    },
  };
}
