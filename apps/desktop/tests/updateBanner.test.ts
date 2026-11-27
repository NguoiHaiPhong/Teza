import test from "node:test";
import assert from "node:assert/strict";
import { translateKey } from "../src/i18n/core.ts";
import {
  DISMISSED_UPDATE_VERSION_STORAGE_KEY,
  formatUpdateVersionTag,
  isUpdateBannerVisible,
  loadDismissedUpdateVersion,
  normalizeUpdateVersion,
  saveDismissedUpdateVersion,
  type UpdateVersionStorage,
} from "../src/utils/updateBanner.ts";

function createMemoryStorage(initial: Record<string, string> = {}): UpdateVersionStorage & {
  dump: () => Record<string, string>;
} {
  const map = new Map<string, string>(Object.entries(initial));
  return {
    getItem: (key: string) => (map.has(key) ? map.get(key)! : null),
    setItem: (key: string, value: string) => {
      map.set(key, value);
    },
    removeItem: (key: string) => {
      map.delete(key);
    },
    dump: () => Object.fromEntries(map.entries()),
  };
}

test("normalizeUpdateVersion strips leading v/V and whitespace", () => {
  assert.equal(normalizeUpdateVersion("0.7.1"), "0.7.1");
  assert.equal(normalizeUpdateVersion("v0.7.1"), "0.7.1");
  assert.equal(normalizeUpdateVersion("  V0.7.1  "), "0.7.1");
  assert.equal(normalizeUpdateVersion("   "), "");
  assert.equal(normalizeUpdateVersion(null), "");
  assert.equal(normalizeUpdateVersion(undefined), "");
});

test("formatUpdateVersionTag prefixes a single v or returns empty string", () => {
  assert.equal(formatUpdateVersionTag("0.7.1"), "v0.7.1");
  assert.equal(formatUpdateVersionTag("v0.7.1"), "v0.7.1");
  assert.equal(formatUpdateVersionTag(" V1.2.3-beta.1 "), "v1.2.3-beta.1");
  assert.equal(formatUpdateVersionTag(""), "");
  assert.equal(formatUpdateVersionTag(null), "");
});

test("isUpdateBannerVisible handles null, matching dismissed version, and newer versions", () => {
  assert.equal(isUpdateBannerVisible(null, null), false);
  assert.equal(isUpdateBannerVisible(undefined, null), false);
  assert.equal(isUpdateBannerVisible({ version: "   " }, null), false);

  // Visible when an update exists and nothing was dismissed
  assert.equal(isUpdateBannerVisible({ version: "0.7.1" }, null), true);
  assert.equal(isUpdateBannerVisible({ version: "0.7.1" }, ""), true);

  // Dismissing v0.7.1 hides the banner for v0.7.1 (with or without leading v)
  assert.equal(isUpdateBannerVisible({ version: "0.7.1" }, "0.7.1"), false);
  assert.equal(isUpdateBannerVisible({ version: "v0.7.1" }, "0.7.1"), false);
  assert.equal(isUpdateBannerVisible({ version: "0.7.1" }, "v0.7.1"), false);

  // When v0.7.2 arrives, the banner automatically reappears even though v0.7.1 was dismissed
  assert.equal(isUpdateBannerVisible({ version: "0.7.2" }, "0.7.1"), true);
  assert.equal(isUpdateBannerVisible({ version: "v0.7.2" }, "v0.7.1"), true);
});

test("loadDismissedUpdateVersion and saveDismissedUpdateVersion persist normalized versions safely", () => {
  const storage = createMemoryStorage();
  assert.equal(loadDismissedUpdateVersion(storage), null);

  assert.equal(saveDismissedUpdateVersion("v0.7.1", storage), "0.7.1");
  assert.equal(storage.dump()[DISMISSED_UPDATE_VERSION_STORAGE_KEY], "0.7.1");
  assert.equal(loadDismissedUpdateVersion(storage), "0.7.1");

  // Overwriting with a newer dismissed version replaces the stored version
  assert.equal(saveDismissedUpdateVersion(" 0.7.2 ", storage), "0.7.2");
  assert.equal(loadDismissedUpdateVersion(storage), "0.7.2");

  // Clearing removes the storage entry
  assert.equal(saveDismissedUpdateVersion(null, storage), null);
  assert.equal(DISMISSED_UPDATE_VERSION_STORAGE_KEY in storage.dump(), false);
  assert.equal(loadDismissedUpdateVersion(storage), null);
});

test("storage helpers fail gracefully when storage throws or is null", () => {
  assert.equal(loadDismissedUpdateVersion(null), null);
  assert.equal(saveDismissedUpdateVersion("0.7.1", null), "0.7.1");

  const brokenStorage: UpdateVersionStorage = {
    getItem: () => {
      throw new Error("storage blocked");
    },
    setItem: () => {
      throw new Error("quota exceeded");
    },
    removeItem: () => {
      throw new Error("storage blocked");
    },
  };
  assert.equal(loadDismissedUpdateVersion(brokenStorage), null);
  assert.equal(saveDismissedUpdateVersion("0.7.1", brokenStorage), "0.7.1");
  assert.equal(saveDismissedUpdateVersion("", brokenStorage), null);
});

test("update banner and rail update i18n keys interpolate versions across vi, en, and ko", () => {
  for (const lang of ["vi", "en", "ko"] as const) {
    const banner = translateKey(lang, "updateBannerText", { version: "0.7.2" });
    const tooltip = translateKey(lang, "railUpdateTooltip", { version: "0.7.2" });
    const railLabel = translateKey(lang, "railUpdateLabel");
    const updateNow = translateKey(lang, "updateNowBtn");
    const later = translateKey(lang, "updateLaterTitle");

    assert.ok(banner.includes("v0.7.2"), `${lang}.updateBannerText should include v0.7.2`);
    assert.ok(tooltip.includes("v0.7.2"), `${lang}.railUpdateTooltip should include v0.7.2`);
    assert.ok(railLabel.trim().length > 0, `${lang}.railUpdateLabel should be non-empty`);
    assert.ok(updateNow.trim().length > 0, `${lang}.updateNowBtn should be non-empty`);
    assert.ok(later.trim().length > 0, `${lang}.updateLaterTitle should be non-empty`);
  }
});
