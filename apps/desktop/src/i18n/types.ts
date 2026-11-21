export type UiLanguage = "vi" | "en" | "ko";

export const UI_LANGUAGE_OPTIONS: readonly {
  code: UiLanguage;
  label: string;
  nativeLabel: string;
}[] = [
  { code: "vi", label: "Tiếng Việt", nativeLabel: "Tiếng Việt" },
  { code: "en", label: "English", nativeLabel: "English" },
  { code: "ko", label: "한국어", nativeLabel: "한국어" },
];

export function uiLanguageToTranslateCode(lang: UiLanguage): "VI" | "EN" | "KO" {
  switch (lang) {
    case "en":
      return "EN";
    case "ko":
      return "KO";
    default:
      return "VI";
  }
}

export function uiLanguageToLocale(lang: UiLanguage): "vi-VN" | "en-US" | "ko-KR" {
  switch (lang) {
    case "en":
      return "en-US";
    case "ko":
      return "ko-KR";
    default:
      return "vi-VN";
  }
}
