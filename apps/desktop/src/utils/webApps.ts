export type WebAppOpenMode = "embedded" | "external";

export interface WebAppItem {
  id: string;
  name: string;
  url: string;
  icon: string;
  openMode: WebAppOpenMode;
}

export interface WebAppsConfig {
  enabled: boolean;
  items: WebAppItem[];
}

export const WEBAPPS_STORAGE_KEY = "teza.webapps.v1";

export const DEFAULT_WEBAPPS_CONFIG: WebAppsConfig = {
  enabled: false,
  items: [
    {
      id: "wa-calendar",
      name: "Lịch Google",
      url: "https://calendar.google.com",
      icon: "calendar",
      openMode: "embedded",
    },
    {
      id: "wa-drive",
      name: "Google Drive",
      url: "https://drive.google.com",
      icon: "folder",
      openMode: "embedded",
    },
  ],
};

export function normalizeWebUrl(raw: string): string {
  const trimmed = raw.trim();
  if (!trimmed) return "";
  const withScheme = /^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//.test(trimmed)
    ? trimmed
    : `https://${trimmed}`;
  try {
    const u = new URL(withScheme);
    if (u.protocol !== "http:" && u.protocol !== "https:") return "";
    if (!u.hostname) return "";
    return u.toString();
  } catch {
    return "";
  }
}

export function extractWebAppDomain(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

export function loadWebAppsConfig(): WebAppsConfig {
  try {
    if (typeof localStorage === "undefined") {
      return {
        enabled: DEFAULT_WEBAPPS_CONFIG.enabled,
        items: DEFAULT_WEBAPPS_CONFIG.items.map((x) => ({ ...x })),
      };
    }
    const raw = localStorage.getItem(WEBAPPS_STORAGE_KEY);
    if (!raw) {
      return {
        enabled: DEFAULT_WEBAPPS_CONFIG.enabled,
        items: DEFAULT_WEBAPPS_CONFIG.items.map((x) => ({ ...x })),
      };
    }
    const parsed = JSON.parse(raw) as Partial<WebAppsConfig>;
    const items = Array.isArray(parsed.items)
      ? parsed.items
          .map((it): WebAppItem | null => {
            if (!it || typeof it !== "object") return null;
            const normUrl = typeof it.url === "string" ? normalizeWebUrl(it.url) : "";
            const name = typeof it.name === "string" ? it.name.trim() : "";
            if (!normUrl || !name) return null;
            return {
              id: typeof it.id === "string" && it.id.trim() ? it.id.trim() : `wa-${Date.now()}`,
              name,
              url: normUrl,
              icon: typeof it.icon === "string" && it.icon.trim() ? it.icon.trim() : "globe",
              openMode: it.openMode === "external" ? "external" : "embedded",
            };
          })
          .filter((x): x is WebAppItem => x !== null)
      : [];
    return {
      enabled: Boolean(parsed.enabled),
      items,
    };
  } catch {
    return {
      enabled: DEFAULT_WEBAPPS_CONFIG.enabled,
      items: DEFAULT_WEBAPPS_CONFIG.items.map((x) => ({ ...x })),
    };
  }
}

export function saveWebAppsConfig(cfg: WebAppsConfig): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(WEBAPPS_STORAGE_KEY, JSON.stringify(cfg));
    }
  } catch {
    // ignore storage quota errors
  }
}
