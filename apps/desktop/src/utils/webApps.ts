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

const MAX_WEB_URL_LENGTH = 4096;

function normalizeHostForCheck(hostname: string): string {
  return hostname
    .trim()
    .toLowerCase()
    .replace(/^\[|\]$/g, "")
    .replace(/\.+$/, "");
}

function isForbiddenInternalHost(hostname: string): boolean {
  const host = normalizeHostForCheck(hostname);
  return (
    host === "tauri.localhost" ||
    host.endsWith(".tauri.localhost") ||
    host === "ipc.localhost" ||
    host.endsWith(".ipc.localhost") ||
    host === "asset.localhost" ||
    host.endsWith(".asset.localhost")
  );
}

function isPrivateOrLocalIpv4Octets(a: number, b: number): boolean {
  return (
    a === 10 ||
    a === 127 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 169 && b === 254) ||
    (a === 100 && b >= 64 && b <= 127)
  );
}

function parseIpv4Octets(text: string): [number, number, number, number] | null {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(text);
  if (!m) return null;
  const octets = m.slice(1).map(Number);
  if (!octets.every((n) => Number.isInteger(n) && n >= 0 && n <= 255)) {
    return null;
  }
  return [octets[0], octets[1], octets[2], octets[3]];
}

function parseIpv6Segments(rawHost: string): number[] | null {
  let addr = rawHost.trim().toLowerCase().replace(/^\[|\]$/g, "");
  if (!addr.includes(":")) return null;

  // Handle embedded dotted-quad IPv4 suffix (e.g. ::ffff:10.7.12.233)
  const lastColon = addr.lastIndexOf(":");
  if (lastColon !== -1 && addr.slice(lastColon + 1).includes(".")) {
    const octets = parseIpv4Octets(addr.slice(lastColon + 1));
    if (!octets) return null;
    const hi = ((octets[0] << 8) | octets[1]).toString(16);
    const lo = ((octets[2] << 8) | octets[3]).toString(16);
    addr = `${addr.slice(0, lastColon + 1)}${hi}:${lo}`;
  }

  const halves = addr.split("::");
  if (halves.length > 2) return null;

  const parsePart = (part: string): number[] | null => {
    if (!part) return [];
    const groups = part.split(":");
    const out: number[] = [];
    for (const g of groups) {
      if (!/^[0-9a-f]{1,4}$/.test(g)) return null;
      out.push(parseInt(g, 16));
    }
    return out;
  };

  const left = parsePart(halves[0]);
  if (!left) return null;

  if (halves.length === 1) {
    return left.length === 8 ? left : null;
  }

  const right = parsePart(halves[1]);
  if (!right) return null;
  const missing = 8 - (left.length + right.length);
  if (missing < 1) return null;

  return [...left, ...new Array<number>(missing).fill(0), ...right];
}

function isLocalOrPrivateHost(hostname: string): boolean {
  const lower = normalizeHostForCheck(hostname);
  if (
    lower === "localhost" ||
    lower.endsWith(".localhost") ||
    lower.endsWith(".local") ||
    lower.endsWith(".lan") ||
    lower.endsWith(".internal") ||
    lower === "::1"
  ) {
    return true;
  }
  const ipv4 = parseIpv4Octets(lower);
  if (ipv4) {
    return isPrivateOrLocalIpv4Octets(ipv4[0], ipv4[1]);
  }
  const segs = parseIpv6Segments(lower);
  if (segs) {
    const isLoopback =
      segs[0] === 0 &&
      segs[1] === 0 &&
      segs[2] === 0 &&
      segs[3] === 0 &&
      segs[4] === 0 &&
      segs[5] === 0 &&
      segs[6] === 0 &&
      segs[7] === 1;
    if (isLoopback) return true;

    const isIpv4Mapped =
      segs[0] === 0 &&
      segs[1] === 0 &&
      segs[2] === 0 &&
      segs[3] === 0 &&
      segs[4] === 0 &&
      segs[5] === 0xffff;
    if (isIpv4Mapped) {
      const a = (segs[6] >> 8) & 0xff;
      const b = segs[6] & 0xff;
      return isPrivateOrLocalIpv4Octets(a, b);
    }

    return (segs[0] & 0xfe00) === 0xfc00 || (segs[0] & 0xffc0) === 0xfe80;
  }
  return false;
}

function hasSchemeSeparatorBeforeDelim(input: string): boolean {
  const sepIdx = input.indexOf("://");
  if (sepIdx <= 0) return false;
  const firstDelim = input.search(/[/?#]/);
  // The "/" of "://" must be the earliest delimiter (so `://` is not inside path/query/hash)
  return firstDelim === sepIdx + 1;
}

function authorityHasAtSign(urlWithScheme: string): boolean {
  const sepIdx = urlWithScheme.indexOf("://");
  if (sepIdx === -1) return false;
  const rest = urlWithScheme.slice(sepIdx + 3);
  const delimIdx = rest.search(/[/?#]/);
  const authority = delimIdx === -1 ? rest : rest.slice(0, delimIdx);
  return authority.includes("@");
}

export function normalizeWebUrl(raw: string): string {
  if (/[\u0000-\u001f\u007f]/.test(raw)) return "";
  const trimmed = raw.trim();
  if (!trimmed || trimmed.length > MAX_WEB_URL_LENGTH) return "";

  let withScheme: string;
  if (hasSchemeSeparatorBeforeDelim(trimmed)) {
    withScheme = trimmed;
  } else if (/^[a-zA-Z][a-zA-Z\d+\-.]*:(?!\d+(?:[/?#]|$))/.test(trimmed)) {
    return "";
  } else {
    let preferHttp = false;
    try {
      const probe = new URL(`http://${trimmed}`);
      preferHttp = isLocalOrPrivateHost(probe.hostname);
    } catch {
      return "";
    }
    withScheme = `${preferHttp ? "http" : "https"}://${trimmed}`;
  }

  if (authorityHasAtSign(withScheme)) return "";

  try {
    const u = new URL(withScheme);
    if (u.protocol !== "http:" && u.protocol !== "https:") return "";
    if (!u.hostname) return "";
    if (u.username || u.password) return "";
    if (isForbiddenInternalHost(u.hostname)) return "";
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
    const now = Date.now();
    const items = Array.isArray(parsed.items)
      ? parsed.items
          .map((it, idx): WebAppItem | null => {
            if (!it || typeof it !== "object") return null;
            const normUrl = typeof it.url === "string" ? normalizeWebUrl(it.url) : "";
            const name = typeof it.name === "string" ? it.name.trim() : "";
            if (!normUrl || !name) return null;
            return {
              id:
                typeof it.id === "string" && it.id.trim()
                  ? it.id.trim()
                  : `wa-${now}-${idx}`,
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
