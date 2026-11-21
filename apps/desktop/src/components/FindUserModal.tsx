import { useEffect, useRef, useState } from "react";
import { Loader2, MessageSquare, Phone, Search, User, X } from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { UserProfileDto } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  open: boolean;
  initialPhone?: string;
  onClose: () => void;
  onOpenProfile: (profile: UserProfileDto) => void;
  onStartChat: (profile: UserProfileDto) => void;
}

export function FindUserModal({
  open,
  initialPhone = "",
  onClose,
  onOpenProfile,
  onStartChat,
}: Props) {
  const { t } = useI18n();
  const [phone, setPhone] = useState("");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<UserProfileDto | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  const runSearch = async (raw: string) => {
    const query = raw.trim();
    if (!query) {
      setError("Vui lòng nhập số điện thoại cần tìm");
      setResult(null);
      return;
    }
    setSearching(true);
    setError(null);
    setResult(null);
    try {
      const found = await bridge.findUserByPhone(query);
      setResult(found);
    } catch (err) {
      setError(String(err).replace(/^Error:\s*/i, ""));
    } finally {
      setSearching(false);
    }
  };

  useEffect(() => {
    if (!open) {
      setPhone("");
      setError(null);
      setResult(null);
      setSearching(false);
      return;
    }
    const init = initialPhone.trim();
    setPhone(init);
    setError(null);
    setResult(null);
    setTimeout(() => inputRef.current?.focus(), 20);
    if (init) {
      void runSearch(init);
    }
  }, [open, initialPhone]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="w-full max-w-md overflow-hidden rounded-2xl shadow-2xl"
        style={{
          background: "var(--bg-sidebar)",
          border: "1px solid var(--border-subtle)",
          color: "var(--text-primary)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          <div className="flex items-center gap-2 font-semibold">
            <Phone className="h-4 w-4" style={{ color: "var(--accent)" }} />
            <span>{t("findNewFriendMenu")}</span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="hover-menu rounded-lg p-1"
            title={t("close")}
          >
            <X className="h-4 w-4" style={{ color: "var(--text-secondary)" }} />
          </button>
        </div>

        <form
          onSubmit={(e) => {
            e.preventDefault();
            void runSearch(phone);
          }}
          className="space-y-3 p-4"
        >
          <div className="flex items-center gap-2">
            <div
              className="flex flex-1 items-center gap-2 rounded-xl px-3 py-2"
              style={{ background: "var(--bg-surface)" }}
            >
              <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
              <input
                ref={inputRef}
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
                placeholder={t("findUserPhonePlaceholder")}
                className="w-full bg-transparent text-[14.5px] outline-none"
              />
            </div>
            <button
              type="submit"
              disabled={searching}
              className="tf-btn flex shrink-0 items-center gap-1.5 rounded-xl px-4 py-2 text-[14px] font-medium disabled:opacity-50"
            >
              {searching ? <Loader2 className="h-4 w-4 animate-spin" /> : t("findShortBtn")}
            </button>
          </div>

          {error && (
            <div className="rounded-xl bg-rose-500/15 px-3 py-2 text-[13px] text-rose-300">
              {error}
            </div>
          )}

          {result && (
            <div
              className="rounded-xl p-3.5"
              style={{ background: "var(--bg-surface)", border: "1px solid var(--border-subtle)" }}
            >
              <div className="flex items-center gap-3">
                <ThreadAvatar
                  avatar={result.avatar}
                  title={result.display_name}
                  kind="direct"
                  size="md"
                />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-[15px] font-semibold">
                      {result.display_name}
                    </span>
                    <span
                      className="shrink-0 rounded-full px-2 py-0.5 text-[11px] font-medium"
                      style={{
                        background: result.is_friend ? "var(--accent-soft)" : "var(--bg-hover)",
                        color: result.is_friend ? "var(--accent)" : "var(--text-secondary)",
                      }}
                    >
                      {result.is_self
                        ? t("badgeSelfAccount")
                        : result.is_friend
                          ? t("badgeFriend")
                          : t("badgeNotFriend")}
                    </span>
                  </div>
                  {result.phone_number && (
                    <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                      {result.phone_number}
                    </div>
                  )}
                </div>
              </div>

              <div className="mt-3 flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => {
                    onOpenProfile(result);
                    onClose();
                  }}
                  className="hover-menu flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[13.5px] font-medium transition"
                  style={{
                    background: "var(--bg-sidebar)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <User className="h-4 w-4" />
                  <span>{t("viewProfileBtn")}</span>
                </button>
                {!result.is_self && (
                  <button
                    type="button"
                    onClick={() => {
                      onStartChat(result);
                      onClose();
                    }}
                    className="tf-btn flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-[13.5px] font-semibold"
                  >
                    <MessageSquare className="h-4 w-4" />
                    <span>{t("startChatBtn")}</span>
                  </button>
                )}
              </div>
            </div>
          )}
        </form>
      </div>
    </div>
  );
}
