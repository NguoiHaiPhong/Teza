import { useEffect, useRef, useState } from "react";
import {
  Ban,
  Calendar,
  Check,
  Clock,
  Copy,
  Image as ImageIcon,
  Loader2,
  MessageCircle,
  MessageSquare,
  Phone,
  RefreshCw,
  Send,
  ShieldCheck,
  Undo2,
  User,
  UserMinus,
  UserPlus,
  Users,
  X,
} from "lucide-react";
import { bridge } from "../bridge";
import { useI18n } from "../i18n";
import type { UserProfileDto } from "../types";
import { copyTextReliable } from "../utils/clipboard";
import { isImeComposingEvent } from "../utils/searchNormalize";
import { formatThreadTime } from "../utils/threadState";
import { RelayImage } from "./RelayImage";
import { ThreadAvatar } from "./ThreadAvatar";

export interface UserProfileModalProps {
  userId: string | null;
  isLocked?: boolean;
  initialProfile?: UserProfileDto | null;
  onClose: () => void;
  onStartChat: (profile: UserProfileDto) => void;
  onOpenGroup?: (groupId: string) => void;
  onOpenImage: (url: string, title: string) => void;
  onRelationshipChanged?: () => void;
}

type PendingRequestState = { kind: "received" | "sent"; message: string } | null;
type ConfirmAction = "unfriend" | "block" | null;
type BusyActionKind = "send" | "accept" | "reject" | "undo" | "unfriend" | "block" | "unblock" | null;

function cleanError(err: unknown): string {
  return String(err).replace(/^Error:\s*/i, "").trim() || String(err);
}

