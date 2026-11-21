import test from "node:test";
import assert from "node:assert/strict";
import { dictionaries, translateKey } from "../src/i18n/core.ts";
import {
  uiLanguageToLocale,
  uiLanguageToTranslateCode,
} from "../src/i18n/types.ts";
import { vi, type TranslationKey } from "../src/i18n/vi.ts";

test("all i18n dictionaries (vi, en, ko) contain identical non-empty keys", () => {
  const keys = Object.keys(vi) as TranslationKey[];
  assert.ok(keys.length > 80);

  for (const lang of ["vi", "en", "ko"] as const) {
    const dict = dictionaries[lang];
    for (const key of keys) {
      assert.equal(typeof dict[key], "string", `${lang}.${key} must exist`);
      assert.ok(dict[key].trim().length > 0, `${lang}.${key} must be non-empty`);
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
