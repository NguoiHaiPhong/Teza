import { useEffect, useRef, useState } from "react";
import { Check, KeyRound, Languages, Loader2, Trash2, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type {
  TranslateConfigSummary,
  TranslateConfigUpdate,
  TranslateDetectMode,
} from "../types";
import { isImeComposingEvent } from "../utils/searchNormalize";

type TranslateConfigUpdatePayload = TranslateConfigUpdate & {
  clear_deepl_key?: boolean;
};

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
  isLocked?: boolean;
  config: TranslateConfigSummary | null;
  relayEnabled?: boolean;
  onClose: () => void;
  onSaved: (cfg: TranslateConfigSummary) => void;
  onCacheCleared?: () => void;
}

export function TranslateSettingsModal({
  open,
  isLocked = false,
  config,
  onClose,
  onSaved,
  onCacheCleared,
}: Props) {
  const { t } = useI18n();
  const [detectMode, setDetectMode] = useState<TranslateDetectMode>("api_cache");
  const [targetLang, setTargetLang] = useState("VI");
  const [deeplKey, setDeeplKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [clearingKey, setClearingKey] = useState(false);
  const [clearingCache, setClearingCache] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedNotice, setSavedNotice] = useState<string | null>(null);
  const [cacheNotice, setCacheNotice] = useState<string | null>(null);
  const prevOpenRef = useRef(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const justOpened = open && !prevOpenRef.current;
    prevOpenRef.current = open;
    if (!open) return;
    if (config) {
      setDetectMode(config.detect_mode || "api_cache");
      setTargetLang(config.target_lang || "VI");
    }
    if (justOpened) {
      setDeeplKey("");
      setError(null);
      setSavedNotice(null);
      setCacheNotice(null);
    }
  }, [open, config]);

  const isBusy = saving || clearingKey || clearingCache;

  useEffect(() => {
    if (!open || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked || isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (!saving && !clearingKey) {
          onClose();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, isLocked, saving, clearingKey, onClose]);

  if (!open) return null;

  const handleRequestClose = () => {
    if (saving || clearingKey) return;
    onClose();
  };

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    if (saving || clearingKey) return;
    setSaving(true);
    setError(null);
    setSavedNotice(null);
    setCacheNotice(null);
    try {
      const payload: TranslateConfigUpdatePayload = {
        provider: "deepl",
        detect_mode: detectMode,
        target_lang: targetLang,
      };
      if (deeplKey.trim()) payload.deepl_key = deeplKey.trim();

      const updated = await bridge.saveTranslateConfig(payload);
      if (!mountedRef.current) return;
      onSaved(updated);
      setDeeplKey("");
      setSavedNotice(t("translateSavedNotice"));
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  const handleClearSavedKey = async () => {
    if (saving || clearingKey || !config?.has_deepl_key) return;
    setClearingKey(true);
    setError(null);
    setSavedNotice(null);
    setCacheNotice(null);
    try {
      const payload: TranslateConfigUpdatePayload = {
        provider: "deepl",
        detect_mode: detectMode,
        target_lang: targetLang,
        clear_deepl_key: true,
      };
      const updated = await bridge.saveTranslateConfig(payload);
      if (!mountedRef.current) return;
      onSaved(updated);
      setDeeplKey("");
      setSavedNotice(t("translateKeyClearedNotice"));
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setClearingKey(false);
    }
  };

  const handleClearCache = async () => {
    if (isBusy) return;
    setClearingCache(true);
    setError(null);
    setSavedNotice(null);
    setCacheNotice(null);
    try {
      const count = await bridge.clearTranslateCache();
      if (!mountedRef.current) return;
      onCacheCleared?.();
      setCacheNotice(t("translateCacheClearedNotice", { count }));
    } catch (err) {
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) setClearingCache(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4"
      onClick={handleRequestClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t("translateModalTitle")}
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
            disabled={saving || clearingKey}
            onClick={handleRequestClose}
            className="rounded-lg p-1.5 hover:bg-black/10 disabled:cursor-not-allowed disabled:opacity-40"
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

          <div
            className="space-y-2 rounded-xl p-3.5"
            style={{
              background: "var(--bg-surface)",
              border: "1px solid var(--border)",
            }}
          >
            <div className="flex flex-wrap items-center justify-between gap-2 text-[13px]">
              <span className="flex items-center gap-1.5 font-medium">
                <KeyRound className="h-3.5 w-3.5" />
                DeepL API Key
              </span>
              <div className="flex items-center gap-2">
                <span style={{ color: "var(--text-secondary)" }}>
                  {config?.has_deepl_key
                    ? t("keySavedPreview", { preview: config.deepl_preview || "" })
                    : t("keyMissing")}
                </span>
                {config?.has_deepl_key && (
                  <button
                    type="button"
                    disabled={isBusy}
                    onClick={() => void handleClearSavedKey()}
                    className="inline-flex items-center gap-1 rounded-lg px-2 py-0.5 text-[11.5px] font-medium transition hover:opacity-85 disabled:opacity-50"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                    }}
                  >
                    {clearingKey ? (
                      <Loader2 className="h-3 w-3 animate-spin" />
                    ) : (
                      <Trash2 className="h-3 w-3" />
                    )}
                    <span>{t("translateClearKeyBtn")}</span>
                  </button>
                )}
              </div>
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

          {error && (
            <div
              role="alert"
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
              <Check className="h-4 w-4 shrink-0" />
              <span>{savedNotice}</span>
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
              <Check className="h-4 w-4 shrink-0" />
              <span>{cacheNotice}</span>
            </div>
          )}

          <div className="flex flex-wrap items-center justify-between gap-2 pt-1">
            <button
              type="button"
              disabled={isBusy}
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
                disabled={saving || clearingKey}
                onClick={handleRequestClose}
                className="rounded-xl px-4 py-2 text-[14px] font-medium disabled:opacity-50"
                style={{
                  background: "var(--bg-surface)",
                  border: "1px solid var(--border)",
                }}
              >
                {t("close")}
              </button>
              <button
                type="submit"
                disabled={saving || clearingKey}
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
