import { useEffect, useState } from "react";
import { Check, KeyRound, Languages, Loader2, Trash2, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type {
  TranslateConfigSummary,
  TranslateConfigUpdate,
  TranslateDetectMode,
  TranslateProvider,
} from "../types";

export const TRANSLATE_LANG_OPTIONS = [
  { code: "VI", label: "Tiếng Việt (VI)" },
  { code: "EN", label: "English (EN)" },
  { code: "JA", label: "日本語 (JA)" },
  { code: "KO", label: "한국어 (KO)" },
  { code: "ZH", label: "中文 (ZH)" },
  { code: "FR", label: "Français (FR)" },
  { code: "DE", label: "Deutsch (DE)" },
  { code: "RU", label: "Русский (RU)" },
  { code: "TH", label: "ภาษาไทย (TH)" },
  { code: "ID", label: "Bahasa Indonesia (ID)" },
];

interface Props {
  open: boolean;
  config: TranslateConfigSummary | null;
  onClose: () => void;
  onSaved: (cfg: TranslateConfigSummary) => void;
  onCacheCleared?: () => void;
}

export function TranslateSettingsModal({
  open,
  config,
  onClose,
  onSaved,
  onCacheCleared,
}: Props) {
  const { t } = useI18n();
  const [provider, setProvider] = useState<TranslateProvider>("deepl");
  const [detectMode, setDetectMode] = useState<TranslateDetectMode>("api_cache");
  const [targetLang, setTargetLang] = useState("VI");
  const [deeplKey, setDeeplKey] = useState("");
  const [geminiKey, setGeminiKey] = useState("");
  const [geminiModel, setGeminiModel] = useState("gemini-2.0-flash");
  const [azureKey, setAzureKey] = useState("");
  const [azureRegion, setAzureRegion] = useState("southeastasia");
  const [saving, setSaving] = useState(false);
  const [clearingCache, setClearingCache] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedNotice, setSavedNotice] = useState(false);
  const [cacheNotice, setCacheNotice] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !config) return;
    setProvider(config.provider);
    setDetectMode(config.detect_mode || "api_cache");
    setTargetLang(config.target_lang || "VI");
    setGeminiModel(config.gemini_model || "gemini-2.0-flash");
    setAzureRegion(config.azure_region || "southeastasia");
    setDeeplKey("");
    setGeminiKey("");
    setAzureKey("");
    setError(null);
    setSavedNotice(false);
    setCacheNotice(null);
  }, [open, config]);

  if (!open) return null;

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setError(null);
    setSavedNotice(false);
    setCacheNotice(null);
    try {
      const payload: TranslateConfigUpdate = {
        provider,
        detect_mode: detectMode,
        target_lang: targetLang,
        gemini_model: geminiModel.trim() || "gemini-2.0-flash",
        azure_region: azureRegion.trim() || "southeastasia",
      };
      if (deeplKey.trim()) payload.deepl_key = deeplKey.trim();
      if (geminiKey.trim()) payload.gemini_key = geminiKey.trim();
      if (azureKey.trim()) payload.azure_key = azureKey.trim();

      const updated = await bridge.saveTranslateConfig(payload);
      onSaved(updated);
      setDeeplKey("");
      setGeminiKey("");
      setAzureKey("");
      setSavedNotice(true);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleClearCache = async () => {
    setClearingCache(true);
    setError(null);
    setSavedNotice(false);
    setCacheNotice(null);
    try {
      const count = await bridge.clearTranslateCache();
      onCacheCleared?.();
      setCacheNotice(t("translateCacheClearedNotice", { count }));
    } catch (err) {
      setError(String(err));
    } finally {
      setClearingCache(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
      onClick={onClose}
    >
      <div
        className="flex max-h-[90vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex shrink-0 items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border)" }}
        >
          <div className="flex items-center gap-2.5">
            <Languages className="h-5 w-5" style={{ color: "var(--accent)" }} />
            <h3 className="text-[16px] font-semibold">
              {t("translateModalTitle")}
            </h3>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg p-1.5 hover:bg-black/10"
            title={t("close")}
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <form onSubmit={handleSave} className="space-y-4 overflow-y-auto p-5">
          <div>
            <label
              className="mb-1.5 block text-[13px] font-medium"
              style={{ color: "var(--text-secondary)" }}
            >
              {t("translateProviderLabel")}
            </label>
            <div className="grid grid-cols-3 gap-2">
              {(
                [
                  { id: "deepl", label: "DeepL", hasKey: config?.has_deepl_key },
                  { id: "gemini", label: "Gemini", hasKey: config?.has_gemini_key },
                  { id: "azure", label: "Azure", hasKey: config?.has_azure_key },
                ] as const
              ).map((item) => {
                const active = provider === item.id;
                return (
                  <button
                    key={item.id}
                    type="button"
                    onClick={() => setProvider(item.id)}
                    className="flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[14px] font-medium transition"
                    style={{
                      background: active ? "var(--accent)" : "var(--bg-surface)",
                      color: active ? "#fff" : "var(--text-primary)",
                      border: `1px solid ${active ? "var(--accent)" : "var(--border)"}`,
                    }}
                  >
                    <span>{item.label}</span>
                    {item.hasKey && (
                      <span
                        className="h-2 w-2 rounded-full"
                        style={{
                          background: active ? "#bbf7d0" : "#22c55e",
                        }}
                        title={t("translateApiConfiguredTitle")}
                      />
                    )}
                  </button>
                );
              })}
            </div>
          </div>

          <div>
            <label
              className="mb-1.5 block text-[13px] font-medium"
              style={{ color: "var(--text-secondary)" }}
            >
              {t("translateDefaultLangLabel")}
            </label>
            <select
              value={targetLang}
              onChange={(e) => setTargetLang(e.target.value)}
              className="w-full rounded-xl px-3 py-2 text-[14.5px] outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border)",
                color: "var(--text-primary)",
              }}
            >
              {TRANSLATE_LANG_OPTIONS.map((opt) => (
                <option key={opt.code} value={opt.code}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>

          <div>
            <label
              className="mb-1.5 block text-[13px] font-medium"
              style={{ color: "var(--text-secondary)" }}
            >
              {t("translateDetectModeLabel")}
            </label>
            <div className="space-y-2">
              {(
                [
                  {
                    id: "api_cache" as const,
                    title: t("translateModeApiCacheTitle"),
                    desc: t("translateModeApiCacheDesc"),
                  },
                  {
                    id: "local_smart_skip" as const,
                    title: t("translateModeLocalSkipTitle"),
                    desc: t("translateModeLocalSkipDesc"),
                  },
                ]
              ).map((opt) => {
                const active = detectMode === opt.id;
                return (
                  <button
                    key={opt.id}
                    type="button"
                    onClick={() => setDetectMode(opt.id)}
                    className="flex w-full items-start gap-2.5 rounded-xl px-3 py-2.5 text-left transition"
                    style={{
                      background: active ? "var(--accent-soft)" : "var(--bg-surface)",
                      border: `1px solid ${active ? "var(--accent)" : "var(--border)"}`,
                    }}
                  >
                    <span
                      className="mt-1 h-2.5 w-2.5 shrink-0 rounded-full"
                      style={{
                        background: active ? "var(--accent)" : "var(--border)",
                      }}
                    />
                    <div className="min-w-0 flex-1">
                      <div
                        className="text-[13.5px] font-semibold"
                        style={{ color: active ? "var(--accent)" : "var(--text-primary)" }}
                      >
                        {opt.title}
                      </div>
                      <div
                        className="mt-0.5 text-[12px] leading-snug"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        {opt.desc}
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          </div>

          {provider === "deepl" && (
            <div
              className="space-y-2 rounded-xl p-3.5"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border)",
              }}
            >
              <div className="flex items-center justify-between text-[13px]">
                <span className="flex items-center gap-1.5 font-medium">
                  <KeyRound className="h-3.5 w-3.5" />
                  DeepL API Key
                </span>
                <span style={{ color: "var(--text-secondary)" }}>
                  {config?.has_deepl_key
                    ? t("keySavedPreview", { preview: config.deepl_preview || "" })
                    : t("keyMissing")}
                </span>
              </div>
              <input
                type="password"
                value={deeplKey}
                onChange={(e) => setDeeplKey(e.target.value)}
                placeholder={
                  config?.has_deepl_key
                    ? t("keepExistingKeyPlaceholder")
                    : t("deeplKeyPlaceholder")
                }
                className="w-full rounded-lg px-3 py-2 text-[14px] outline-none"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
                {t("deeplKeyHint")}
              </p>
            </div>
          )}

          {provider === "gemini" && (
            <div
              className="space-y-2.5 rounded-xl p-3.5"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border)",
              }}
            >
              <div className="flex items-center justify-between text-[13px]">
                <span className="flex items-center gap-1.5 font-medium">
                  <KeyRound className="h-3.5 w-3.5" />
                  Gemini API Key
                </span>
                <span style={{ color: "var(--text-secondary)" }}>
                  {config?.has_gemini_key
                    ? t("keySavedPreview", { preview: config.gemini_preview || "" })
                    : t("keyMissing")}
                </span>
              </div>
              <input
                type="password"
                value={geminiKey}
                onChange={(e) => setGeminiKey(e.target.value)}
                placeholder={
                  config?.has_gemini_key
                    ? t("keepExistingKeyPlaceholder")
                    : t("geminiKeyPlaceholder")
                }
                className="w-full rounded-lg px-3 py-2 text-[14px] outline-none"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              <div>
                <label
                  className="mb-1 block text-xs"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("geminiModelLabel")}
                </label>
                <input
                  type="text"
                  value={geminiModel}
                  onChange={(e) => setGeminiModel(e.target.value)}
                  placeholder="gemini-2.0-flash"
                  className="w-full rounded-lg px-3 py-1.5 text-[14px] outline-none"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border)",
                    color: "var(--text-primary)",
                  }}
                />
              </div>
            </div>
          )}

          {provider === "azure" && (
            <div
              className="space-y-2.5 rounded-xl p-3.5"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border)",
              }}
            >
              <div className="flex items-center justify-between text-[13px]">
                <span className="flex items-center gap-1.5 font-medium">
                  <KeyRound className="h-3.5 w-3.5" />
                  Azure Translator Key
                </span>
                <span style={{ color: "var(--text-secondary)" }}>
                  {config?.has_azure_key
                    ? t("keySavedPreview", { preview: config.azure_preview || "" })
                    : t("keyMissing")}
                </span>
              </div>
              <input
                type="password"
                value={azureKey}
                onChange={(e) => setAzureKey(e.target.value)}
                placeholder={
                  config?.has_azure_key
                    ? t("keepExistingKeyPlaceholder")
                    : t("azureKeyPlaceholder")
                }
                className="w-full rounded-lg px-3 py-2 text-[14px] outline-none"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border)",
                  color: "var(--text-primary)",
                }}
              />
              <div>
                <label
                  className="mb-1 block text-xs"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("azureRegionLabel")}
                </label>
                <input
                  type="text"
                  value={azureRegion}
                  onChange={(e) => setAzureRegion(e.target.value)}
                  placeholder="southeastasia"
                  className="w-full rounded-lg px-3 py-1.5 text-[14px] outline-none"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border)",
                    color: "var(--text-primary)",
                  }}
                />
              </div>
            </div>
          )}

          {error && (
            <div
              className="rounded-xl px-3 py-2 text-[13px]"
              style={{
                background: "var(--danger-bg)",
                color: "var(--danger-text)",
              }}
            >
              {error}
            </div>
          )}

          {savedNotice && (
            <div
              className="flex items-center gap-1.5 rounded-xl px-3 py-2 text-[13px]"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
              }}
            >
              <Check className="h-4 w-4" />
              {t("translateSavedNotice")}
            </div>
          )}

          {cacheNotice && (
            <div
              className="flex items-center gap-1.5 rounded-xl px-3 py-2 text-[13px]"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
              }}
            >
              <Check className="h-4 w-4" />
              {cacheNotice}
            </div>
          )}

          <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
            <button
              type="button"
              disabled={clearingCache || saving}
              onClick={() => void handleClearCache()}
              className="hover-menu flex items-center gap-1.5 rounded-xl px-3 py-2 text-[13px] font-medium transition disabled:opacity-60"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border)",
                color: "var(--text-secondary)",
              }}
            >
              {clearingCache ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Trash2 className="h-3.5 w-3.5" />
              )}
              {t("translateClearCacheBtn")}
            </button>
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={onClose}
                className="rounded-xl px-4 py-2 text-[14px] font-medium"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border)",
                }}
              >
                {t("close")}
              </button>
              <button
                type="submit"
                disabled={saving}
                className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-[14px] font-medium text-white disabled:opacity-60"
                style={{ background: "var(--accent)" }}
              >
                {saving && <Loader2 className="h-4 w-4 animate-spin" />}
                {t("saveSettingsBtn")}
              </button>
            </div>
          </div>
        </form>
      </div>
    </div>
  );
}