export function UserProfileModal({
  userId,
  isLocked = false,
  initialProfile = null,
  onClose,
  onStartChat,
  onOpenGroup,
  onOpenImage,
  onRelationshipChanged,
}: UserProfileModalProps) {
  const { lang, t } = useI18n();
  const [profile, setProfile] = useState<UserProfileDto | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [copiedPhone, setCopiedPhone] = useState(false);
  const [pendingRequest, setPendingRequest] = useState<PendingRequestState>(null);
  const [confirmAction, setConfirmAction] = useState<ConfirmAction>(null);
  const [addFormOpen, setAddFormOpen] = useState(false);
  const [greetingText, setGreetingText] = useState("");
  const [actionBusyKind, setActionBusyKind] = useState<BusyActionKind>(null);
  const actionBusy = actionBusyKind !== null;
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionNotice, setActionNotice] = useState<string | null>(null);
  const copiedTimerRef = useRef<number | null>(null);
  const copiedPhoneTimerRef = useRef<number | null>(null);
  const relationshipMutatedRef = useRef(false);
  const mountedRef = useRef(true);
  const activeUserIdRef = useRef<string | null>(userId);
  activeUserIdRef.current = userId;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (copiedTimerRef.current !== null) {
        window.clearTimeout(copiedTimerRef.current);
        copiedTimerRef.current = null;
      }
      if (copiedPhoneTimerRef.current !== null) {
        window.clearTimeout(copiedPhoneTimerRef.current);
        copiedPhoneTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!userId || isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (isLocked || isImeComposingEvent(e)) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (actionBusy) return;
        if (confirmAction !== null) {
          setConfirmAction(null);
        } else if (addFormOpen) {
          setAddFormOpen(false);
        } else {
          onClose();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [userId, isLocked, actionBusy, confirmAction, addFormOpen, onClose]);

  useEffect(() => {
    if (!userId) {
      setProfile(null);
      setError(null);
      setPendingRequest(null);
      setConfirmAction(null);
      setAddFormOpen(false);
      setGreetingText("");
      setActionBusyKind(null);
      setActionError(null);
      setActionNotice(null);
      relationshipMutatedRef.current = false;
      return;
    }

    let cancelled = false;
    relationshipMutatedRef.current = false;
    setError(null);
    setCopied(false);
    setCopiedPhone(false);
    setPendingRequest(null);
    setConfirmAction(null);
    setAddFormOpen(false);
    setGreetingText("");
    setActionBusyKind(null);
    setActionError(null);
    setActionNotice(null);

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
        if (cancelled) return;
        setProfile((prev) => {
          if (relationshipMutatedRef.current && prev && prev.uid === res.uid) {
            return {
              ...res,
              is_friend: prev.is_friend,
              is_blocked: prev.is_blocked,
            };
          }
          return res;
        });
      })
      .catch((err) => {
        if (!cancelled && !hasInitial) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    if (!(hasInitial && initialProfile?.is_self)) {
      void bridge
        .friendListRequests()
        .then((list) => {
          if (cancelled || relationshipMutatedRef.current) return;
          const incoming = list.received.find((r) => r.user_id === userId);
          if (incoming) {
            setPendingRequest({ kind: "received", message: incoming.message || "" });
            return;
          }
          const outgoing = list.sent.find((r) => r.user_id === userId);
          if (outgoing) {
            setPendingRequest({ kind: "sent", message: outgoing.message || "" });
            return;
          }
          setPendingRequest(null);
        })
        .catch(() => {
          // Best-effort pending request status lookup
        });
    }

    return () => {
      cancelled = true;
    };
  }, [userId, initialProfile]);

  if (!userId) return null;

  const genderLabel =
    profile?.gender === 0 ? t("genderMale") : profile?.gender === 1 ? t("genderFemale") : null;

  const handleCopyUid = () => {
    const uid = profile?.uid || userId;
    void copyTextReliable(uid).catch(() => undefined);
    setCopied(true);
    if (copiedTimerRef.current !== null) {
      window.clearTimeout(copiedTimerRef.current);
    }
    copiedTimerRef.current = window.setTimeout(() => {
      copiedTimerRef.current = null;
      setCopied(false);
    }, 1500);
  };

  const handleCopyPhone = (phone: string) => {
    void copyTextReliable(phone).catch(() => undefined);
    setCopiedPhone(true);
    if (copiedPhoneTimerRef.current !== null) {
      window.clearTimeout(copiedPhoneTimerRef.current);
    }
    copiedPhoneTimerRef.current = window.setTimeout(() => {
      copiedPhoneTimerRef.current = null;
      setCopiedPhone(false);
    }, 1500);
  };

  const mutualGroups = profile?.mutual_groups ?? [];
  const msgCount = profile?.local_message_count ?? 0;
  const mediaCount = profile?.local_media_count ?? 0;
  const lastInteractionTs = profile?.last_interaction_ts ?? 0;

  const handleSendFriendRequest = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    const msg = greetingText.trim() || t("friendRequestDefaultGreeting");
    setActionBusyKind("send");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendSendRequest(targetUid, msg);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendRequestSendBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setAddFormOpen(false);
      if (res.already_friends) {
        setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_friend: true } : prev));
        setPendingRequest(null);
        setActionNotice(t("friendAlreadyFriends"));
      } else if (res.auto_accepted) {
        setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_friend: true } : prev));
        setPendingRequest(null);
        setActionNotice(t("friendRequestAutoAccepted"));
      } else {
        setPendingRequest({ kind: "sent", message: msg });
        setActionNotice(t("friendRequestSentSuccess"));
      }
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleAcceptRequest = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    const targetName = profile.display_name;
    setActionBusyKind("accept");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendAcceptRequest(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendRequestAcceptBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_friend: true } : prev));
      setPendingRequest(null);
      setActionNotice(t("friendAcceptedSuccess", { name: targetName }));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleRejectRequest = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    setActionBusyKind("reject");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendRejectRequest(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendRequestRejectBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setPendingRequest(null);
      setActionNotice(t("friendRejectedSuccess"));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleUndoRequest = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    setActionBusyKind("undo");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendUndoRequest(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendRequestUndoBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setPendingRequest(null);
      setActionNotice(t("friendUndoneSuccess"));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleConfirmUnfriend = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    const targetName = profile.display_name;
    setActionBusyKind("unfriend");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendRemove(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendUnfriendBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_friend: false } : prev));
      setPendingRequest(null);
      setConfirmAction(null);
      setActionNotice(t("friendUnfriendSuccess", { name: targetName }));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleConfirmBlock = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    const targetName = profile.display_name;
    setActionBusyKind("block");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendBlockUser(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendBlockBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_blocked: true } : prev));
      setConfirmAction(null);
      setActionNotice(t("friendBlockedSuccess", { name: targetName }));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleUnblock = async () => {
    if (!profile || profile.is_self || actionBusy) return;
    const targetUid = profile.uid;
    const targetName = profile.display_name;
    setActionBusyKind("unblock");
    setActionError(null);
    setActionNotice(null);
    try {
      const res = await bridge.friendUnblockUser(targetUid);
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      if (!res.ok) {
        setActionError(res.message || t("friendUnblockBtn"));
        return;
      }
      relationshipMutatedRef.current = true;
      setProfile((prev) => (prev && prev.uid === targetUid ? { ...prev, is_blocked: false } : prev));
      setConfirmAction(null);
      setActionNotice(t("friendUnblockedSuccess", { name: targetName }));
      onRelationshipChanged?.();
    } catch (err) {
      if (!mountedRef.current || activeUserIdRef.current !== targetUid) return;
      setActionError(cleanError(err));
    } finally {
      if (mountedRef.current && activeUserIdRef.current === targetUid) {
        setActionBusyKind(null);
      }
    }
  };

  const handleRequestClose = () => {
    if (actionBusy) return;
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
      onClick={handleRequestClose}
    >
      <div
        className="relative flex max-h-[86vh] w-full max-w-md flex-col overflow-hidden rounded-2xl shadow-2xl"
        style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          disabled={actionBusy}
          onClick={handleRequestClose}
          title={t("close")}
          className="absolute right-3 top-3 z-30 rounded-full bg-black/45 p-1.5 text-white transition hover:bg-black/65 disabled:cursor-not-allowed disabled:opacity-45"
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
              <div className="relative z-10 mb-1 flex flex-wrap items-center justify-end gap-1.5">
                {!profile.is_self && profile.is_blocked === true && (
                  <span
                    className="inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[11px] font-semibold"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                    }}
                  >
                    <Ban className="h-3 w-3" />
                    <span>{t("friendBlockedBadge")}</span>
                  </span>
                )}
                {!profile.is_self && !profile.is_friend && pendingRequest?.kind === "received" && (
                  <span
                    className="rounded-full px-2.5 py-0.5 text-[11px] font-medium"
                    style={{
                      background: "var(--accent-soft)",
                      color: "var(--accent)",
                    }}
                  >
                    {t("friendRequestReceivedBadge")}
                  </span>
                )}
                {!profile.is_self && !profile.is_friend && pendingRequest?.kind === "sent" && (
                  <span className="rounded-full bg-emerald-500/15 px-2.5 py-0.5 text-[11px] font-medium text-emerald-400">
                    {t("friendRequestSentBadge")}
                  </span>
                )}
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
                    ? t("badgeSelfAccount")
                    : profile.is_friend === true
                      ? t("badgeFriend")
                      : profile.is_friend === false
                        ? t("badgeStranger")
                        : t("badgeAccount")}
                </span>
              </div>
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
                  {profile.account_name && profile.account_name !== profile.display_name && (
                    <div className="selectable-text text-[13px]" style={{ color: "var(--text-secondary)" }}>
                      {t("originalNameLabel", { name: profile.account_name })}
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
                  <div className="mt-4 space-y-2.5">
                    {actionError && (
                      <div
                        role="alert"
                        className="rounded-xl px-3 py-2 text-xs"
                        style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                      >
                        {actionError}
                      </div>
                    )}

                    {actionNotice && (
                      <div className="rounded-xl bg-emerald-500/15 px-3 py-2 text-xs text-emerald-400">
                        {actionNotice}
                      </div>
                    )}

                    {!profile.is_friend && pendingRequest?.kind === "received" && (
                      <div
                        className="space-y-2 rounded-xl p-3"
                        style={{
                          background: "var(--bg-surface)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <div className="text-xs font-semibold" style={{ color: "var(--accent)" }}>
                          {t("friendRequestReceivedBadge")}
                        </div>
                        {pendingRequest.message.trim() && (
                          <p
                            className="selectable-text wrap-anywhere rounded-lg px-2.5 py-1.5 text-xs"
                            style={{
                              background: "var(--bg-sidebar)",
                              color: "var(--text-secondary)",
                            }}
                          >
                            {pendingRequest.message.trim()}
                          </p>
                        )}
                        <div className="grid grid-cols-2 gap-2">
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() => void handleAcceptRequest()}
                            className="tf-btn flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold disabled:opacity-50"
                          >
                            {actionBusyKind === "accept" ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <Check className="h-3.5 w-3.5" />
                            )}
                            <span>{t("friendRequestAcceptBtn")}</span>
                          </button>
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() => void handleRejectRequest()}
                            className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-medium transition disabled:opacity-50"
                            style={{
                              background: "var(--bg-sidebar)",
                              border: "1px solid var(--border-subtle)",
                              color: "var(--danger-text)",
                            }}
                          >
                            {actionBusyKind === "reject" ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <X className="h-3.5 w-3.5" />
                            )}
                            <span>{t("friendRequestRejectBtn")}</span>
                          </button>
                        </div>
                      </div>
                    )}

                    <button
                      type="button"
                      onClick={() => {
                        onStartChat(profile);
                        onClose();
                      }}
                      className="tf-btn flex w-full items-center justify-center gap-2 rounded-xl py-2.5 text-[14px] font-semibold"
                    >
                      <MessageSquare className="h-4 w-4" /> {t("startChatNowBtn")}
                    </button>

                    <div
                      className={
                        !profile.is_friend && pendingRequest?.kind === "received"
                          ? "grid grid-cols-1 gap-2"
                          : "grid grid-cols-2 gap-2"
                      }
                    >
                      {profile.is_friend === true ? (
                        <button
                          type="button"
                          disabled={actionBusy}
                          onClick={() => {
                            setAddFormOpen(false);
                            setActionError(null);
                            setConfirmAction((prev) => (prev === "unfriend" ? null : "unfriend"));
                          }}
                          className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background:
                              confirmAction === "unfriend"
                                ? "var(--danger-bg)"
                                : "var(--bg-surface)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--danger-text)",
                          }}
                        >
                          <UserMinus className="h-3.5 w-3.5" />
                          <span>{t("friendUnfriendBtn")}</span>
                        </button>
                      ) : pendingRequest?.kind === "sent" ? (
                        <button
                          type="button"
                          disabled={actionBusy}
                          onClick={() => void handleUndoRequest()}
                          className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background: "var(--bg-surface)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--text-primary)",
                          }}
                        >
                          {actionBusyKind === "undo" ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Undo2 className="h-3.5 w-3.5" />
                          )}
                          <span>{t("friendRequestUndoBtn")}</span>
                        </button>
                      ) : pendingRequest?.kind === "received" ? null : (
                        <button
                          type="button"
                          disabled={actionBusy}
                          onClick={() => {
                            setConfirmAction(null);
                            setActionError(null);
                            setAddFormOpen((prev) => {
                              const next = !prev;
                              if (next && !greetingText.trim()) {
                                setGreetingText(t("friendRequestDefaultGreeting"));
                              }
                              return next;
                            });
                          }}
                          className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background: addFormOpen ? "var(--accent-soft)" : "var(--bg-surface)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--accent)",
                          }}
                        >
                          <UserPlus className="h-3.5 w-3.5" />
                          <span>{t("friendAddBtn")}</span>
                        </button>
                      )}

                      {profile.is_blocked === true ? (
                        <button
                          type="button"
                          disabled={actionBusy}
                          onClick={() => void handleUnblock()}
                          className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background: "var(--bg-surface)",
                            border: "1px solid var(--border-subtle)",
                            color: "#34d399",
                          }}
                        >
                          {actionBusyKind === "unblock" ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <ShieldCheck className="h-3.5 w-3.5" />
                          )}
                          <span>{t("friendUnblockBtn")}</span>
                        </button>
                      ) : (
                        <button
                          type="button"
                          disabled={actionBusy}
                          onClick={() => {
                            setAddFormOpen(false);
                            setActionError(null);
                            setConfirmAction((prev) => (prev === "block" ? null : "block"));
                          }}
                          className="hover-menu flex items-center justify-center gap-1.5 rounded-xl px-3 py-2 text-xs font-semibold transition disabled:opacity-50"
                          style={{
                            background:
                              confirmAction === "block"
                                ? "var(--danger-bg)"
                                : "var(--bg-surface)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--danger-text)",
                          }}
                        >
                          <Ban className="h-3.5 w-3.5" />
                          <span>{t("friendBlockBtn")}</span>
                        </button>
                      )}
                    </div>

                    {addFormOpen && !profile.is_friend && (
                      <div
                        className="space-y-2 rounded-xl p-3"
                        style={{
                          background: "var(--bg-surface)",
                          border: "1px solid var(--border-subtle)",
                        }}
                      >
                        <textarea
                          rows={2}
                          maxLength={150}
                          value={greetingText}
                          onChange={(e) => setGreetingText(e.target.value)}
                          placeholder={t("friendRequestMessagePlaceholder")}
                          className="w-full resize-none rounded-xl px-3 py-2 text-xs outline-none"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--text-primary)",
                          }}
                        />
                        <div className="flex items-center justify-end gap-2">
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() => setAddFormOpen(false)}
                            className="hover-menu rounded-xl px-3 py-1.5 text-xs font-medium"
                            style={{ background: "var(--bg-sidebar)" }}
                          >
                            {t("socialCancelBtn")}
                          </button>
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() => void handleSendFriendRequest()}
                            className="tf-btn flex items-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-semibold disabled:opacity-50"
                          >
                            {actionBusy ? (
                              <Loader2 className="h-3.5 w-3.5 animate-spin" />
                            ) : (
                              <Send className="h-3.5 w-3.5" />
                            )}
                            <span>{t("friendRequestSendBtn")}</span>
                          </button>
                        </div>
                      </div>
                    )}

                    {confirmAction && (
                      <div
                        className="space-y-2.5 rounded-xl p-3"
                        style={{
                          background: "var(--danger-bg)",
                          border: "1px solid rgba(239, 68, 68, 0.35)",
                        }}
                      >
                        <div
                          className="text-xs font-semibold"
                          style={{ color: "var(--danger-text)" }}
                        >
                          {confirmAction === "unfriend"
                            ? t("friendUnfriendConfirmTitle")
                            : t("friendBlockConfirmTitle")}
                        </div>
                        <p className="text-xs leading-relaxed" style={{ color: "var(--text-primary)" }}>
                          {confirmAction === "unfriend"
                            ? t("friendUnfriendConfirmDesc", { name: profile.display_name })
                            : t("friendBlockConfirmDesc", { name: profile.display_name })}
                        </p>
                        <div className="flex items-center justify-end gap-2">
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() => setConfirmAction(null)}
                            className="hover-menu rounded-xl px-3 py-1.5 text-xs font-medium"
                            style={{
                              background: "var(--bg-sidebar)",
                              color: "var(--text-primary)",
                            }}
                          >
                            {t("socialCancelBtn")}
                          </button>
                          <button
                            type="button"
                            disabled={actionBusy}
                            onClick={() =>
                              void (confirmAction === "unfriend"
                                ? handleConfirmUnfriend()
                                : handleConfirmBlock())
                            }
                            className="flex items-center gap-1.5 rounded-xl bg-rose-600 px-3 py-1.5 text-xs font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
                          >
                            {actionBusy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                            <span>{t("socialConfirmActionBtn")}</span>
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
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
