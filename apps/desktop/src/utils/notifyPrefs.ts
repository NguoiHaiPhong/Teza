import type {
  MessageDto,
  NotificationPrefs,
  NotifyBadgeMode,
  NotifyPosition,
  NotifyPreviewMode,
  ThreadDto,
} from "../types.ts";
import { tokenizeMentions } from "./mentions.ts";

const PREFS_STORAGE_KEY = "teza.notifyPrefs";

export const DEFAULT_NOTIFY_PREFS: NotificationPrefs = {
  enabled: true,
  sound: true,
  buzz: true,
  notifyDirect: true,
  notifyGroup: true,
  mentionOverride: true,
  notifyWhenFocused: false,
  previewMode: "full",
  position: "bottom-right",
  durationSec: 5,
  badgeMode: "unread_messages",
  includeMutedInBadge: false,
  useCustomPopup: true,
};

const VALID_POSITIONS = new Set<NotifyPosition>([
  "top-left",
  "top-right",
  "bottom-left",
  "bottom-right",
]);
const VALID_PREVIEW_MODES = new Set<NotifyPreviewMode>(["full", "sender_only", "hidden"]);
const VALID_BADGE_MODES = new Set<NotifyBadgeMode>(["unread_messages", "unread_chats"]);

export function normalizeNotifyPrefs(
  raw?: Partial<NotificationPrefs> | null,
): NotificationPrefs {
  const src = raw ?? {};
  const durationNum = Number(src.durationSec);
  const safeDuration =
    Number.isFinite(durationNum) && durationNum >= 3 && durationNum <= 15
      ? Math.round(durationNum)
      : DEFAULT_NOTIFY_PREFS.durationSec;

  return {
    enabled: typeof src.enabled === "boolean" ? src.enabled : DEFAULT_NOTIFY_PREFS.enabled,
    sound: typeof src.sound === "boolean" ? src.sound : DEFAULT_NOTIFY_PREFS.sound,
    buzz: typeof src.buzz === "boolean" ? src.buzz : DEFAULT_NOTIFY_PREFS.buzz,
    notifyDirect:
      typeof src.notifyDirect === "boolean"
        ? src.notifyDirect
        : DEFAULT_NOTIFY_PREFS.notifyDirect,
    notifyGroup:
      typeof src.notifyGroup === "boolean" ? src.notifyGroup : DEFAULT_NOTIFY_PREFS.notifyGroup,
    mentionOverride:
      typeof src.mentionOverride === "boolean"
        ? src.mentionOverride
        : DEFAULT_NOTIFY_PREFS.mentionOverride,
    notifyWhenFocused:
      typeof src.notifyWhenFocused === "boolean"
        ? src.notifyWhenFocused
        : DEFAULT_NOTIFY_PREFS.notifyWhenFocused,
    previewMode:
      src.previewMode && VALID_PREVIEW_MODES.has(src.previewMode)
        ? src.previewMode
        : DEFAULT_NOTIFY_PREFS.previewMode,
    position:
      src.position && VALID_POSITIONS.has(src.position)
        ? src.position
        : DEFAULT_NOTIFY_PREFS.position,
    durationSec: safeDuration,
    badgeMode:
      src.badgeMode && VALID_BADGE_MODES.has(src.badgeMode)
        ? src.badgeMode
        : DEFAULT_NOTIFY_PREFS.badgeMode,
    includeMutedInBadge:
      typeof src.includeMutedInBadge === "boolean"
        ? src.includeMutedInBadge
        : DEFAULT_NOTIFY_PREFS.includeMutedInBadge,
    useCustomPopup:
      typeof src.useCustomPopup === "boolean"
        ? src.useCustomPopup
        : DEFAULT_NOTIFY_PREFS.useCustomPopup,
  };
}

export function loadNotifyPrefs(): NotificationPrefs {
  if (typeof localStorage === "undefined") {
    return { ...DEFAULT_NOTIFY_PREFS };
  }
  try {
    const raw = localStorage.getItem(PREFS_STORAGE_KEY);
    if (raw) {
      return normalizeNotifyPrefs(JSON.parse(raw) as Partial<NotificationPrefs>);
    }
    return normalizeNotifyPrefs({
      enabled: localStorage.getItem("litechat.notify") !== "0",
      sound: localStorage.getItem("litechat.sound") !== "0",
      buzz: localStorage.getItem("litechat.buzz") !== "0",
    });
  } catch {
    return { ...DEFAULT_NOTIFY_PREFS };
  }
}

