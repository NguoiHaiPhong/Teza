import { useEffect, useState } from "react";
import {
  Calendar,
  Check,
  Clock,
  Copy,
  Image as ImageIcon,
  MessageCircle,
  MessageSquare,
  Phone,
  RefreshCw,
  User,
  Users,
  X,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { UserProfileDto } from "../types";
import { copyTextReliable } from "../utils/clipboard";
import { formatThreadTime } from "../utils/threadState";
import { RelayImage } from "./RelayImage";
import { ThreadAvatar } from "./ThreadAvatar";

interface Props {
  userId: string | null;
  initialProfile?: UserProfileDto | null;
  onClose: () => void;
  onStartChat: (profile: UserProfileDto) => void;
  onOpenGroup?: (groupId: string) => void;
  onOpenImage: (url: string, title: string) => void;
}

export function UserProfileModal({
  userId,
  initialProfile = null,
  onClose,
  onStartChat,
  onOpenGroup,
  onOpenImage,
}: Props) {
  const { lang, t } = useI18n();
  const [profile, setProfile] = useState<UserProfileDto | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [copiedPhone, setCopiedPhone] = useState(false);

  useEffect(() => {
    if (!userId) {
      setProfile(null);
      setError(null);
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);

    let cancelled = false;
    setError(null);
    setCopied(false);
    setCopiedPhone(false);

    const hasInitial = Boolean(initialProfile && initialProfile.uid === userId);
    if (hasInitial && initialProfile) {
      setProfile(initialProfile);
      setLoading(false);
    } else {
      setProfile(null);
      setLoading(true);
    }

    void bridge
      .getUserProfile(userId)
      .then((res) => {
        if (!cancelled) setProfile(res);
      })
      .catch((err) => {
        if (!cancelled && !hasInitial) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [userId, initialProfile, onClose]);

  if (!userId) return null;

  const genderLabel =
    profile?.gender === 0 ? t("genderMale") : profile?.gender === 1 ? t("genderFemale") : null;

  const handleCopyUid = () => {
    const uid = profile?.uid || userId;
    void copyTextReliable(uid).catch(() => undefined);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const handleCopyPhone = (phone: string) => {
    void copyTextReliable(phone).catch(() => undefined);
    setCopiedPhone(true);
    setTimeout(() => setCopiedPhone(false), 1500);
  };

  const mutualGroups = profile?.mutual_groups ?? [];
  const msgCount = profile?.local_message_count ?? 0;
  const mediaCount = profile?.local_media_count ?? 0;
  const lastInteractionTs = profile?.last_interaction_ts ?? 0;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
      onClick={onClose}
    >
      <div
        className="relative flex max-h-[86vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          onClick={onClose}
          title={t("close")}
          className="absolute right-3 top-3 z-30 rounded-full bg-black/45 p-1.5 text-white transition hover:bg-black/65"
        >
          <X className="h-4 w-4" />
        </button>

        <div className="min-h-0 flex-1 overflow-y-auto">
          <div
            className="relative z-0 h-32 w-full overflow-hidden"
            style={{
              background: "linear-gradient(135deg, rgba(56,189,248,0.35), rgba(99,102,241,0.35))",
            }}
          >
            {profile?.cover && /^https:\/\//i.test(profile.cover) && (
              <button
                type="button"
                onClick={() => onOpenImage(profile.cover, profile.display_name)}
                title={t("viewCoverTitle")}
                className="block h-full w-full"
              >
                <RelayImage
                  src={profile.cover}
                  alt={t("viewCoverTitle")}
                  referrerPolicy="no-referrer"
                  className="h-full w-full object-cover transition hover:brightness-95"
                />
              </button>
            )}
          </div>

          <div className="px-5 pb-5">
            <div className="relative z-10 -mt-10 flex items-end justify-between">
            <button
              type="button"
              disabled={!profile?.avatar || !/^https:\/\//i.test(profile.avatar)}
              onClick={() => {
                if (profile?.avatar && /^https:\/\//i.test(profile.avatar)) {
                  onOpenImage(profile.avatar, profile.display_name);
                }
              }}
              className="relative z-10 rounded-full p-0.5 ring-4 shadow-md"
              style={{
                background: "var(--bg-sidebar)",
                ["--tw-ring-color" as string]: "var(--bg-sidebar)",
              }}
              title={profile?.avatar ? t("viewAvatarTitle") : undefined}
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
                className="relative z-10 mb-1 rounded-full px-2.5 py-0.5 text-[11px] font-medium"
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
                  ? t("badgeSelfAccount")
                  : profile.is_friend === true
                    ? t("badgeFriend")
                    : profile.is_friend === false
                      ? t("badgeStranger")
                      : t("badgeAccount")}
              </span>
            )}
          </div>

          {loading ? (
            <div className="flex items-center justify-center gap-2 py-8 text-[13px]" style={{ color: "var(--text-secondary)" }}>
              <RefreshCw className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
              <span>{t("lookingUpProfile")}</span>
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
                  <h3 className="selectable-text text-[17px] font-bold">{profile.display_name}</h3>
                  {profile.zalo_name && profile.zalo_name !== profile.display_name && (
                    <div className="selectable-text text-[13px]" style={{ color: "var(--text-secondary)" }}>
                      {t("originalNameLabel", { name: profile.zalo_name })}
                    </div>
                  )}
                  {profile.status && (
                    <p
                      className="selectable-text mt-2 rounded-xl px-3 py-2 text-[13px] italic"
                      style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                    >
                      "{profile.status}"
                    </p>
                  )}
                </div>

                <div
                  className="selectable-text mt-4 space-y-2 rounded-xl p-3 text-[13px]"
                  style={{ background: "var(--bg-surface)" }}
                >
                  {genderLabel && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <User className="h-3.5 w-3.5" /> {t("genderLabel")}
                      </span>
                      <span className="font-medium">{genderLabel}</span>
                    </div>
                  )}
                  {profile.sdob && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <Calendar className="h-3.5 w-3.5" /> {t("dobLabel")}
                      </span>
                      <span className="font-medium">{profile.sdob}</span>
                    </div>
                  )}
                  {profile.phone_number && (
                    <div className="flex items-center justify-between gap-3">
                      <span className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                        <Phone className="h-3.5 w-3.5" /> {t("phoneLabel")}
                      </span>
                      <button
                        type="button"
                        onClick={() => handleCopyPhone(profile.phone_number)}
                        title={t("copyPhoneTitle")}
                        className="flex items-center gap-1 font-medium hover:opacity-80"
                      >
                        <span>{profile.phone_number}</span>
                        {copiedPhone ? (
                          <Check className="h-3.5 w-3.5 text-emerald-400" />
                        ) : (
                          <Copy className="h-3.5 w-3.5 opacity-60" />
                        )}
                      </button>
                    </div>
                  )}
                  <div className="flex items-center justify-between gap-3">
                    <span style={{ color: "var(--text-secondary)" }}>UID</span>
                    <button
                      type="button"
                      onClick={handleCopyUid}
                      className="flex items-center gap-1 font-mono text-xs hover:opacity-80"
                      title={t("copyUidTitle")}
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

                {!profile.is_self && (msgCount > 0 || mediaCount > 0 || lastInteractionTs > 0) && (
                  <div
                    className="mt-3 grid grid-cols-3 gap-2 rounded-xl p-2.5 text-center text-xs"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    <div>
                      <div className="flex items-center justify-center gap-1 font-semibold" style={{ color: "var(--accent)" }}>
                        <MessageCircle className="h-3.5 w-3.5" />
                        <span>{msgCount}</span>
                      </div>
                      <div className="mt-0.5 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {t("statsSavedMsgs")}
                      </div>
                    </div>
                    <div>
                      <div className="flex items-center justify-center gap-1 font-semibold" style={{ color: "var(--accent)" }}>
                        <ImageIcon className="h-3.5 w-3.5" />
                        <span>{mediaCount}</span>
                      </div>
                      <div className="mt-0.5 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {t("statsMediaFiles")}
                      </div>
                    </div>
                    <div>
                      <div className="flex items-center justify-center gap-1 font-semibold" style={{ color: "var(--accent)" }}>
                        <Clock className="h-3.5 w-3.5" />
                        <span className="truncate">
                          {lastInteractionTs > 0 ? formatThreadTime(lastInteractionTs, lang) : "—"}
                        </span>
                      </div>
                      <div className="mt-0.5 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                        {t("statsRecent")}
                      </div>
                    </div>
                  </div>
                )}

                {!profile.is_self && (
                  <div
                    className="mt-3 rounded-xl p-3"
                    style={{ background: "var(--bg-surface)" }}
                  >
                    <div
                      className="mb-2 flex items-center gap-1.5 text-xs font-semibold"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <Users className="h-3.5 w-3.5" />
                      <span>{t("mutualGroupsLabel", { count: mutualGroups.length })}</span>
                    </div>
                    {mutualGroups.length === 0 ? (
                      <div className="text-xs opacity-65">{t("noMutualGroups")}</div>
                    ) : (
                      <div className="max-h-36 space-y-1.5 overflow-y-auto pr-0.5">
                        {mutualGroups.map((g) => (
                          <button
                            key={g.id}
                            type="button"
                            onClick={() => {
                              if (onOpenGroup) {
                                onOpenGroup(g.id);
                                onClose();
                              }
                            }}
                            className="hover-menu flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left transition"
                          >
                            <ThreadAvatar avatar={g.avatar} title={g.title} kind="group" size="sm" />
                            <div className="min-w-0 flex-1">
                              <div className="truncate text-[13px] font-medium">{g.title}</div>
                              {g.total_members !== null && (
                                <div className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                                  {t("groupMembersCount", { count: g.total_members })}
                                </div>
                              )}
                            </div>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                )}

                {!profile.is_self && (
                  <button
                    type="button"
                    onClick={() => {
                      onStartChat(profile);
                      onClose();
                    }}
                    className="tf-btn mt-4 flex w-full items-center justify-center gap-2 rounded-xl py-2.5 text-[14px] font-semibold"
                  >
                    <MessageSquare className="h-4 w-4" /> {t("startChatNowBtn")}
                  </button>
                )}
              </>
            )
          )}
          </div>
        </div>
      </div>
    </div>
  );
}
