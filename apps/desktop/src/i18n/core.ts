import { en } from "./en.ts";
import { ko } from "./ko.ts";
import type { UiLanguage } from "./types.ts";
import { vi, type TranslationKey } from "./vi.ts";

export const UI_LANG_STORAGE_KEY = "teza.uiLanguage";

export const dictionaries: Record<UiLanguage, Record<TranslationKey, string>> = {
  vi,
  en,
  ko,
};

export function normalizeUiLanguage(raw: unknown): UiLanguage {
  return raw === "en" || raw === "ko" ? raw : "vi";
}

export function loadUiLanguage(): UiLanguage {
  try {
    if (typeof localStorage === "undefined") return "vi";
    return normalizeUiLanguage(localStorage.getItem(UI_LANG_STORAGE_KEY));
  } catch {
    return "vi";
  }
}

export function saveUiLanguage(lang: UiLanguage): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(UI_LANG_STORAGE_KEY, lang);
    }
  } catch {
    // ignore storage failures
  }
}

export function translateKey(
  lang: UiLanguage,
  key: TranslationKey,
  vars?: Record<string, string | number>,
): string {
  const dict = dictionaries[lang] ?? vi;
  let template = dict[key] ?? vi[key] ?? key;
  if (!vars) return template;
  for (const [k, v] of Object.entries(vars)) {
    template = template.replaceAll(`{${k}}`, String(v));
  }
  return template;
}
