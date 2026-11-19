import { useEffect, useState } from "react";
import { Calendar, Check, Copy, MessageSquare, Phone, RefreshCw, User, X } from "lucide-react";
import { bridge } from "../bridge";
import type { UserProfileDto } from "../types";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  userId: string | null;
  onClose: () => void;
  onStartChat: (profile: UserProfileDto) => void;
  onOpenImage: (url: string, title: string) => void;
}

export function UserProfileModal({ userId, onClose, onStartChat, onOpenImage }: Props) {
  const [profile, setProfile] = useState<UserProfileDto | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!userId) {
      setProfile(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    setCopied(false);
    void bridge
      .getUserProfile(userId)
      .then((res) => {
        if (!cancelled) setProfile(res);
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      cancelled = true;
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [userId, onClose]);

  if (!userId) return null;

  const genderLabel =
    profile?.gender === 0 ? "Nam" : profile?.gender === 1 ? "Nữ" : null;

  const handleCopyUid = () => {
    const uid = profile?.uid || userId;
    void navigator.clipboard?.writeText(uid);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="relative w-full max-w-sm overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div
          className="relative h-28 w-full overflow-hidden"
          style={{
            background: "linear-gradient(135deg, rgba(56,189,248,0.35), rgba(99,102,241,0.35))",
          }}
        >
          {profile?.cover && /^https:\/\//i.test(profile.cover) && (
            <img
              src={profile.cover}
              alt="Ảnh bìa"
              referrerPolicy="no-referrer"
              className="h-full w-full object-cover"
            />
          )}
          <button
            type="button"
            onClick={onClose}
            title="Đóng (Esc)"
            className="absolute right-3 top-3 rounded-full bg-black/45 p-1.5 text-white hover:bg-black/65"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="px-5 pb-5">
          <div className="-mt-10 flex items-end justify-between">
            <button
              type="button"
              disabled={!profile?.avatar || !/^https:\/\//i.test(profile.avatar)}
              onClick={() => {
                if (profile?.avatar && /^https:\/\//i.test(profile.avatar)) {
                  onOpenImage(profile.avatar, profile.display_name);
                }
              }}
              className="rounded-full ring-4"
              style={{ borderColor: "var(--bg-sidebar)", ["--tw-ring-color" as string]: "var(--bg-sidebar)" }}
              title={profile?.avatar ? "Xem ảnh đại diện" : undefined}
            >
              <ThreadAvatar
                avatar={profile?.avatar || ""}
                title={profile?.display_name || userId}
                kind="direct"
                size="xl"
              />
            </button>

            {profile && (
              <span
                className="rounded-full px-2.5 py-0.5 text-[11px] font-medium"
                style={{
                  background: "var(--bg-surface)",
                  color: profile.is_self
                    ? "var(--accent)"
                    : profile.is_friend
                      ? "#34d399"
                      : "var(--text-secondary)",
                }}
              >
                {profile.is_self
                  ? "Tài khoản của bạn"
                  : profile.is_friend === true
                    ? "Bạn bè"
                    : profile.is_friend === false
                      ? "Người lạ"
                      : "Tài khoản"}
              </span>
            )}
          </div>

          {loading ? (
            <div className="flex items-center justify-center gap-2 py-8 text-[13px]" style={{ color: "var(--text-secondary)" }}>
              <RefreshCw className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
              <span>Đang tra cứu thông tin tài khoản...</span>
            </div>
          ) : error ? (
            <div
              className="mt-4 rounded-xl p-3 text-[13px]"
              style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
            >
              {error}
            </div>
          ) : (
            profile && (
              <>
                <div className="mt-3">
                  <h3 className="text-[17px] font-bold">{profile.display_name}</h3>
                  {profile.zalo_name && profile.zalo_name !== profile.display_name && (
                    <div className="text-[13px]" style={{ color: "var(--text-secondary)" }}>
                      Tên gốc: {profile.zalo_name}
                    </div>
                  )}
                  {profile.status && (
                    <p
                      className="mt-2 rounded-xl px-3 py-2 text-[13px] italic"
                      style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                    >
                      "{profile.status}"
                    </p>
                  )}
                </div>

                <div
                  className="mt-4 space-y-2 rounded-xl p-3 text-[13px]"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {genderLabel && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <User className="h-3.5 w-3.5" /> Giới tính
                      </span>
                      <span className="font-medium">{genderLabel}</span>
                    </div>
                  )}
                  {profile.sdob && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <Calendar className="h-3.5 w-3.5" /> Ngày sinh
                      </span>
                      <span className="font-medium">{profile.sdob}</span>
                    </div>
                  )}
                  {profile.phone_number && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <Phone className="h-3.5 w-3.5" /> Điện thoại
                      </span>
                      <span className="font-medium">{profile.phone_number}</span>
                    </div>
                  )}
                  <div className="flex items-center justify-between gap-3">
                    <span style={{ color: "var(--text-secondary)" }}>UID</span>
                    <button
                      type="button"
                      onClick={handleCopyUid}
                      className="flex items-center gap-1 font-mono text-xs hover:opacity-80"
                      title="Sao chép UID"
                    >
                      <span>{profile.uid}</span>
                      {copied ? (
                        <Check className="h-3.5 w-3.5 text-emerald-400" />
                      ) : (
                        <Copy className="h-3.5 w-3.5 opacity-70" />
                      )}
                    </button>
                  </div>
                </div>

                {!profile.is_self && (
                  <button
                    type="button"
                    onClick={() => {
                      onStartChat(profile);
                      onClose();
                    }}
                    className="tf-btn mt-4 flex w-full items-center justify-center gap-2 rounded-xl py-2.5 text-[14px] font-semibold"
                  >
                    <MessageSquare className="h-4 w-4" /> Nhắn tin ngay
                  </button>
                )}
              </>
            )
          )}
        </div>
      </div>
    </div>
  );
}
