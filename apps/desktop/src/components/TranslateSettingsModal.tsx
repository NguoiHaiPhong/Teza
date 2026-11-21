import { useEffect, useState } from "react";
import { Check, KeyRound, Languages, Loader2, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type {
  TranslateConfigSummary,
  TranslateConfigUpdate,
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
}

export function TranslateSettingsModal({
  open,
  config,
  onClose,
  onSaved,
}: Props) {
  const { t } = useI18n();
  const [provider, setProvider] = useState<TranslateProvider>("deepl");
  const [targetLang, setTargetLang] = useState("VI");
  const [deeplKey, setDeeplKey] = useState("");
  const [geminiKey, setGeminiKey] = useState("");
  const [geminiModel, setGeminiModel] = useState("gemini-2.0-flash");
  const [azureKey, setAzureKey] = useState("");
  const [azureRegion, setAzureRegion] = useState("southeastasia");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedNotice, setSavedNotice] = useState(false);

  useEffect(() => {
    if (!open || !config) return;
    setProvider(config.provider);
    setTargetLang(config.target_lang || "VI");
    setGeminiModel(config.gemini_model || "gemini-2.0-flash");
    setAzureRegion(config.azure_region || "southeastasia");
    setDeeplKey("");
    setGeminiKey("");
    setAzureKey("");
    setError(null);
    setSavedNotice(false);
  }, [open, config]);

  if (!open) return null;

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    setError(null);
    setSavedNotice(false);
    try {
      const payload: TranslateConfigUpdate = {
        provider,
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

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-lg rounded-2xl shadow-2xl overflow-hidden"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between px-5 py-3.5"
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

        <form onSubmit={handleSave} className="space-y-4 p-5">
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

          <div className="flex justify-end gap-2 pt-1">
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
        </form>
      </div>
    </div>
  );
}