export function saveNotifyPrefs(prefs: NotificationPrefs): void {
  if (typeof localStorage === "undefined") return;
  try {
    const clean = normalizeNotifyPrefs(prefs);
    localStorage.setItem(PREFS_STORAGE_KEY, JSON.stringify(clean));
  } catch {
    // Ignore storage quota errors
  }
}

export function isThreadCurrentlyMuted(
  thread: Pick<ThreadDto, "is_muted" | "muted_until_ts"> | undefined,
  nowMs = Date.now(),
): boolean {
  if (!thread?.is_muted) return false;
  if (!thread.muted_until_ts || thread.muted_until_ts <= 0) return true;
  return thread.muted_until_ts > nowMs;
}

export function isMessageMentioningMe(
  msg: MessageDto,
  myUid: string,
  myName: string,
): boolean {
  const normalizedUid = myUid.trim();
  const cleanName = myName.trim();
  const candidates =
    normalizedUid && cleanName ? [{ id: normalizedUid, name: cleanName }] : [];
  const tokens = tokenizeMentions(msg.text || "", candidates);
  for (const tok of tokens) {
    if (tok.kind !== "mention") continue;
    if (tok.isAll) return true;
    if (normalizedUid && tok.userId === normalizedUid) return true;
  }
  const quotedSender = (msg.quote?.sender_name || "").normalize("NFC").trim().toLowerCase();
  const cleanNameLower = cleanName.normalize("NFC").toLowerCase();
  if (
    quotedSender &&
    (quotedSender === "bạn" ||
      quotedSender === "you" ||
      (cleanNameLower && quotedSender === cleanNameLower))
  ) {
    return true;
  }
  return false;
}

export function shouldNotifyMessage(params: {
  msg: MessageDto;
  thread?: ThreadDto;
  isActiveThread: boolean;
  windowFocused: boolean;
  prefs: NotificationPrefs;
  myUid: string;
  myName: string;
  nowMs?: number;
}): { showPopup: boolean; playSound: boolean } {
  const { msg, thread, isActiveThread, windowFocused, prefs, myUid, myName, nowMs } = params;
  if (msg.is_outgoing || thread?.kind === "cloud") {
    return { showPopup: false, playSound: false };
  }
  if (isActiveThread && windowFocused) {
    return { showPopup: false, playSound: false };
  }

  const kind = thread?.kind ?? "direct";
  if (kind === "direct" && !prefs.notifyDirect) {
    return { showPopup: false, playSound: false };
  }
  if (kind === "group" && !prefs.notifyGroup) {
    return { showPopup: false, playSound: false };
  }

  if (isThreadCurrentlyMuted(thread, nowMs)) {
    const allowedByMention =
      prefs.mentionOverride && isMessageMentioningMe(msg, myUid, myName);
    if (!allowedByMention) {
      return { showPopup: false, playSound: false };
    }
  }

  const showPopup =
    prefs.enabled && (!windowFocused || (prefs.notifyWhenFocused && !isActiveThread));
  const playSound = prefs.sound && (!windowFocused || !isActiveThread);
  return { showPopup, playSound };
}

export function formatNotificationContent(params: {
  msg: MessageDto;
  thread?: ThreadDto;
  previewMode: NotifyPreviewMode;
}): { title: string; body: string } {
  const { msg, thread, previewMode } = params;
  if (previewMode === "hidden") {
    return { title: "Teza", body: "Bạn có tin nhắn mới" };
  }
  const title = thread?.title || msg.sender_name || "Tin nhắn mới";
  if (previewMode === "sender_only") {
    return { title, body: "Đã gửi một tin nhắn mới" };
  }
  const rawBody = msg.text.trim() || msg.attachment_name || "[Tệp đính kèm]";
  const body =
    thread?.kind === "group" && msg.sender_name
      ? `${msg.sender_name}: ${rawBody}`
      : rawBody;
  return { title, body };
}

export function computeTotalUnreadBadge(
  threads: ThreadDto[],
  prefs: Pick<NotificationPrefs, "badgeMode" | "includeMutedInBadge">,
  nowMs = Date.now(),
): number {
  let total = 0;
  for (const t of threads) {
    if (t.unread_count <= 0) continue;
    if (!prefs.includeMutedInBadge && isThreadCurrentlyMuted(t, nowMs)) continue;
    total += prefs.badgeMode === "unread_chats" ? 1 : t.unread_count;
  }
  return total;
}
