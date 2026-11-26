import { useEffect, useState } from "react";
import { BarChart3, Loader2, Plus, Trash2, X } from "lucide-react";
import { useI18n } from "../i18n";
import { isImeComposingEvent } from "../utils/searchNormalize";

interface Props {
  open: boolean;
  groupTitle: string;
  onClose: () => void;
  onSubmit: (payload: {
    question: string;
    options: string[];
    expiredTimeMs: number;
    allowMulti: boolean;
    allowAddOption: boolean;
    isAnonymous: boolean;
  }) => Promise<void>;
}

export function CreatePollModal({ open, groupTitle, onClose, onSubmit }: Props) {
  const { t } = useI18n();
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState<string[]>(["", ""]);
  const [allowMulti, setAllowMulti] = useState(true);
  const [allowAddOption, setAllowAddOption] = useState(true);
  const [isAnonymous, setIsAnonymous] = useState(false);
  const [expiryHours, setExpiryHours] = useState<number>(0);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setQuestion("");
    setOptions(["", ""]);
    setAllowMulti(true);
    setAllowAddOption(true);
    setIsAnonymous(false);
    setExpiryHours(0);
    setError(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isImeComposingEvent(e)) return;
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const cleanOptions = options.map((o) => o.trim()).filter(Boolean);

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!question.trim()) {
      setError(t("pollErrorEmptyQuestion"));
      return;
    }
    if (cleanOptions.length < 2) {
      setError(t("pollErrorMinOptions"));
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const expiredTimeMs = expiryHours > 0 ? Date.now() + expiryHours * 3600 * 1000 : 0;
      await onSubmit({
        question: question.trim(),
        options: cleanOptions,
        expiredTimeMs,
        allowMulti,
        allowAddOption,
        isAnonymous,
      });
      onClose();
    } catch (err) {
      setError(String(err).replace(/^Error:\s*/i, ""));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <form
        onSubmit={(e) => void handleSave(e)}
        onKeyDown={(e) => {
          if (isImeComposingEvent(e) && e.key === "Enter") {
            e.preventDefault();
          }
        }}
        className="flex max-h-[88vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between px-5 py-3.5"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2 font-semibold">
            <BarChart3 className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
            <span className="truncate text-[15.5px]">{t("createPollModalTitle", { title: groupTitle })}</span>
          </div>
          <button type="button" onClick={onClose} className="hover-menu rounded-lg p-1.5">
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="min-h-0 flex-1 space-y-3.5 overflow-y-auto p-5">
          <div>
            <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              {t("pollQuestionLabel")}
            </label>
            <input
              autoFocus
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              placeholder={t("pollQuestionPlaceholder")}
              className="w-full rounded-xl px-3 py-2 text-sm outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            />
          </div>

          <div className="space-y-2">
            <label className="block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              {t("pollOptionsCountLabel", { count: cleanOptions.length })}
            </label>
            {options.map((opt, idx) => (
              <div key={idx} className="flex items-center gap-2">
                <input
                  value={opt}
                  onChange={(e) => {
                    const next = [...options];
                    next[idx] = e.target.value;
                    setOptions(next);
                  }}
                  placeholder={t("pollOptionPlaceholder", { idx: idx + 1 })}
                  className="min-w-0 flex-1 rounded-xl px-3 py-1.5 text-sm outline-none"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                />
                {options.length > 2 && (
                  <button
                    type="button"
                    onClick={() => setOptions((prev) => prev.filter((_, i) => i !== idx))}
                    className="hover-menu rounded-lg p-1.5 text-rose-400"
                    title={t("removeOptionTitle")}
                  >
                    <Trash2 className="h-4 w-4" />
                  </button>
                )}
              </div>
            ))}
            {options.length < 10 && (
              <button
                type="button"
                onClick={() => setOptions((prev) => [...prev, ""])}
                className="hover-menu inline-flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold"
                style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
              >
                <Plus className="h-3.5 w-3.5" />
                <span>{t("addOptionBtn")}</span>
              </button>
            )}
          </div>

          <div className="space-y-2 rounded-xl p-3" style={{ background: "var(--bg-surface)" }}>
            <label className="flex cursor-pointer items-center justify-between gap-2 text-xs">
              <span>{t("pollAllowMultiLabel")}</span>
              <input
                type="checkbox"
                checked={allowMulti}
                onChange={(e) => setAllowMulti(e.target.checked)}
                className="h-4 w-4 accent-[var(--accent)]"
              />
            </label>
            <label className="flex cursor-pointer items-center justify-between gap-2 text-xs">
              <span>{t("pollAllowAddOptionLabel")}</span>
              <input
                type="checkbox"
                checked={allowAddOption}
                onChange={(e) => setAllowAddOption(e.target.checked)}
                className="h-4 w-4 accent-[var(--accent)]"
              />
            </label>
            <label className="flex cursor-pointer items-center justify-between gap-2 text-xs">
              <span>{t("pollAnonymousLabel")}</span>
              <input
                type="checkbox"
                checked={isAnonymous}
                onChange={(e) => setIsAnonymous(e.target.checked)}
                className="h-4 w-4 accent-[var(--accent)]"
              />
            </label>
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              {t("pollExpiryLabel")}
            </label>
            <select
              value={expiryHours}
              onChange={(e) => setExpiryHours(Number(e.target.value) || 0)}
              className="w-full rounded-xl px-3 py-2 text-xs outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              <option value={0}>{t("pollExpiryUnlimited")}</option>
              <option value={1}>{t("pollExpiry1h")}</option>
              <option value={24}>{t("pollExpiry24h")}</option>
              <option value={168}>{t("pollExpiry7d")}</option>
            </select>
          </div>

          {error && (
            <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300">
              {error}
            </div>
          )}
        </div>

        <div
          className="flex items-center justify-end gap-2 px-5 py-3"
          style={{ borderTop: "1px solid var(--border-subtle)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-xl px-4 py-2 text-xs font-medium"
            style={{ background: "var(--bg-surface)" }}
          >
            {t("cancel")}
          </button>
          <button
            type="submit"
            disabled={saving || !question.trim() || cleanOptions.length < 2}
            className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
          >
            {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
            <span>{t("createPollBtn")}</span>
          </button>
        </div>
      </form>
    </div>
  );
}
