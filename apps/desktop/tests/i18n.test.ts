import test from "node:test";
import assert from "node:assert/strict";
import { dictionaries, translateKey } from "../src/i18n/core.ts";
import {
  uiLanguageToLocale,
  uiLanguageToTranslateCode,
} from "../src/i18n/types.ts";
import { vi, type TranslationKey } from "../src/i18n/vi.ts";

function extractPlaceholders(template: string): string[] {
  const names = new Set<string>();
  for (const match of template.matchAll(/\{([a-zA-Z0-9_]+)\}/g)) {
    names.add(match[1]);
  }
  return [...names].sort();
}

test("all i18n dictionaries (vi, en, ko) have exact bidirectional key parity and matching {placeholder} sets", () => {
  const viKeys = (Object.keys(vi) as TranslationKey[]).sort();
  assert.ok(viKeys.length > 80);

  for (const lang of ["vi", "en", "ko"] as const) {
    const dict = dictionaries[lang];
    const langKeys = Object.keys(dict).sort();
    assert.deepEqual(langKeys, viKeys, `${lang} dictionary keys must match vi exactly (no missing or extra keys)`);

    for (const key of viKeys) {
      const val = dict[key];
      assert.equal(typeof val, "string", `${lang}.${key} must exist`);
      assert.ok(val.trim().length > 0, `${lang}.${key} must be non-empty`);
      assert.deepEqual(
        extractPlaceholders(val),
        extractPlaceholders(vi[key]),
        `${lang}.${key} placeholders must match vi.${key}`,
      );
    }
  }
});

test("translateKey interpolates variables and language mapping matches translation codes", () => {
  assert.equal(translateKey("vi", "archiveBannerTitle", { count: 4 }), "Hộp lưu trữ (4)");
  assert.equal(translateKey("en", "archiveBannerTitle", { count: 4 }), "Archived (4)");
  assert.equal(translateKey("ko", "archiveBannerTitle", { count: 4 }), "보관함 (4)");

  assert.equal(uiLanguageToTranslateCode("vi"), "VI");
  assert.equal(uiLanguageToTranslateCode("en"), "EN");
  assert.equal(uiLanguageToTranslateCode("ko"), "KO");

  assert.equal(uiLanguageToLocale("vi"), "vi-VN");
  assert.equal(uiLanguageToLocale("en"), "en-US");
  assert.equal(uiLanguageToLocale("ko"), "ko-KR");
});

test("Round-2 UX workflow keys exist and interpolate cleanly across vi, en, and ko", () => {
  for (const lang of ["vi", "en", "ko"] as const) {
    assert.ok(translateKey(lang, "lockScreenLockoutError", { seconds: 30 }).includes("30"));
    assert.ok(translateKey(lang, "lockScreenAttemptsHint", { remaining: 2 }).includes("2"));
    assert.ok(translateKey(lang, "screenshotDiscardDesc", { count: 4 }).includes("4"));
    assert.ok(translateKey(lang, "qrExpiresIn", { time: "02:59" }).includes("02:59"));
    assert.ok(translateKey(lang, "navSchedulesAttentionTitle", { count: 3 }).includes("3"));
    assert.ok(translateKey(lang, "schedMissedToast", { title: "Team" }).includes("Team"));
    assert.ok(
      translateKey(lang, "schedFailedToast", { title: "Team", error: "Timeout" }).includes(
        "Timeout",
      ),
    );
    assert.ok(translateKey(lang, "localApiAuditLoadMoreBtn", { limit: 250 }).includes("250"));
    assert.ok(translateKey(lang, "localApiUnsavedTokenConfirm").length > 10);
    assert.ok(translateKey(lang, "pinnedNextTitle").length > 3);
    assert.ok(translateKey(lang, "backToConversationList").length > 3);
    assert.ok(translateKey(lang, "gdriveLockVaultBtn").length > 2);
  }
});

test("ZeroClaw Teza MCP 2-axis permission and approval keys interpolate cleanly across vi, en, and ko", () => {
  for (const lang of ["vi", "en", "ko"] as const) {
    assert.ok(translateKey(lang, "aiPermSectionTitle").includes("ZeroClaw"));
    assert.ok(translateKey(lang, "aiMcpStatusRunning", { count: 26 }).includes("26"));
    assert.ok(translateKey(lang, "aiMcpPendingApprovalsBadge", { count: 2 }).includes("2"));
    assert.ok(translateKey(lang, "aiMcpActivePsBadge", { count: 1 }).includes("1"));
    assert.ok(translateKey(lang, "aiAuditLogToggleTitle", { count: 15 }).includes("15"));
    assert.ok(translateKey(lang, "aiApprovalCountdown", { secs: 45 }).includes("45"));
    const otherNotice = translateKey(lang, "aiApprovalOtherSessionNotice", {
      count: 3,
      session: "Work",
    });
    assert.ok(otherNotice.includes("3") && otherNotice.includes("Work"));
    const estopDone = translateKey(lang, "aiEstopDoneToast", {
      approvals: 2,
      processes: 1,
    });
    assert.ok(estopDone.includes("2") && estopDone.includes("1"));
    assert.ok(translateKey(lang, "aiApprovalResolveFailed", { error: "IPC" }).includes("IPC"));
    assert.ok(translateKey(lang, "aiRailPendingApprovalTooltip", { count: 4 }).includes("4"));
    assert.ok(translateKey(lang, "aiComposeDraftToast", { title: "Team" }).includes("Team"));
  }
});
