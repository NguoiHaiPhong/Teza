import test from "node:test";
import assert from "node:assert/strict";
import {
  extractWebAppDomain,
  loadWebAppsConfig,
  normalizeWebUrl,
  WEBAPPS_STORAGE_KEY,
} from "../src/utils/webApps.ts";

test("normalizeWebUrl enforces http/https and defaults LAN hosts to http", () => {
  assert.equal(normalizeWebUrl("notion.so"), "https://notion.so/");
  assert.equal(normalizeWebUrl("https://app.slack.com/client"), "https://app.slack.com/client");
  assert.equal(normalizeWebUrl("http://localhost:3000/dashboard"), "http://localhost:3000/dashboard");
  assert.equal(normalizeWebUrl("localhost:3000/dashboard"), "http://localhost:3000/dashboard");
  assert.equal(normalizeWebUrl("http://10.7.12.233/index.html"), "http://10.7.12.233/index.html");
  assert.equal(normalizeWebUrl("10.7.12.233/index.html"), "http://10.7.12.233/index.html");
  assert.equal(
    normalizeWebUrl("10.7.12.233:5093/summary_error.html"),
    "http://10.7.12.233:5093/summary_error.html",
  );
  assert.equal(normalizeWebUrl("https://10.7.12.233/secure"), "https://10.7.12.233/secure");
  assert.equal(normalizeWebUrl("192.168.1.20:8080"), "http://192.168.1.20:8080/");
  assert.equal(normalizeWebUrl("172.16.5.10/status"), "http://172.16.5.10/status");
  assert.equal(normalizeWebUrl("100.95.41.126:9000"), "http://100.95.41.126:9000/");
  assert.equal(normalizeWebUrl("nas.local/files"), "http://nas.local/files");
  assert.equal(normalizeWebUrl("[::1]:8080/api"), "http://[::1]:8080/api");
  assert.equal(normalizeWebUrl("javascript:alert(1)"), "");
  assert.equal(normalizeWebUrl("data:text/html,hi"), "");
  assert.equal(normalizeWebUrl("file:///etc/passwd"), "");
  assert.equal(normalizeWebUrl("not a url !!"), "");
});

test("normalizeWebUrl enforces hardened security rules and URL edge cases", () => {
  // Control chars rejected
  assert.equal(normalizeWebUrl("https://example.com/\n"), "");
  assert.equal(normalizeWebUrl("https://ex\tample.com"), "");
  assert.equal(normalizeWebUrl("https://example.com/\r\nSet-Cookie:x"), "");
  assert.equal(normalizeWebUrl("https://example.com/\u0000"), "");
  assert.equal(normalizeWebUrl(`https://example.com/${"a".repeat(4100)}`), "");

  // Userinfo rejected, while @ in path or query remains allowed
  assert.equal(normalizeWebUrl("https://user:pass@example.com/login"), "");
  assert.equal(normalizeWebUrl("https://user@example.com"), "");
  assert.equal(normalizeWebUrl("http://:pass@10.7.12.233/admin"), "");
  assert.equal(normalizeWebUrl("https://:@example.com"), "");
  assert.equal(normalizeWebUrl("user@example.com"), "");
  assert.equal(normalizeWebUrl("https://example.com/@alice"), "https://example.com/@alice");
  assert.equal(
    normalizeWebUrl("example.com/login?email=alice@example.com"),
    "https://example.com/login?email=alice@example.com",
  );

  // Tauri internal hosts rejected (including trailing dots and subdomains)
  assert.equal(normalizeWebUrl("tauri.localhost"), "");
  assert.equal(normalizeWebUrl("http://tauri.localhost/"), "");
  assert.equal(normalizeWebUrl("https://tauri.localhost."), "");
  assert.equal(normalizeWebUrl("http://sub.tauri.localhost/index.html"), "");
  assert.equal(normalizeWebUrl("ipc.localhost"), "");
  assert.equal(normalizeWebUrl("http://ipc.localhost/"), "");
  assert.equal(normalizeWebUrl("asset.localhost"), "");
  assert.equal(normalizeWebUrl("https://asset.localhost/file.png"), "");

  // :// inside query/path/fragment does not break scheme insertion
  assert.equal(
    normalizeWebUrl("example.com/login?next=https://other.com"),
    "https://example.com/login?next=https://other.com",
  );
  assert.equal(
    normalizeWebUrl("localhost:3000/callback?next=https://other.com"),
    "http://localhost:3000/callback?next=https://other.com",
  );
  assert.equal(
    normalizeWebUrl("10.7.12.233:5093/app#https://other.com"),
    "http://10.7.12.233:5093/app#https://other.com",
  );

  // Trailing dots normalize local/private host detection
  assert.equal(normalizeWebUrl("localhost.:3000/dashboard"), "http://localhost.:3000/dashboard");
  assert.equal(normalizeWebUrl("nas.local./files"), "http://nas.local./files");
  assert.equal(normalizeWebUrl("router.lan./"), "http://router.lan./");

  // IPv4-mapped IPv6 detects private/local addresses vs public addresses
  assert.equal(
    normalizeWebUrl("[::ffff:10.7.12.233]:5093/ui"),
    "http://[::ffff:a07:ce9]:5093/ui",
  );
  assert.equal(
    normalizeWebUrl("[::ffff:192.168.1.20]/admin"),
    "http://[::ffff:c0a8:114]/admin",
  );
  assert.equal(
    normalizeWebUrl("[::ffff:127.0.0.1]:8080"),
    "http://[::ffff:7f00:1]:8080/",
  );
  assert.equal(
    normalizeWebUrl("[::ffff:100.95.41.126]:9000"),
    "http://[::ffff:645f:297e]:9000/",
  );
  assert.equal(
    normalizeWebUrl("[::ffff:8.8.8.8]/public"),
    "https://[::ffff:808:808]/public",
  );
});

test("extractWebAppDomain parses host cleanly", () => {
  assert.equal(extractWebAppDomain("https://calendar.google.com/calendar/u/0/r"), "calendar.google.com");
});

test("loadWebAppsConfig assigns distinct fallback IDs for multiple items without IDs", () => {
  const prevStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const prevDateNow = Date.now;
  const store = new Map<string, string>();
  store.set(
    WEBAPPS_STORAGE_KEY,
    JSON.stringify({
      enabled: true,
      items: [
        { name: "First App", url: "https://one.example.com" },
        { id: "   ", name: "Second App", url: "https://two.example.com" },
        { id: "explicit-id", name: "Third App", url: "https://three.example.com" },
      ],
    }),
  );

  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => store.set(k, v),
    },
  });
  Date.now = () => 1_700_000_000_000;

  try {
    const cfg = loadWebAppsConfig();
    assert.equal(cfg.enabled, true);
    assert.equal(cfg.items.length, 3);
    assert.equal(cfg.items[0].id, "wa-1700000000000-0");
    assert.equal(cfg.items[1].id, "wa-1700000000000-1");
    assert.equal(cfg.items[2].id, "explicit-id");
    assert.equal(new Set(cfg.items.map((x) => x.id)).size, 3);
  } finally {
    Date.now = prevDateNow;
    if (prevStorage) {
      Object.defineProperty(globalThis, "localStorage", prevStorage);
    } else {
      delete (globalThis as { localStorage?: unknown }).localStorage;
    }
  }
});
