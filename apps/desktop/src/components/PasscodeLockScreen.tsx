import { useEffect, useRef, useState } from "react";
import { Eye, EyeOff, Lock, LogOut } from "lucide-react";
import { useI18n } from "../i18n";

interface Props {
  open: boolean;
  myName?: string;
  onUnlock: (pin: string) => Promise<boolean>;
  onLogout: () => void;
}

export function PasscodeLockScreen({ open, myName, onUnlock, onLogout }: Props) {
  const { t } = useI18n();
  const [pin, setPin] = useState("");
  const [showPin, setShowPin] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!open) {
      setPin("");
      setError(null);
      setShowPin(false);
      setConfirmLogout(false);
      return;
    }
    const timer = window.setTimeout(() => inputRef.current?.focus(), 30);
    return () => window.clearTimeout(timer);
  }, [open]);

  if (!open) return null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!pin || checking) return;
    setChecking(true);
    setError(null);
    try {
      const ok = await onUnlock(pin);
      if (!ok) {
        setError(t("lockScreenError"));
        inputRef.current?.select();
      } else {
        setPin("");
      }
    } finally {
      setChecking(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center p-4"
      style={{ background: "var(--bg-stage)", color: "var(--text-primary)" }}
    >
      <div
        className="w-full max-w-sm rounded-3xl p-6 shadow-2xl"
        style={{
          background: "var(--bg-elevated)",
          border: "1px solid var(--border-subtle)",
        }}
      >
        <div className="mb-5 flex flex-col items-center text-center">
          <div
            className="mb-3 flex h-14 w-14 items-center justify-center rounded-2xl"
            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
          >
            <Lock className="h-7 w-7" />
          </div>
          <h2 className="text-lg font-semibold">{t("lockScreenTitle")}</h2>
          <p className="mt-1 text-xs" style={{ color: "var(--text-secondary)" }}>
            {myName ? t("lockScreenAccount", { name: myName }) : t("lockScreenPrompt")}
          </p>
        </div>

        <form onSubmit={(e) => void submit(e)} className="space-y-3">
          <div
            className="flex items-center gap-2 rounded-xl px-3 py-2"
            style={{
              background: "var(--bg-surface)",
              border: `1px solid ${error ? "rgba(244,63,94,0.6)" : "var(--border-subtle)"}`,
            }}
          >
            <input
              ref={inputRef}
              type={showPin ? "text" : "password"}
              value={pin}
              onChange={(e) => {
                setPin(e.target.value);
                if (error) setError(null);
              }}
              placeholder={t("lockScreenPlaceholder")}
              autoComplete="off"
              className="min-w-0 flex-1 bg-transparent text-sm tracking-wider outline-none"
            />
            <button
              type="button"
              onClick={() => setShowPin((v) => !v)}
              className="rounded-lg p-1 opacity-70 transition hover:opacity-100"
            >
              {showPin ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
            </button>
          </div>

          {error && (
            <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-xs text-rose-300">
              {error}
            </div>
          )}

          <button
            type="submit"
            disabled={!pin || checking}
            className="w-full rounded-xl py-2.5 text-sm font-semibold transition disabled:opacity-50"
            style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
          >
            {checking ? t("lockScreenChecking") : t("lockScreenUnlock")}
          </button>
        </form>

        <div className="mt-5 border-t pt-3 text-center" style={{ borderColor: "var(--border-subtle)" }}>
          {!confirmLogout ? (
            <button
              type="button"
              onClick={() => setConfirmLogout(true)}
              className="inline-flex items-center gap-1.5 text-xs opacity-70 transition hover:opacity-100"
              style={{ color: "var(--text-secondary)" }}
            >
              <LogOut className="h-3.5 w-3.5" />
              <span>{t("lockScreenForgot")}</span>
            </button>
          ) : (
            <div className="flex items-center justify-center gap-2 text-xs">
              <span style={{ color: "var(--text-secondary)" }}>{t("lockScreenConfirmLogout")}</span>
              <button
                type="button"
                onClick={onLogout}
                className="rounded-lg bg-rose-500/20 px-2.5 py-1 font-medium text-rose-300 hover:bg-rose-500/30"
              >
                {t("logoutMenu")}
              </button>
              <button
                type="button"
                onClick={() => setConfirmLogout(false)}
                className="rounded-lg px-2 py-1 opacity-80 hover:opacity-100"
              >
                {t("cancel")}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
