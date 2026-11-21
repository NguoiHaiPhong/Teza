import { useEffect, useState } from "react";
import { CalendarClock, Loader2, X } from "lucide-react";
import { useI18n } from "../i18n";

interface Props {
  open: boolean;
  threadTitle: string;
  onClose: () => void;
  onSubmit: (title: string, startTimeMs: number, repeat: number) => Promise<void>;
}

function toLocalDatetimeInputValue(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function CreateReminderModal({ open, threadTitle, onClose, onSubmit }: Props) {
  const { t } = useI18n();
  const [title, setTitle] = useState("");
  const [dateTimeStr, setDateTimeStr] = useState(() =>
    toLocalDatetimeInputValue(Date.now() + 30 * 60_000),
  );
  const [repeat, setRepeat] = useState<number>(0);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setTitle("");
    setDateTimeStr(toLocalDatetimeInputValue(Date.now() + 30 * 60_000));
    setRepeat(0);
    setError(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const setPreset = (offsetMs?: number, tomorrow9Am = false) => {
    if (tomorrow9Am) {
      const d = new Date();
      d.setDate(d.getDate() + 1);
      d.setHours(9, 0, 0, 0);
      setDateTimeStr(toLocalDatetimeInputValue(d.getTime()));
    } else if (offsetMs) {
      setDateTimeStr(toLocalDatetimeInputValue(Date.now() + offsetMs));
    }
  };

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    const clean = title.trim();
    if (!clean) {
      setError("Vui lòng nhập nội dung nhắc hẹn");
      return;
    }
    const parsedMs = new Date(dateTimeStr).getTime();
    if (!Number.isFinite(parsedMs) || parsedMs <= 0) {
      setError("Thời gian nhắc hẹn không hợp lệ");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await onSubmit(clean, parsedMs, repeat);
      onClose();
    } catch (err) {
      setError(String(err));
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
        className="w-full max-w-md overflow-hidden rounded-2xl shadow-2xl"
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
            <CalendarClock className="h-5 w-5 shrink-0" style={{ color: "var(--accent)" }} />
            <span className="truncate text-[15.5px]">{t("createReminderModalTitle", { title: threadTitle })}</span>
          </div>
          <button type="button" onClick={onClose} className="hover-menu rounded-lg p-1.5">
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <div className="space-y-3.5 p-5">
          <div>
            <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              {t("reminderContentLabel")}
            </label>
            <input
              autoFocus
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder={t("reminderContentPlaceholder")}
              className="w-full rounded-xl px-3 py-2 text-sm outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            />
          </div>

          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <label className="text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
                {t("reminderTimeLabel")}
              </label>
              <div className="flex gap-1">
                <button
                  type="button"
                  onClick={() => setPreset(15 * 60_000)}
                  className="hover-menu rounded-md px-2 py-0.5 text-[11px] font-medium"
                  style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
                >
                  {t("preset15m")}
                </button>
                <button
                  type="button"
                  onClick={() => setPreset(60 * 60_000)}
                  className="hover-menu rounded-md px-2 py-0.5 text-[11px] font-medium"
                  style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
                >
                  {t("preset1h")}
                </button>
                <button
                  type="button"
                  onClick={() => setPreset(undefined, true)}
                  className="hover-menu rounded-md px-2 py-0.5 text-[11px] font-medium"
                  style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
                >
                  {t("presetTomorrow9")}
                </button>
              </div>
            </div>
            <input
              type="datetime-local"
              value={dateTimeStr}
              onChange={(e) => setDateTimeStr(e.target.value)}
              className="w-full rounded-xl px-3 py-2 text-sm outline-none"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            />
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium" style={{ color: "var(--text-secondary)" }}>
              {t("reminderRepeatLabel")}
            </label>
            <div className="grid grid-cols-4 gap-1.5">
              {[
                { val: 0, label: t("repeatOnce") },
                { val: 1, label: t("repeatDaily") },
                { val: 2, label: t("repeatWeekly") },
                { val: 3, label: t("repeatMonthly") },
              ].map((opt) => (
                <button
                  key={opt.val}
                  type="button"
                  onClick={() => setRepeat(opt.val)}
                  className="rounded-lg px-2 py-1.5 text-xs font-medium transition"
                  style={{
                    background: repeat === opt.val ? "var(--bg-active)" : "var(--bg-surface)",
                    color: repeat === opt.val ? "var(--text-on-active)" : "var(--text-primary)",
                  }}
                >
                  {opt.label}
                </button>
              ))}
            </div>
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
            disabled={saving || !title.trim()}
            className="tf-btn flex items-center gap-1.5 rounded-xl px-4 py-2 text-xs font-semibold disabled:opacity-50"
          >
            {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
            <span>{t("createReminderBtn")}</span>
          </button>
        </div>
      </form>
    </div>
  );
}
