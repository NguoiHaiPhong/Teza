import test from "node:test";
import assert from "node:assert/strict";
import {
  buildWebAppAccountInput,
  extractWebAppOrigin,
  filterDemoWebAppAccounts,
  isWebAppOverlaySuspended,
  resolveWebAppAccountLabel,
  sortWebAppAccountsForDisplay,
  upsertDemoWebAppAccount,
} from "../src/utils/webappAccounts.ts";
import type { WebAppAccountDto } from "../src/types.ts";

test("extractWebAppOrigin normalizes public and LAN URLs to canonical origins", () => {
  assert.equal(extractWebAppOrigin("https://App.Example.COM:443/login?next=/"), "https://app.example.com");
  assert.equal(extractWebAppOrigin("https://portal.company.vn:8443/auth"), "https://portal.company.vn:8443");
  assert.equal(extractWebAppOrigin("10.7.12.233:5093/summary_error.html"), "http://10.7.12.233:5093");
  assert.equal(extractWebAppOrigin("http://10.7.12.233:80/index.html"), "http://10.7.12.233");
  assert.equal(extractWebAppOrigin("localhost:3000/signin"), "http://localhost:3000");
  assert.equal(extractWebAppOrigin("notion.so/login"), "https://notion.so");
  assert.equal(extractWebAppOrigin("javascript:alert(1)"), "");
  assert.equal(extractWebAppOrigin("file:///C:/secret.txt"), "");
  assert.equal(extractWebAppOrigin("https://user:pass@example.com/login"), "");
  assert.equal(extractWebAppOrigin("http://tauri.localhost/"), "");
  assert.equal(extractWebAppOrigin("https://example.com/\n"), "");
  assert.equal(
    extractWebAppOrigin("example.com/login?next=https://other.com"),
    "https://example.com",
  );
  assert.equal(
    extractWebAppOrigin("[::ffff:10.7.12.233]:5093/ui"),
    "http://[::ffff:a07:ce9]:5093",
  );
  assert.equal(extractWebAppOrigin(""), "");
});

test("resolveWebAppAccountLabel falls back cleanly from label -> username -> appName", () => {
  assert.equal(resolveWebAppAccountLabel("  Admin  ", "root@corp.vn", "Portal"), "Admin");
  assert.equal(resolveWebAppAccountLabel("   ", "alice@corp.vn", "Portal"), "alice@corp.vn");
  assert.equal(resolveWebAppAccountLabel("", "   ", "  Internal CRM  "), "Internal CRM");
  assert.equal(resolveWebAppAccountLabel("", "", ""), "Account");
});

test("buildWebAppAccountInput handles create, edit preserve-password, and clear-password flows", () => {
  const created = buildWebAppAccountInput({
    editingAccount: null,
    appId: "wa-1",
    appUrl: "10.7.12.233:5093/login",
    appName: "Ops Dashboard",
    label: "",
    username: "  ops@corp.vn  ",
    password: "secret-value",
    autoFill: true,
  });
  assert.deepEqual(created, {
    id: null,
    app_id: "wa-1",
    url: "http://10.7.12.233:5093/login",
    label: "ops@corp.vn",
    username: "ops@corp.vn",
    password: "secret-value",
    clear_password: false,
    auto_fill: true,
  });

  const existing: WebAppAccountDto = {
    id: "acc-1",
    app_id: "wa-1",
    origin: "http://10.7.12.233:5093",
    label: "Ops",
    username: "ops@corp.vn",
    has_password: true,
    auto_fill: false,
    updated_at_ms: 1000,
  };

  const editKeepPw = buildWebAppAccountInput({
    editingAccount: existing,
    appId: "wa-1",
    appUrl: "http://10.7.12.233:5093/login",
    appName: "Ops Dashboard",
    label: "Ops Primary",
    username: "ops@corp.vn",
    password: "",
    autoFill: true,
  });
  assert.equal(editKeepPw.id, "acc-1");
  assert.equal(editKeepPw.password, null);
  assert.equal(editKeepPw.clear_password, false);
  assert.equal(editKeepPw.auto_fill, true);

  const editClearPw = buildWebAppAccountInput({
    editingAccount: existing,
    appId: "wa-1",
    appUrl: "http://10.7.12.233:5093/login",
    appName: "Ops Dashboard",
    label: "Ops Primary",
    username: "ops@corp.vn",
    password: "ignored-when-clearing",
    clearPassword: true,
    autoFill: false,
  });
  assert.equal(editClearPw.password, null);
  assert.equal(editClearPw.clear_password, true);
});

test("isWebAppOverlaySuspended suspends native WebView2 when external modal or accounts panel is open", () => {
  assert.equal(isWebAppOverlaySuspended(false, false), false);
  assert.equal(isWebAppOverlaySuspended(true, false), true);
  assert.equal(isWebAppOverlaySuspended(false, true), true);
  assert.equal(isWebAppOverlaySuspended(true, true), true);
});

