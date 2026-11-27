import type { AppUpdateInfo } from "../types.ts";

export const DISMISSED_UPDATE_VERSION_STORAGE_KEY = "teza.dismissedUpdateVersion";

export type UpdateVersionStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/**
 * Normalizes a release version string so comparisons and display tags stay consistent
 * regardless of surrounding whitespace or an optional leading "v"/"V".
 */
export function normalizeUpdateVersion(version: string | null | undefined): string {
  if (typeof version !== "string") return "";
  return version.trim().replace(/^v/i, "").trim();
}

/**
 * Formats a version string as "vX.Y.Z" (or empty string if blank/invalid).
 */
export function formatUpdateVersionTag(version: string | null | undefined): string {
  const normalized = normalizeUpdateVersion(version);
  return normalized ? `v${normalized}` : "";
}

/**
 * Returns true when the top update banner should be displayed for `availableUpdate`.
 * Dismissing a specific version (e.g. "0.7.1") hides the banner only for that version;
 * when a newer version (e.g. "0.7.2") arrives, the banner automatically reappears.
 */
export function isUpdateBannerVisible(
  availableUpdate: Pick<AppUpdateInfo, "version"> | null | undefined,
  dismissedVersion: string | null | undefined,
): boolean {
  if (!availableUpdate) return false;
  const availableNormalized = normalizeUpdateVersion(availableUpdate.version);
  if (!availableNormalized) return false;
  const dismissedNormalized = normalizeUpdateVersion(dismissedVersion);
  if (dismissedNormalized && dismissedNormalized === availableNormalized) {
    return false;
  }
  return true;
}

function getDefaultStorage(): UpdateVersionStorage | null {
  try {
    if (typeof globalThis !== "undefined" && "localStorage" in globalThis && globalThis.localStorage) {
      return globalThis.localStorage;
    }
  } catch {
    // Ignore restricted storage access
  }
  return null;
}

/**
 * Reads the persisted dismissed update version (normalized), or null if none is stored.
 */
export function loadDismissedUpdateVersion(
  storage: Pick<Storage, "getItem"> | null = getDefaultStorage(),
): string | null {
  if (!storage) return null;
  try {
    const raw = storage.getItem(DISMISSED_UPDATE_VERSION_STORAGE_KEY);
    const normalized = normalizeUpdateVersion(raw);
    return normalized || null;
  } catch {
    return null;
  }
}

/**
 * Persists a dismissed update version (normalized) or clears it when blank/null.
 * Returns the normalized version stored (or null if cleared).
 */
export function saveDismissedUpdateVersion(
  version: string | null | undefined,
  storage: Pick<Storage, "setItem" | "removeItem"> | null = getDefaultStorage(),
): string | null {
  const normalized = normalizeUpdateVersion(version);
  if (storage) {
    try {
      if (normalized) {
        storage.setItem(DISMISSED_UPDATE_VERSION_STORAGE_KEY, normalized);
      } else {
        storage.removeItem(DISMISSED_UPDATE_VERSION_STORAGE_KEY);
      }
    } catch {
      // Ignore storage quota or security errors
    }
  }
  return normalized || null;
}
