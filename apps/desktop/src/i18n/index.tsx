import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  UI_LANG_STORAGE_KEY,
  loadUiLanguage,
  normalizeUiLanguage,
  saveUiLanguage,
  translateKey,
} from "./core";
import {
  uiLanguageToLocale,
  uiLanguageToTranslateCode,
  type UiLanguage,
} from "./types";
import type { TranslationKey } from "./vi";

export type { TranslationKey } from "./vi";
export { UI_LANGUAGE_OPTIONS, uiLanguageToLocale, uiLanguageToTranslateCode, type UiLanguage } from "./types";

interface I18nContextValue {
  lang: UiLanguage;
  setLang: (next: UiLanguage) => void;
  locale: "vi-VN" | "en-US" | "ko-KR";
  translateCode: "VI" | "EN" | "KO";
  t: (key: TranslationKey, vars?: Record<string, string | number>) => string;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<UiLanguage>(() => loadUiLanguage());

  const setLang = useCallback((next: UiLanguage) => {
    const norm = normalizeUiLanguage(next);
    saveUiLanguage(norm);
    setLangState(norm);
  }, []);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === UI_LANG_STORAGE_KEY) {
        setLangState(normalizeUiLanguage(e.newValue));
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const value = useMemo<I18nContextValue>(
    () => ({
      lang,
      setLang,
      locale: uiLanguageToLocale(lang),
      translateCode: uiLanguageToTranslateCode(lang),
      t: (key, vars) => translateKey(lang, key, vars),
    }),
    [lang, setLang],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (!ctx) {
    return {
      lang: "vi",
      setLang: () => undefined,
      locale: "vi-VN",
      translateCode: "VI",
      t: (key, vars) => translateKey("vi", key, vars),
    };
  }
  return ctx;
}