test("demo vault helpers enforce single auto_fill per (app_id, origin), preserve password, and filter", () => {
  let store: WebAppAccountDto[] = [];

  const first = upsertDemoWebAppAccount(
    store,
    {
      app_id: "wa-1",
      url: "https://erp.company.vn/login",
      label: "Personal",
      username: "loc@company.vn",
      password: "pw1",
      auto_fill: true,
    },
    100,
  );
  store = first.next;
  assert.equal(first.saved.has_password, true);
  assert.equal(first.saved.auto_fill, true);
  assert.equal(first.saved.origin, "https://erp.company.vn");

  const second = upsertDemoWebAppAccount(
    store,
    {
      app_id: "wa-1",
      url: "https://erp.company.vn/admin",
      label: "Admin",
      username: "admin@company.vn",
      password: "pw2",
      auto_fill: true,
    },
    200,
  );
  store = second.next;
  assert.equal(store.length, 2);
  assert.equal(store.find((a) => a.id === second.saved.id)?.auto_fill, true);
  assert.equal(store.find((a) => a.id === first.saved.id)?.auto_fill, false);

  // Edit first without password -> keeps has_password === true
  const edited = upsertDemoWebAppAccount(
    store,
    {
      id: first.saved.id,
      app_id: "wa-1",
      url: "https://erp.company.vn/login",
      label: "Personal Updated",
      username: "loc@company.vn",
      password: null,
      clear_password: false,
      auto_fill: false,
    },
    300,
  );
  store = edited.next;
  assert.equal(edited.saved.has_password, true);
  assert.equal(edited.saved.label, "Personal Updated");

  // Clear password explicitly
  const cleared = upsertDemoWebAppAccount(
    store,
    {
      id: first.saved.id,
      app_id: "wa-1",
      url: "https://erp.company.vn/login",
      label: "Personal Updated",
      username: "loc@company.vn",
      password: null,
      clear_password: true,
      auto_fill: false,
    },
    400,
  );
  store = cleared.next;
  assert.equal(cleared.saved.has_password, false);

  // Sort puts auto_fill first even if older updated_at_ms
  const sorted = sortWebAppAccountsForDisplay(store);
  assert.equal(sorted[0].id, second.saved.id);
  assert.equal(sorted[1].id, first.saved.id);

  // Filter matches Rust WebAppAccountStore::list semantics (AND when both provided)
  assert.equal(filterDemoWebAppAccounts(store, "wa-1", "https://erp.company.vn").length, 2);
  assert.equal(filterDemoWebAppAccounts(store, undefined, "https://erp.company.vn").length, 2);
  assert.equal(filterDemoWebAppAccounts(store, "wa-other", "https://erp.company.vn").length, 0);
  assert.equal(filterDemoWebAppAccounts(store, "wa-1", "https://other.example").length, 0);
  assert.equal(filterDemoWebAppAccounts(store, "wa-1").length, 2);

  // Origin change on an account with saved password must refuse silent carry-over
  assert.throws(() => {
    upsertDemoWebAppAccount(
      store,
      {
        id: second.saved.id,
        app_id: "wa-1",
        url: "https://new-origin.company.vn/login",
        label: "Admin",
        username: "admin@company.vn",
        password: null,
        clear_password: false,
        auto_fill: true,
      },
      500,
    );
  });

  // Origin change succeeds when explicitly clearing or replacing password
  const migrated = upsertDemoWebAppAccount(
    store,
    {
      id: second.saved.id,
      app_id: "wa-1",
      url: "https://new-origin.company.vn/login",
      label: "Admin",
      username: "admin@company.vn",
      password: "new-origin-password",
      clear_password: false,
      auto_fill: true,
    },
    501,
  );
  assert.equal(migrated.saved.origin, "https://new-origin.company.vn");
  assert.equal(migrated.saved.has_password, true);
});

test("upsertDemoWebAppAccount rejects unknown id and enforces Rust max-length limits", () => {
  const existing = upsertDemoWebAppAccount(
    [],
    {
      app_id: "wa-1",
      url: "https://portal.example.com/login",
      label: "Work",
      username: "user@example.com",
      password: "secret",
      auto_fill: true,
    },
    1000,
  ).next;

  // Non-existent id must throw instead of silently creating a new record
  assert.throws(
    () =>
      upsertDemoWebAppAccount(
        existing,
        {
          id: "does-not-exist",
          app_id: "wa-1",
          url: "https://portal.example.com/login",
          label: "Ghost",
          username: "ghost@example.com",
          password: "pw",
          auto_fill: false,
        },
        1001,
      ),
    /Account not found/i,
  );

  // Max length checks matching Rust WebAppAccountStore::save (256 / 256 / 512 / 2048)
  assert.throws(
    () =>
      upsertDemoWebAppAccount(existing, {
        app_id: "a".repeat(257),
        url: "https://portal.example.com/login",
        label: "Work",
        username: "user@example.com",
        auto_fill: false,
      }),
    /Web App ID exceeds maximum length/i,
  );

  assert.throws(
    () =>
      upsertDemoWebAppAccount(existing, {
        app_id: "wa-1",
        url: "https://portal.example.com/login",
        label: "l".repeat(257),
        username: "user@example.com",
        auto_fill: false,
      }),
    /Label exceeds maximum length/i,
  );

  assert.throws(
    () =>
      upsertDemoWebAppAccount(existing, {
        app_id: "wa-1",
        url: "https://portal.example.com/login",
        label: "Short",
        username: "u".repeat(513),
        auto_fill: false,
      }),
    /Username exceeds maximum length/i,
  );

  assert.throws(
    () =>
      upsertDemoWebAppAccount(existing, {
        app_id: "wa-1",
        url: "https://portal.example.com/login",
        label: "Short",
        username: "user@example.com",
        password: "p".repeat(2049),
        auto_fill: false,
      }),
    /Password exceeds maximum length/i,
  );

  // Exact boundary lengths succeed
  const boundary = upsertDemoWebAppAccount(
    existing,
    {
      app_id: "a".repeat(256),
      url: "https://portal.example.com/login",
      label: "l".repeat(256),
      username: "u".repeat(512),
      password: "p".repeat(2048),
      auto_fill: false,
    },
    1002,
  );
  assert.equal(boundary.saved.label.length, 256);
  assert.equal(boundary.saved.username.length, 512);
  assert.equal(boundary.saved.has_password, true);
});
