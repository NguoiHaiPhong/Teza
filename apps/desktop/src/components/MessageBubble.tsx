import { memo, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Ban,
  BarChart3,
  CalendarClock,
  Check,
  CheckCheck,
  CheckSquare,
  Copy,
  CornerUpLeft,
  Download,
  ExternalLink,
  Eye,
  EyeOff,
  FileText,
  Forward,
  Image as ImageIcon,
  Languages,
  Loader2,
  Lock,
  MessagesSquare,
  Phone,
  PhoneIncoming,
  PhoneMissed,
  PhoneOutgoing,
  Pin,
  Plus,
  RefreshCw,
  Repeat,
  RotateCcw,
  Smile,
  Trash2,
  Video,
  X,
  Zap,
} from "lucide-react";
import { bridge } from "../bridge";
import type { ConnectionStatus, MessageDto, PollDto, ThreadKind } from "../types";
import { useI18n } from "../i18n";
import {
  extractStickerIdentity,
  type BlockedStickerDisplayMode,
  type BlockedStickerKind,
  type BlockedStickerRule,
} from "../utils/blockedStickers";
import { isBuzzMessage } from "../utils/buzz";
import { formatCallDuration, type CallStartDisabledReason } from "../utils/callState";
import { copyImageReliable, copyTextReliable } from "../utils/clipboard";
import { useEmojiStyle } from "../utils/emojiStyleContext";
import {
  isPureRetroEmoticonMessage,
  tokenizeRetroEmoticons,
  type EmojiStyle,
} from "../utils/retroEmoticons";
import { EmojiGlyph, RetroEmoticon } from "./RetroEmoticon";
import {
  classifyNonMediaAttachment,
  extractLinkHost,
  isBlobUrl,
  isDataImageUrl,
  isDataUrl,
  isRemoteMediaUrl,
  normalizeMediaRequestUrl,
} from "../utils/mediaCache";
import {
  formatReminderCountdown,
  parseCallPayload,
  parsePollPayload,
  parseReminderPayload,
} from "../utils/richMessage";
import { tokenizeMentions, type MentionCandidate } from "../utils/mentions";
import { isImeComposingEvent } from "../utils/searchNormalize";
import {
  QUICK_REACTION_EMOJIS,
  getDisplayBodyText,
  normalizeReactions,
  splitTextLinks,
} from "../utils/threadState";
import { RelayAudio, RelayImage, RelayVideo } from "./RelayImage";
import { ThreadAvatar } from "./ThreadAvatar";

export interface MessageTranslationState {
  text?: string;
  src?: string | null;
  targetLang?: string;
  loading?: boolean;
  error?: string;
  skipped?: boolean;
  note?: string;
}

interface Props {
  msg: MessageDto;
  threadKind: ThreadKind;
  groupedWithPrev: boolean;
  groupedWithNext?: boolean;
  highlighted: boolean;
  isSeen?: boolean;
  senderAvatar?: string;
  translation?: MessageTranslationState;
  replaceWithTranslation?: boolean;
  replyCount?: number;
  hideQuote?: boolean;
  highlightQuery?: string;
  selectMode?: boolean;
  selected?: boolean;
  onToggleSelect?: (msgId: string) => void;
  onStartSelect?: (msgId: string) => void;
  onTogglePin?: (msg: MessageDto) => void;
  onReply: (msg: MessageDto) => void;
  onReact: (msg: MessageDto, emoji: string) => void;
  onRecall: (msg: MessageDto) => void;
  onDeleteLocal: (msg: MessageDto) => void;
  onJumpToMsg: (msgId: string) => void;
  onOpenImage: (url: string, title: string, opts?: { threadId?: string; msgId?: string }) => void;
  onOpenProfile?: (userId: string) => void;
  mentionMembers?: MentionCandidate[];
  onForward?: (msg: MessageDto) => void;
  onTranslate?: (msg: MessageDto) => void;
  onHideTranslation?: (msgId: string) => void;
  onOpenThread?: (msg: MessageDto) => void;
  onRetrySend?: (msg: MessageDto) => void;
  onDiscardFailed?: (msgId: string) => void;
  discardDisabled?: boolean;
  onSaveFile?: (url: string, name: string, opts?: { threadId?: string; msgId?: string }) => void;
  onOpenExternal?: (url: string) => void;
  onCopyError?: (err: unknown) => void;
  onVotePoll?: (
    msg: MessageDto,
    optionIds: number[],
  ) => Promise<boolean | string | void> | boolean | string | void;
  onAddPollOption?: (
    msg: MessageDto,
    optionText: string,
    votedOptionIds: number[],
  ) => Promise<boolean | string | void> | boolean | string | void;
  onLockPoll?: (
    msg: MessageDto,
  ) => Promise<boolean | string | void> | boolean | string | void;
  onRefreshPoll?: (
    msg: MessageDto,
  ) => Promise<boolean | string | void> | boolean | string | void;
  currentUserId?: string;
  canManagePolls?: boolean;
  connectionStatus?: ConnectionStatus;
  onStartVoiceCall?: (partnerId: string, partnerName?: string) => void;
  callDisabledReason?: CallStartDisabledReason | null;
  blockedStickerRule?: BlockedStickerRule | null;
  blockedStickerMode?: BlockedStickerDisplayMode;
  onBlockSticker?: (
    msg: MessageDto,
    opts: { kind: BlockedStickerKind; global: boolean },
  ) => void;
  onUnblockSticker?: (ruleId: string) => void;
}

function formatClock(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
}

async function blobUrlToDataUrl(blobUrl: string): Promise<string> {
  try {
    const res = await fetch(blobUrl);
    if (!res.ok) {
      throw new Error(`HTTP ${res.status}`);
    }
    const blob = await res.blob();
    return await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        if (typeof reader.result === "string" && reader.result.length > 0) {
          resolve(reader.result);
        } else {
          reject(new Error("Không chuyển đổi được dữ liệu media cục bộ"));
        }
      };
      reader.onerror = () => reject(new Error("Không đọc được dữ liệu media cục bộ"));
      reader.readAsDataURL(blob);
    });
  } catch {
    const img = await new Promise<HTMLImageElement>((resolve, reject) => {
      const el = new Image();
      el.onload = () => resolve(el);
      el.onerror = () => reject(new Error("Không đọc được ảnh cục bộ để lưu"));
      el.src = blobUrl;
    });
    const w = img.naturalWidth || img.width;
    const h = img.naturalHeight || img.height;
    if (!w || !h) {
      throw new Error("Ảnh cục bộ không có kích thước hợp lệ");
    }
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      throw new Error("Không khởi tạo được bộ đệm ảnh");
    }
    ctx.drawImage(img, 0, 0);
    return canvas.toDataURL("image/png");
  }
}

function isStickerMessage(msg: MessageDto, normalizedUrl: string): boolean {
  const t = msg.msg_type.toLowerCase();
  return (
    !!normalizedUrl &&
    (t.includes("sticker") || t === "5" || t === "35" || t === "36")
  );
}

function isVoiceAttachment(msg: MessageDto, normalizedUrl: string): boolean {
  const t = msg.msg_type.toLowerCase();
  return (
    (isRemoteMediaUrl(normalizedUrl) || /^data:audio\//i.test(normalizedUrl)) &&
    (t.includes("voice") || t === "3")
  );
}

function isVideoAttachment(msg: MessageDto, normalizedUrl: string): boolean {
  const t = msg.msg_type.toLowerCase();
  return (
    (isRemoteMediaUrl(normalizedUrl) || /^data:video\//i.test(normalizedUrl)) &&
    (t.includes("video") || t === "4" || t === "44")
  );
}

function renderHighlightedPlain(text: string, query?: string) {
  const q = query?.trim();
  if (!q) return text;
  const escaped = q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`(${escaped})`, "gi");
  const segments = text.split(re);
  if (segments.length <= 1) return text;
  const qLower = q.toLowerCase();
  return segments.map((seg, i) =>
    seg.toLowerCase() === qLower ? (
      <mark key={i} className="rounded bg-amber-400/35 px-0.5 text-inherit">
        {seg}
      </mark>
    ) : (
      seg
    ),
  );
}

function renderPlainWithEmoticons(
  text: string,
  emojiStyle: EmojiStyle,
  highlightQuery?: string,
  largeEmoticons = false,
) {
  if (emojiStyle !== "retro") {
    return renderHighlightedPlain(text, highlightQuery);
  }
  const tokens = tokenizeRetroEmoticons(text);
  if (!tokens.some((tok) => tok.kind === "emoticon")) {
    return renderHighlightedPlain(text, highlightQuery);
  }
  const px = largeEmoticons ? 36 : 22;
  return tokens.map((tok, i) =>
    tok.kind === "emoticon" ? (
      <RetroEmoticon
        key={i}
        id={tok.emoticon.id}
        size={px}
        className="mx-[1px]"
        title={`${tok.emoticon.label} (${tok.emoticon.primaryShortcut})`}
      />
    ) : (
      <span key={i}>{renderHighlightedPlain(tok.text, highlightQuery)}</span>
    ),
  );
}

function formatPollErrorMessage(
  err: unknown,
  lang: "vi" | "en" | "ko",
  fallbackKind: "action" | "load" = "action",
): string {
  const raw =
    err instanceof Error
      ? err.message
      : typeof err === "string"
        ? err
        : err !== null && err !== undefined
          ? String(err)
          : "";
  const trimmed = raw.trim();
  if (trimmed) return trimmed;
  if (fallbackKind === "load") {
    return lang === "en"
      ? "Unable to load poll options. Tap refresh to try again."
      : lang === "ko"
        ? "투표 항목을 불러오지 못했습니다. 새로고침을 눌러 다시 시도하세요."
        : "Không thể tải lựa chọn bình chọn. Nhấn làm mới để thử lại.";
  }
  return lang === "en"
    ? "Poll action failed. Please try again."
    : lang === "ko"
      ? "투표 작업을 완료하지 못했습니다. 다시 시도해 주세요."
      : "Thao tác bình chọn thất bại. Vui lòng thử lại.";
}

function renderMentionsAndSearch(
  text: string,
  isOutgoing: boolean,
  emojiStyle: EmojiStyle,
  mentionMembers: MentionCandidate[],
  onOpenProfile: ((userId: string) => void) | undefined,
  selectMode: boolean,
  viewProfileTitle: string,
  highlightQuery?: string,
  largeEmoticons = false,
  allowAll = true,
) {
  const tokens = tokenizeMentions(text, mentionMembers, { allowAll });
  return tokens.map((tok, idx) => {
    if (tok.kind === "mention") {
      const color = isOutgoing ? "var(--link-out)" : "var(--accent)";
      if (tok.userId && onOpenProfile && !selectMode) {
        const targetUid = tok.userId;
        return (
          <button
            key={idx}
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onOpenProfile(targetUid);
            }}
            title={tok.displayName ? `${tok.displayName} — ${viewProfileTitle}` : viewProfileTitle}
            className="inline cursor-pointer font-semibold hover:underline focus:outline-none"
            style={{ color }}
          >
            {renderHighlightedPlain(tok.text, highlightQuery)}
          </button>
        );
      }
      return (
        <span key={idx} className="font-semibold" style={{ color }}>
          {renderHighlightedPlain(tok.text, highlightQuery)}
        </span>
      );
    }
    return (
      <span key={idx}>
        {renderPlainWithEmoticons(tok.text, emojiStyle, highlightQuery, largeEmoticons)}
      </span>
    );
  });
}

function renderLinkedText(
  text: string,
  isOutgoing: boolean,
  emojiStyle: EmojiStyle,
  mentionMembers: MentionCandidate[],
  onOpenProfile: ((userId: string) => void) | undefined,
  selectMode: boolean,
  viewProfileTitle: string,
  highlightQuery?: string,
  largeEmoticons = false,
  allowAll = true,
) {
  const parts = splitTextLinks(text);
  return parts.map((part, idx) =>
    part.url ? (
      <a
        key={idx}
        href={part.url}
        target="_blank"
        rel="noreferrer"
        className="underline underline-offset-2 hover:opacity-80"
        style={{ color: isOutgoing ? "var(--link-out)" : "var(--link-in)" }}
      >
        {part.text}
      </a>
    ) : (
      <span key={idx}>
        {renderMentionsAndSearch(
          part.text,
          isOutgoing,
          emojiStyle,
          mentionMembers,
          onOpenProfile,
          selectMode,
          viewProfileTitle,
          highlightQuery,
          largeEmoticons,
          allowAll,
        )}
      </span>
    ),
  );
}

function isImageAttachment(msg: MessageDto, normalizedUrl: string): boolean {
  if (!normalizedUrl) return false;
  if (isStickerMessage(msg, normalizedUrl)) return false;
  const t = msg.msg_type.toLowerCase();
  if (t.includes("photo") || t === "2" || t.includes("gif")) {
    return (
      isRemoteMediaUrl(normalizedUrl) ||
      isDataImageUrl(normalizedUrl) ||
      isBlobUrl(normalizedUrl)
    );
  }
  if (isDataImageUrl(normalizedUrl)) return true;
  return (
    isRemoteMediaUrl(normalizedUrl) &&
    /\.(png|jpe?g|webp|gif|bmp)(\?|$)/i.test(normalizedUrl)
  );
}

export const MessageBubble = memo(function MessageBubble({
  msg,
  threadKind,
  groupedWithPrev,
  groupedWithNext = false,
  highlighted,
  isSeen = false,
  senderAvatar,
  translation,
  replaceWithTranslation = false,
  replyCount = 0,
  hideQuote = false,
  highlightQuery = "",
  selectMode = false,
  selected = false,
  onToggleSelect,
  onStartSelect,
  onTogglePin,
  onReply,
  onReact,
  onRecall,
  onDeleteLocal,
  onJumpToMsg,
  onOpenImage,
  onOpenProfile,
  mentionMembers = [],
  onForward,
  onTranslate,
  onHideTranslation,
  onOpenThread,
  onRetrySend,
  onDiscardFailed,
  discardDisabled = false,
  onSaveFile,
  onOpenExternal,
  onCopyError,
  onVotePoll,
  onAddPollOption,
  onLockPoll,
  onRefreshPoll,
  currentUserId,
  canManagePolls,
  connectionStatus,
  onStartVoiceCall,
  callDisabledReason = null,
  blockedStickerRule = null,
  blockedStickerMode = "collapse",
  onBlockSticker,
  onUnblockSticker,
}: Props) {
  const { lang, locale, t } = useI18n();
  const { emojiStyle } = useEmojiStyle();
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [pickerPos, setPickerPos] = useState<{ left: number; top: number } | null>(null);
  const [mediaError, setMediaError] = useState(false);
  const [mediaRetryKey, setMediaRetryKey] = useState(0);
  const [showOriginal, setShowOriginal] = useState(false);
  const [tempRevealed, setTempRevealed] = useState(false);
  const [pollBusy, setPollBusy] = useState(false);
  const [pollError, setPollError] = useState<string | null>(null);
  const [pollRetryTick, setPollRetryTick] = useState(0);
  const [newPollOption, setNewPollOption] = useState("");
  const [confirmLockPoll, setConfirmLockPoll] = useState(false);
  const hydratedPollIdsRef = useRef<Set<number>>(new Set());

  useEffect(() => {
    setTempRevealed(false);
  }, [msg.msg_id, blockedStickerRule?.id]);

  useEffect(() => {
    setMediaError(false);
    setMediaRetryKey(0);
  }, [msg.attachment_url, msg.msg_type]);
  const normalizedAttachmentUrl = normalizeMediaRequestUrl(msg.attachment_url);
  const hasVisibleTranslation = Boolean(translation?.text && !translation?.skipped);
  const isReplaced = Boolean(replaceWithTranslation && hasVisibleTranslation && !showOriginal);
  const reminderData = !msg.is_recalled ? parseReminderPayload(msg) : null;
  const pollData = !msg.is_recalled && !reminderData ? parsePollPayload(msg) : null;
  const sparsePollId =
    pollData && pollData.poll_id > 0 && pollData.options.length === 0 ? pollData.poll_id : 0;
  const pollClosedFlag = Boolean(
    pollData && (pollData.closed || (pollData.expired_time > 0 && pollData.expired_time < Date.now())),
  );

  useEffect(() => {
    setPollError(null);
  }, [pollData?.poll_id]);

  useEffect(() => {
    if (sparsePollId <= 0) return;
    const onOnline = () => {
      if (!hydratedPollIdsRef.current.has(sparsePollId)) {
        setPollRetryTick((n) => n + 1);
      }
    };
    window.addEventListener("online", onOnline);
    return () => window.removeEventListener("online", onOnline);
  }, [sparsePollId]);

  useEffect(() => {
    if (sparsePollId <= 0 || !onRefreshPoll) return;
    if (connectionStatus && connectionStatus !== "connected") return;
    if (hydratedPollIdsRef.current.has(sparsePollId)) return;
    let cancelled = false;
    hydratedPollIdsRef.current.add(sparsePollId);
    setPollBusy(true);
    setPollError(null);
    void Promise.resolve(onRefreshPoll(msg))
      .then((res) => {
        if (res === false || (typeof res === "string" && res.trim().length > 0)) {
          hydratedPollIdsRef.current.delete(sparsePollId);
          if (!cancelled) {
            setPollError(
              formatPollErrorMessage(typeof res === "string" ? res : "", lang, "load"),
            );
          }
        }
      })
      .catch((err) => {
        hydratedPollIdsRef.current.delete(sparsePollId);
        if (!cancelled) {
          setPollError(formatPollErrorMessage(err, lang, "load"));
        }
      })
      .finally(() => {
        if (!cancelled) {
          setPollBusy(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [sparsePollId, onRefreshPoll, msg, connectionStatus, pollRetryTick, lang]);

  useEffect(() => {
    if (pollClosedFlag) {
      setConfirmLockPoll(false);
      setNewPollOption("");
    }
  }, [pollClosedFlag, pollData?.poll_id]);
  const callData = !msg.is_recalled && !reminderData && !pollData ? parseCallPayload(msg) : null;
  const isRichCard = Boolean(reminderData || pollData || callData);
  const isBuzz = !isRichCard && isBuzzMessage(msg);
  const isStickerKind =
    !msg.is_recalled && !isRichCard && isStickerMessage(msg, normalizedAttachmentUrl);
  const isVoiceKind =
    !msg.is_recalled && !isRichCard && isVoiceAttachment(msg, normalizedAttachmentUrl);
  const isVideoKind =
    !msg.is_recalled && !isRichCard && isVideoAttachment(msg, normalizedAttachmentUrl);
  const isImageKind =
    !msg.is_recalled &&
    !isRichCard &&
    !isVoiceKind &&
    !isVideoKind &&
    isImageAttachment(msg, normalizedAttachmentUrl);
  const isAnyMediaKind = isStickerKind || isVoiceKind || isVideoKind || isImageKind;
  const isSticker = isStickerKind && !mediaError;
  const isVoice = isVoiceKind && !mediaError;
  const isVideo = isVideoKind && !mediaError;
  const isImage = isImageKind && !mediaError;
  const isMediaFailed = isAnyMediaKind && mediaError;
  const nonMediaAction =
    !msg.is_recalled && !isRichCard && !isAnyMediaKind
      ? classifyNonMediaAttachment(msg)
      : "none";
  const isExternalLinkCard =
    nonMediaAction === "open_external" && isRemoteMediaUrl(normalizedAttachmentUrl);
  const isFileAttachment =
    nonMediaAction === "save_file" || nonMediaAction === "file_no_url";
  const displayText = msg.is_recalled
    ? msg.text
    : isReplaced && translation?.text
      ? translation.text
      : getDisplayBodyText(msg);
  const isPureEmoticon =
    emojiStyle === "retro" &&
    !msg.is_recalled &&
    !isRichCard &&
    !isBuzz &&
    Boolean(displayText && isPureRetroEmoticonMessage(displayText));
  const isPlainSticker =
    isSticker &&
    !msg.quote &&
    !msg.is_recalled &&
    !displayText &&
    !isBuzz &&
    !(threadKind === "group" && !msg.is_outgoing && !groupedWithPrev);
  const hasTranslatableBody = Boolean(
    !msg.is_recalled && !isRichCard && getDisplayBodyText(msg).trim(),
  );
  const safeDownloadUrl =
    nonMediaAction === "save_file" &&
    (isRemoteMediaUrl(normalizedAttachmentUrl) || isDataUrl(normalizedAttachmentUrl))
      ? normalizedAttachmentUrl
      : null;
  const canCopyImage =
    isImage &&
    (isRemoteMediaUrl(normalizedAttachmentUrl) ||
      isDataImageUrl(normalizedAttachmentUrl) ||
      isBlobUrl(normalizedAttachmentUrl));
  const canSaveMedia = Boolean(
    onSaveFile &&
      !msg.is_recalled &&
      (isImage || isVideo || isVoice || isSticker) &&
      (isRemoteMediaUrl(normalizedAttachmentUrl) ||
        isDataUrl(normalizedAttachmentUrl) ||
        isBlobUrl(normalizedAttachmentUrl)),
  );
  const saveMediaLabel = isVideo
    ? t("saveVideoMenu")
    : isVoice
      ? t("saveVoiceMenu")
      : isSticker
        ? t("saveStickerMenu")
        : t("saveImageMenu");
  const suggestedMediaName =
    msg.attachment_name?.trim() ||
    (isVideo
      ? "teza-video"
      : isVoice
        ? "teza-voice"
        : isSticker
          ? "teza-sticker"
          : "teza-image");

  useEffect(() => {
    if (!menuPos && !pickerPos) return;
    const close = () => {
      setMenuPos(null);
      setPickerPos(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && !isImeComposingEvent(e)) {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [menuPos, pickerPos]);

  const handleCopy = () => {
    const textToCopy = isReplaced && translation?.text ? translation.text : msg.text;
    void copyTextReliable(textToCopy).catch((err) => onCopyError?.(err));
    setMenuPos(null);
  };

  const handleCopyImage = () => {
    if (!canCopyImage) return;
    setMenuPos(null);
    void copyImageReliable(normalizedAttachmentUrl).catch((err) => onCopyError?.(err));
  };

  const handleSaveMedia = () => {
    if (!canSaveMedia || !onSaveFile) return;
    setMenuPos(null);
    const run = async () => {
      const targetUrl = isBlobUrl(normalizedAttachmentUrl)
        ? await blobUrlToDataUrl(normalizedAttachmentUrl)
        : normalizedAttachmentUrl;
      onSaveFile(targetUrl, suggestedMediaName, {
        threadId: msg.thread_id,
        msgId: msg.msg_id,
      });
    };
    void run().catch((err) => onCopyError?.(err));
  };

  const handleOpenExternalLink = (url: string) => {
    if (onOpenExternal) {
      onOpenExternal(url);
    } else {
      void bridge.openExternalUrl(url).catch((err) => onCopyError?.(err));
    }
  };

  const bubbleCornerClass = msg.is_outgoing
    ? groupedWithPrev && groupedWithNext
      ? "rounded-2xl rounded-r-[6px]"
      : groupedWithPrev && !groupedWithNext
        ? "rounded-2xl rounded-tr-[6px] rounded-br-[4px]"
        : !groupedWithPrev && groupedWithNext
          ? "rounded-2xl rounded-br-[6px]"
          : "rounded-2xl rounded-br-[4px]"
    : groupedWithPrev && groupedWithNext
      ? "rounded-2xl rounded-l-[6px]"
      : groupedWithPrev && !groupedWithNext
        ? "rounded-2xl rounded-tl-[6px] rounded-bl-[4px]"
        : !groupedWithPrev && groupedWithNext
          ? "rounded-2xl rounded-bl-[6px]"
          : "rounded-2xl rounded-bl-[4px]";

  const hasTranslationPanel = Boolean(
    translation &&
      !isReplaced &&
      (!translation.skipped || Boolean(translation.note)) &&
      (!replaceWithTranslation ||
        showOriginal ||
        Boolean(translation.error) ||
        Boolean(translation.note)),
  );
  const visibleReactions = normalizeReactions(msg.reactions);
  const canInteractReactions = !selectMode && !msg.is_recalled && !msg.send_status;
  const useInlineMeta = Boolean(
    displayText &&
      !isBuzz &&
      !isPureEmoticon &&
      !isRichCard &&
      !isPlainSticker &&
      !hasTranslationPanel &&
      visibleReactions.length === 0 &&
      !isReplaced &&
      !(replaceWithTranslation && translation?.loading) &&
      msg.send_status !== "failed" &&
      !(replyCount > 0 && onOpenThread),
  );
  const inlineMetaWidthPx =
    48 +
    (msg.is_outgoing && !msg.is_recalled ? (isSeen ? 18 : 16) : 0) +
    (msg.is_pinned ? 16 : 0);

  const timestampEl = (
    <span
      className={`inline-flex select-none items-center gap-1 text-[11px] leading-none tabular-nums ${
        isPlainSticker ? "rounded-full px-2 py-0.5" : ""
      }`}
      style={{
        color: isPlainSticker ? "var(--pill-text)" : "var(--text-secondary)",
        background: isPlainSticker ? "var(--pill-bg)" : undefined,
      }}
    >
      {msg.is_pinned && (
        <span title={t("pinnedBadgeTitle")} className="inline-flex items-center">
          <Pin
            className="h-3 w-3 rotate-45"
            style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
          />
        </span>
      )}
      <span>{formatClock(msg.ts)}</span>
      {msg.is_outgoing && !msg.is_recalled && msg.send_status === "sending" && (
        <span title={t("sendingStatus")}>
          <Loader2 className="h-3 w-3 animate-spin" />
        </span>
      )}
      {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
        isSeen ? (
          <span title={t("youSeenStatus")}>
            <CheckCheck className="h-3.5 w-3.5" style={{ color: "var(--accent)" }} />
          </span>
        ) : (
          <span title={t("youSentStatus")}>
            <Check className="h-3 w-3 opacity-80" />
          </span>
        )
      )}
    </span>
  );

  const effectiveBlockedStickerRule =
    !msg.is_outgoing && !msg.is_recalled ? blockedStickerRule : null;
  const stickerIdentity = isStickerKind ? extractStickerIdentity(msg) : null;
  const canBlockStickerSingle = Boolean(
    !msg.is_outgoing &&
      isStickerKind &&
      !msg.is_recalled &&
      !msg.send_status &&
      onBlockSticker &&
      stickerIdentity &&
      (stickerIdentity.stickerId || stickerIdentity.fallbackUrl),
  );
  const canBlockStickerCategory = Boolean(
    !msg.is_outgoing &&
      isStickerKind &&
      !msg.is_recalled &&
      !msg.send_status &&
      onBlockSticker &&
      stickerIdentity?.cateId,
  );
  const hasBlockStickerActions = canBlockStickerSingle || canBlockStickerCategory;

  if (effectiveBlockedStickerRule && !tempRevealed) {
    if (blockedStickerMode === "hide") {
      return null;
    }
    return (
      <div
        id={`msg-${msg.msg_id}`}
        className="flex justify-start"
        style={{
          marginTop: groupedWithPrev ? "var(--msg-gap-grouped)" : "var(--msg-gap-group)",
        }}
      >
        {!msg.is_outgoing && threadKind === "group" && (
          <div className="mr-2 w-7 shrink-0" />
        )}
        <div
          className={`inline-flex max-w-[78%] flex-wrap items-center gap-2 rounded-full px-3 py-1 text-[12px] ${
            highlighted ? "ring-2 ring-sky-400" : ""
          }`}
          style={{
            background: "var(--bg-surface)",
            border: "1px solid var(--border-subtle)",
            color: "var(--text-secondary)",
          }}
        >
          <EyeOff className="h-3.5 w-3.5 shrink-0 opacity-75" />
          <span className="truncate">{t("blockedStickerHiddenPill")}</span>
          <button
            type="button"
            onClick={() => setTempRevealed(true)}
            className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium transition hover:opacity-85"
            style={{
              background: "var(--chip-bg)",
              color: "var(--text-primary)",
            }}
          >
            <Eye className="h-3 w-3" />
            <span>{t("blockedStickerShowOnce")}</span>
          </button>
          {onUnblockSticker && (
            <button
              type="button"
              onClick={() => onUnblockSticker(effectiveBlockedStickerRule.id)}
              className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium transition hover:opacity-85"
              style={{
                background: "var(--accent-soft)",
                color: "var(--accent)",
              }}
            >
              <span>{t("blockedStickerUnblock")}</span>
            </button>
          )}
          <span className="text-[10.5px] tabular-nums opacity-70">{formatClock(msg.ts)}</span>
        </div>
      </div>
    );
  }

  return (
    <div
      id={`msg-${msg.msg_id}`}
      onClick={selectMode && !msg.send_status ? () => onToggleSelect?.(msg.msg_id) : undefined}
      className={`group relative flex ${msg.is_outgoing ? "justify-end" : "justify-start"} ${
        selectMode ? "cursor-pointer rounded-xl px-1.5 py-0.5 transition" : ""
      }`}
      style={{
        marginTop: groupedWithPrev ? "var(--msg-gap-grouped)" : "var(--msg-gap-group)",
        ...(selectMode && selected ? { background: "var(--accent-soft)" } : undefined),
      }}
    >
      {selectMode && (
        <div className="order-0 mr-2.5 flex items-center self-center">
          <div
            className={`flex h-5 w-5 items-center justify-center rounded-full border transition ${
              msg.send_status ? "opacity-35" : ""
            }`}
            style={{
              background: selected ? "var(--accent)" : "transparent",
              borderColor: selected ? "var(--accent)" : "var(--text-secondary)",
            }}
          >
            {selected && <Check className="h-3 w-3 text-white" />}
          </div>
        </div>
      )}

      {!msg.is_outgoing && threadKind === "group" && (
        <div className="order-1 mb-0.5 mr-2 flex w-7 shrink-0 items-end justify-center self-end">
          {!groupedWithNext && (
            <button
              type="button"
              disabled={!onOpenProfile || !msg.sender_id}
              onClick={() => msg.sender_id && onOpenProfile?.(msg.sender_id)}
              title={t("viewProfileTitle")}
              className="rounded-full transition hover:opacity-85"
            >
              <ThreadAvatar
                avatar={senderAvatar || ""}
                title={msg.sender_name}
                kind="direct"
                size="xs"
              />
            </button>
          )}
        </div>
      )}

      {/* Quick hover actions (left side for outgoing, right side for incoming) */}
      {!selectMode && !msg.is_recalled && !msg.send_status && (
        <div
          onClick={(e) => e.stopPropagation()}
          className={`flex items-center gap-1 self-center opacity-0 transition group-hover:opacity-100 ${
            msg.is_outgoing ? "order-1 mr-2" : "order-3 ml-2"
          }`}
        >
          <button
            onClick={() => onReply(msg)}
            title={t("replyTitle")}
            className="hover-surface rounded-full p-1.5 transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            <CornerUpLeft className="h-3.5 w-3.5" />
          </button>
          {onOpenThread && (
            <button
              onClick={() => onOpenThread(msg)}
              title={replyCount > 0 ? t("viewThreadReplies", { count: replyCount }) : t("openThreadReplies")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{
                background: "var(--bg-surface)",
                color: replyCount > 0 ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              <MessagesSquare className="h-3.5 w-3.5" />
            </button>
          )}
          {onTranslate && hasTranslatableBody && (
            <button
              onClick={() =>
                hasVisibleTranslation ? onHideTranslation?.(msg.msg_id) : onTranslate(msg)
              }
              title={hasVisibleTranslation ? t("hideTranslation") : t("translateMessage")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{
                background: "var(--bg-surface)",
                color: hasVisibleTranslation ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              <Languages className="h-3.5 w-3.5" />
            </button>
          )}
          {onForward && (
            <button
              onClick={() => onForward(msg)}
              title={t("forwardMessage")}
              className="hover-surface rounded-full p-1.5 transition"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Forward className="h-3.5 w-3.5" />
            </button>
          )}
          <div className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                if (pickerPos) {
                  setPickerPos(null);
                  return;
                }
                const rect = e.currentTarget.getBoundingClientRect();
                const pickerW = 240;
                const left = Math.max(
                  12,
                  Math.min(
                    rect.left + rect.width / 2 - pickerW / 2,
                    window.innerWidth - pickerW - 12,
                  ),
                );
                const top =
                  rect.top >= 58
                    ? rect.top - 48
                    : Math.min(window.innerHeight - 52, rect.bottom + 8);
                setMenuPos(null);
                setPickerPos({ left, top });
              }}
              title={t("reactTitle")}
              className="rounded-full p-1.5 hover:opacity-80"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Smile className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      )}

      {pickerPos &&
        typeof document !== "undefined" &&
        createPortal(
          <div
            data-teza-floating-popup="true"
            onClick={(e) => e.stopPropagation()}
            className="fixed z-[80] flex items-center gap-1 rounded-full px-2 py-1 shadow-xl"
            style={{
              left: `${pickerPos.left}px`,
              top: `${pickerPos.top}px`,
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            {QUICK_REACTION_EMOJIS.map((emoji) => {
              const active = visibleReactions.some((r) => r.emoji === emoji && r.me);
              return (
                <button
                  key={emoji}
                  type="button"
                  aria-pressed={active}
                  onClick={() => {
                    onReact(msg, emoji);
                    setPickerPos(null);
                  }}
                  title={active ? t("reactToggleRemoveTitle") : t("reactWithTitle", { emoji })}
                  className="inline-flex items-center justify-center rounded-full p-1 text-sm transition hover:scale-125"
                  style={
                    active
                      ? {
                          background: "rgba(56, 189, 248, 0.24)",
                          outline: "1px solid var(--accent)",
                        }
                      : undefined
                  }
                >
                  <EmojiGlyph emoji={emoji} size={18} />
                </button>
              );
            })}
          </div>,
          document.body,
        )}

      <div
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setPickerPos(null);
          const menuW = hasBlockStickerActions ? 256 : 210;
          const maxMenuH = Math.max(120, window.innerHeight - 16);
          const menuH = Math.min(hasBlockStickerActions ? 470 : 390, maxMenuH);
          setMenuPos({
            x: Math.max(8, Math.min(e.clientX, window.innerWidth - menuW - 8)),
            y: Math.max(8, Math.min(e.clientY, window.innerHeight - menuH - 8)),
          });
        }}
        onDoubleClick={(e) => {
          if (selectMode || msg.is_recalled || msg.send_status) return;
          if ((window.getSelection()?.toString().trim().length ?? 0) > 0) return;
          const el = e.target instanceof Element ? e.target : null;
          if (el?.closest("a, button, input, textarea, audio, video")) return;
          onReply(msg);
        }}
        className={`selectable-text wrap-anywhere relative order-2 min-w-0 max-w-[72%] transition ${
          isPlainSticker
            ? "rounded-2xl px-1 py-0.5"
            : `px-3 shadow-sm ${bubbleCornerClass}`
        } ${highlighted ? "ring-2 ring-sky-400" : ""} ${
          msg.send_status === "sending" ? "opacity-80" : ""
        }`}
        style={{
          paddingBlock: isPlainSticker ? undefined : "var(--bubble-py)",
          background: isPlainSticker
            ? "transparent"
            : msg.is_outgoing
              ? "var(--bg-bubble-out)"
              : "var(--bg-bubble-in)",
          border:
            msg.send_status === "failed"
              ? "1px solid var(--danger-text)"
              : isPlainSticker
                ? "none"
                : msg.is_outgoing
                  ? "1px solid var(--bubble-out-border)"
                  : "1px solid var(--bubble-in-border)",
        }}
      >
        {!msg.is_outgoing && threadKind === "group" && !groupedWithPrev && (
          <button
            type="button"
            disabled={!onOpenProfile || !msg.sender_id}
            onClick={() => msg.sender_id && onOpenProfile?.(msg.sender_id)}
            className="mb-0.5 block text-left text-[12.5px] font-semibold leading-tight hover:underline"
            style={{ color: "var(--accent)" }}
            title={t("viewProfileTitle")}
          >
            {msg.sender_name}
          </button>
        )}

        {!hideQuote && msg.quote && !msg.is_recalled && (
          <button
            type="button"
            onClick={() => msg.quote?.msg_id && onJumpToMsg(msg.quote.msg_id)}
            className="mb-1 block w-full rounded-lg border-l-2 px-2.5 py-1 text-left text-[12.5px] transition hover:opacity-85"
            style={{
              borderColor: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
              background: "var(--quote-bg)",
            }}
          >
            <div
              className="font-semibold"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            >
              {msg.quote.sender_name || t("originalMessageFallback")}
            </div>
            <div className="truncate" style={{ color: "var(--text-secondary)" }}>
              {msg.quote.snippet}
            </div>
          </button>
        )}

        {isSticker && normalizedAttachmentUrl && (
          <RelayImage
            key={mediaRetryKey}
            src={normalizedAttachmentUrl}
            threadId={msg.thread_id}
            msgId={msg.msg_id}
            alt={t("stickerAlt")}
            referrerPolicy="no-referrer"
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 h-32 w-32 object-contain drop-shadow-sm"
            loading="lazy"
          />
        )}

        {isVoice && normalizedAttachmentUrl && (
          <RelayAudio
            key={mediaRetryKey}
            controls
            preload="none"
            src={normalizedAttachmentUrl}
            threadId={msg.thread_id}
            msgId={msg.msg_id}
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 max-w-full"
          />
        )}

        {isVideo && normalizedAttachmentUrl && (
          <RelayVideo
            key={mediaRetryKey}
            controls
            preload="metadata"
            src={normalizedAttachmentUrl}
            threadId={msg.thread_id}
            msgId={msg.msg_id}
            onFallbackFail={() => setMediaError(true)}
            className="mb-1 max-h-72 w-full rounded-xl bg-black/40"
          />
        )}

        {isImage && normalizedAttachmentUrl && (
          <button
            type="button"
            onClick={() =>
              onOpenImage(normalizedAttachmentUrl, msg.attachment_name || t("photoAlt"), {
                threadId: msg.thread_id,
                msgId: msg.msg_id,
              })
            }
            className="relative mb-1 block overflow-hidden rounded-xl"
          >
            <RelayImage
              key={mediaRetryKey}
              src={normalizedAttachmentUrl}
              threadId={msg.thread_id}
              msgId={msg.msg_id}
              alt={msg.attachment_name || t("photoAlt")}
              referrerPolicy="no-referrer"
              onFallbackFail={() => setMediaError(true)}
              className="max-h-72 w-auto rounded-xl object-cover transition hover:brightness-95"
              loading="lazy"
            />
            {msg.send_status === "sending" && (
              <span className="pointer-events-none absolute inset-0 flex items-center justify-center bg-black/30">
                <span className="inline-flex items-center gap-1.5 rounded-full bg-black/70 px-3 py-1 text-xs font-medium text-white shadow">
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  <span>{t("sendingStatus")}</span>
                </span>
              </span>
            )}
          </button>
        )}

        {msg.send_status === "sending" && !normalizedAttachmentUrl && msg.msg_type === "chat.photo" && (
          <div
            className="mb-1 flex items-center gap-2.5 rounded-xl px-3 py-2.5"
            style={{ background: "var(--card-bg)" }}
          >
            <Loader2
              className="h-4 w-4 shrink-0 animate-spin"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium">
                {msg.attachment_name || t("photoAlt")}
              </div>
              <div className="text-[11.5px] opacity-80" style={{ color: "var(--text-secondary)" }}>
                {t("sendingStatus")}
              </div>
            </div>
          </div>
        )}

        {isMediaFailed && (
          <div
            className="mb-1 flex items-center gap-2.5 rounded-xl px-3 py-2"
            style={{ background: "var(--card-bg)" }}
          >
            <ImageIcon
              className="h-4 w-4 shrink-0 opacity-75"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--text-secondary)" }}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium">
                {isStickerKind
                  ? t("stickerAlt")
                  : isVideoKind
                    ? "Video"
                    : isVoiceKind
                      ? lang === "en"
                        ? "Voice message"
                        : lang === "ko"
                          ? "음성 메시지"
                          : "Tin nhắn thoại"
                      : msg.attachment_name || t("photoAlt")}
              </div>
              <div className="text-xs opacity-75" style={{ color: "var(--text-secondary)" }}>
                {lang === "en"
                  ? "Unable to load media"
                  : lang === "ko"
                    ? "미디어를 불러올 수 없습니다"
                    : "Không tải được nội dung media"}
              </div>
            </div>
            <button
              type="button"
              onClick={() => {
                setMediaRetryKey((k) => k + 1);
                setMediaError(false);
              }}
              title={t("reload")}
              className="hover-menu inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            >
              <RefreshCw className="h-3.5 w-3.5" />
              <span>{t("retryShort")}</span>
            </button>
          </div>
        )}

        {isExternalLinkCard && (
          <button
            type="button"
            onClick={() => handleOpenExternalLink(normalizedAttachmentUrl)}
            title={t("openExternalBrowser")}
            className="mb-1 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left transition hover:brightness-95"
            style={{ background: "var(--card-bg)" }}
          >
            <ExternalLink
              className="h-4 w-4 shrink-0"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            />
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13.5px] font-medium">
                {msg.attachment_name || normalizedAttachmentUrl}
              </div>
              <div className="truncate text-xs" style={{ color: "var(--text-secondary)" }}>
                {extractLinkHost(normalizedAttachmentUrl) || t("openExternalBrowser")}
              </div>
            </div>
            <span
              className="rounded-lg p-1.5"
              style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
            >
              <ExternalLink className="h-4 w-4" />
            </span>
          </button>
        )}

        {isFileAttachment &&
          (safeDownloadUrl && onSaveFile ? (
            <button
              type="button"
              onClick={() =>
                onSaveFile(safeDownloadUrl, msg.attachment_name || "download", {
                  threadId: msg.thread_id,
                  msgId: msg.msg_id,
                })
              }
              title={t("clickToDownloadFile")}
              className="mb-1 flex w-full items-center gap-2.5 rounded-xl px-3 py-2 text-left transition hover:brightness-95"
              style={{ background: "var(--card-bg)" }}
            >
              <FileText
                className="h-5 w-5 shrink-0"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px] font-medium">
                  {msg.attachment_name || t("attachedFileTitle")}
                </div>
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  {t("attachedFileHint")}
                </div>
              </div>
              <span
                className="rounded-lg p-1.5"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              >
                <Download className="h-4 w-4" />
              </span>
            </button>
          ) : (
            <div
              className="mb-1 flex items-center gap-2.5 rounded-xl px-3 py-2"
              style={{ background: "var(--card-bg)" }}
            >
              <FileText
                className="h-5 w-5 shrink-0"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px] font-medium">
                  {msg.attachment_name || t("attachedFileTitle")}
                </div>
                <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
                  {t("attachedFileSub")}
                </div>
              </div>
              {safeDownloadUrl && (
                <a
                  href={safeDownloadUrl}
                  target="_blank"
                  rel="noreferrer"
                  download={msg.attachment_name || undefined}
                  title={t("downloadFileTitle")}
                  className="hover-menu rounded-lg p-1.5 transition"
                  style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                >
                  <Download className="h-4 w-4" />
                </a>
              )}
            </div>
          ))}

        {reminderData ? (
          <div
            className="my-0.5 flex min-w-[240px] max-w-[330px] items-stretch gap-3 rounded-xl p-2.5"
            style={{
              background: msg.is_outgoing ? "rgba(0,0,0,0.16)" : "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div
              className="flex w-12 shrink-0 flex-col items-center justify-center rounded-xl py-1.5 text-center"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
            >
              {reminderData.startTime > 0 ? (
                <>
                  <span className="text-[10px] font-bold uppercase tracking-wider">
                    T{new Date(reminderData.startTime).getMonth() + 1}
                  </span>
                  <span className="text-lg font-extrabold leading-tight tabular-nums">
                    {String(new Date(reminderData.startTime).getDate()).padStart(2, "0")}
                  </span>
                </>
              ) : (
                <CalendarClock className="h-5 w-5" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div
                className="flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wider"
                style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
              >
                <CalendarClock className="h-3.5 w-3.5 shrink-0" />
                <span>{t("reminderTitleLabel")}</span>
              </div>
              <div className="mt-0.5 text-[14px] font-semibold leading-snug">
                {reminderData.title}
              </div>
              <div
                className="mt-1 flex flex-wrap items-center gap-1.5 text-[11.5px]"
                style={{ color: "var(--text-secondary)" }}
              >
                {(reminderData.scheduleText || reminderData.startTime > 0) && (
                  <span>
                    {reminderData.scheduleText ||
                      new Date(reminderData.startTime).toLocaleString(locale, {
                        hour: "2-digit",
                        minute: "2-digit",
                        day: "2-digit",
                        month: "2-digit",
                        year: "numeric",
                      })}
                  </span>
                )}
                {reminderData.repeat > 0 && (
                  <span
                    className="inline-flex items-center gap-0.5 rounded-md px-1.5 py-0.5 text-[10.5px] font-medium"
                    style={{ background: "var(--bg-sidebar)" }}
                  >
                    <Repeat className="h-3 w-3" />
                    {reminderData.repeat === 1
                      ? t("repeatDaily")
                      : reminderData.repeat === 2
                        ? t("repeatWeekly")
                        : t("repeatMonthly")}
                  </span>
                )}
              </div>
              {reminderData.startTime > 0 && (
                <div
                  className="mt-1 inline-block rounded-full px-2 py-0.5 text-[10.5px] font-semibold"
                  style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                >
                  {formatReminderCountdown(reminderData.startTime, Date.now(), lang)}
                </div>
              )}
            </div>
          </div>
        ) : pollData ? (
          (() => {
            const isPollClosed = Boolean(
              pollData.closed || (pollData.expired_time > 0 && pollData.expired_time < Date.now()),
            );
            const totalVotes = pollData.options.reduce((s, o) => s + (o.votes || 0), 0);
            const canVote = Boolean(onVotePoll && pollData.poll_id > 0 && !isPollClosed);
            const canAddOption = Boolean(
              !isPollClosed && pollData.allow_add_option && pollData.poll_id > 0 && onAddPollOption,
            );
            const pollCreatorRaw = (pollData as PollDto & { creator_id?: string | number })
              .creator_id;
            const normalizedPollCreator =
              pollCreatorRaw !== undefined && pollCreatorRaw !== null
                ? String(pollCreatorRaw).trim()
                : "";
            const normalizedCurrentUser = (currentUserId ?? "").trim();
            const isPollCreator = Boolean(
              msg.is_outgoing ||
                (normalizedCurrentUser &&
                  ((normalizedPollCreator && normalizedPollCreator === normalizedCurrentUser) ||
                    (msg.sender_id && msg.sender_id.trim() === normalizedCurrentUser))),
            );
            const hasPollManagePermission =
              canManagePolls !== undefined
                ? Boolean(canManagePolls || isPollCreator)
                : normalizedCurrentUser || normalizedPollCreator
                  ? isPollCreator
                  : msg.is_outgoing;
            const canLockPoll = Boolean(
              !isPollClosed && pollData.poll_id > 0 && onLockPoll && hasPollManagePermission,
            );

            const submitNewPollOption = async () => {
              if (!canAddOption || !onAddPollOption || pollBusy) return;
              const trimmed = newPollOption.trim();
              if (!trimmed) return;
              const votedIds = pollData.allow_multi
                ? pollData.options.filter((o) => o.voted).map((o) => o.id)
                : [];
              setPollError(null);
              setPollBusy(true);
              try {
                const res = await onAddPollOption(msg, trimmed, votedIds);
                if (res === false || (typeof res === "string" && res.trim().length > 0)) {
                  setPollError(formatPollErrorMessage(typeof res === "string" ? res : "", lang));
                } else {
                  setNewPollOption("");
                }
              } catch (err) {
                setPollError(formatPollErrorMessage(err, lang));
              } finally {
                setPollBusy(false);
              }
            };

            return (
              <div
                className="my-0.5 min-w-[250px] max-w-[340px] rounded-xl p-2.5"
                style={{
                  background: msg.is_outgoing ? "rgba(0,0,0,0.16)" : "var(--bg-surface)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <div className="flex items-start justify-between gap-2">
                  <div
                    className="flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider"
                    style={{ color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)" }}
                  >
                    <BarChart3 className="h-3.5 w-3.5 shrink-0" />
                    <span>{t("pollTitleLabel")}</span>
                  </div>
                  {onRefreshPoll && pollData.poll_id > 0 && (
                    <button
                      type="button"
                      disabled={pollBusy}
                      onClick={(e) => {
                        e.stopPropagation();
                        if (pollBusy) return;
                        setPollError(null);
                        setPollBusy(true);
                        void Promise.resolve(onRefreshPoll(msg))
                          .then((res) => {
                            if (
                              res === false ||
                              (typeof res === "string" && res.trim().length > 0)
                            ) {
                              if (sparsePollId > 0) {
                                hydratedPollIdsRef.current.delete(sparsePollId);
                              }
                              setPollError(
                                formatPollErrorMessage(
                                  typeof res === "string" ? res : "",
                                  lang,
                                  "load",
                                ),
                              );
                            }
                          })
                          .catch((err) => {
                            if (sparsePollId > 0) {
                              hydratedPollIdsRef.current.delete(sparsePollId);
                            }
                            setPollError(formatPollErrorMessage(err, lang, "load"));
                          })
                          .finally(() => setPollBusy(false));
                      }}
                      title={t("pollRefreshTitle")}
                      className="hover-menu rounded p-1 opacity-75 hover:opacity-100 disabled:opacity-40"
                    >
                      <RefreshCw className={`h-3 w-3 ${pollBusy ? "animate-spin" : ""}`} />
                    </button>
                  )}
                </div>
                <div className="mt-1 text-[14px] font-semibold leading-snug">{pollData.question}</div>
                <div
                  className="mt-1 flex flex-wrap gap-1 text-[10.5px]"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <span>{pollData.allow_multi ? t("pollMultiChoice") : t("pollSingleChoice")}</span>
                  {pollData.is_anonymous && <span>{t("pollAnonymous")}</span>}
                  {isPollClosed && <span className="font-medium text-rose-400">{t("pollClosed")}</span>}
                </div>
                <div className="mt-2 space-y-1.5">
                  {pollData.options.length === 0 ? (
                    <div className="flex items-center gap-1.5 text-xs italic opacity-70">
                      {pollBusy && <Loader2 className="h-3 w-3 animate-spin" />}
                      <span>{pollBusy ? t("pollLoadingOptionsHint") : t("pollNoOptionsHint")}</span>
                    </div>
                  ) : (
                    pollData.options.map((opt) => {
                      const pct = totalVotes > 0 ? Math.round((opt.votes / totalVotes) * 100) : 0;
                      return (
                        <button
                          key={opt.id}
                          type="button"
                          disabled={!canVote || pollBusy}
                          onClick={(e) => {
                            e.stopPropagation();
                            if (!canVote || !onVotePoll || pollBusy) return;
                            const nextIds = pollData.allow_multi
                              ? opt.voted
                                ? pollData.options
                                    .filter((o) => o.voted && o.id !== opt.id)
                                    .map((o) => o.id)
                                : [...pollData.options.filter((o) => o.voted).map((o) => o.id), opt.id]
                              : opt.voted
                                ? []
                                : [opt.id];
                            setPollError(null);
                            setPollBusy(true);
                            void Promise.resolve(onVotePoll(msg, nextIds))
                              .then((res) => {
                                if (
                                  res === false ||
                                  (typeof res === "string" && res.trim().length > 0)
                                ) {
                                  setPollError(
                                    formatPollErrorMessage(
                                      typeof res === "string" ? res : "",
                                      lang,
                                    ),
                                  );
                                }
                              })
                              .catch((err) => {
                                setPollError(formatPollErrorMessage(err, lang));
                              })
                              .finally(() => setPollBusy(false));
                          }}
                          className={`relative block w-full overflow-hidden rounded-lg px-2.5 py-1.5 text-left text-xs transition hover:brightness-95 disabled:cursor-default ${
                            pollBusy ? "opacity-70" : ""
                          }`}
                          style={{
                            background: "var(--bg-sidebar)",
                            border: opt.voted
                              ? "1px solid var(--accent)"
                              : "1px solid var(--border-subtle)",
                          }}
                        >
                          <div
                            aria-hidden="true"
                            className="pointer-events-none absolute inset-y-0 left-0 opacity-20 transition-all"
                            style={{
                              width: `${pct}%`,
                              background: "var(--accent)",
                            }}
                          />
                          <div className="relative flex items-center justify-between gap-2">
                            <span className="flex min-w-0 items-center gap-1.5 font-medium">
                              {opt.voted && (
                                <Check
                                  className="h-3.5 w-3.5 shrink-0"
                                  style={{ color: "var(--accent)" }}
                                />
                              )}
                              <span className="truncate">{opt.content}</span>
                            </span>
                            <span
                              className="shrink-0 text-[11px] font-semibold tabular-nums"
                              style={{ color: "var(--text-secondary)" }}
                            >
                              {opt.votes}
                              {totalVotes > 0 ? ` (${pct}%)` : ""}
                            </span>
                          </div>
                        </button>
                      );
                    })
                  )}
                </div>

                {canAddOption && (
                  <div
                    className="mt-2 flex items-center gap-1.5"
                    onClick={(e) => e.stopPropagation()}
                  >
                    <input
                      type="text"
                      value={newPollOption}
                      disabled={pollBusy}
                      onChange={(e) => setNewPollOption(e.target.value)}
                      onKeyDown={(e) => {
                        if (isImeComposingEvent(e)) return;
                        if (e.key === "Enter") {
                          e.preventDefault();
                          e.stopPropagation();
                          void submitNewPollOption();
                        } else if (e.key === "Escape") {
                          e.preventDefault();
                          e.stopPropagation();
                          setNewPollOption("");
                        }
                      }}
                      placeholder={t("pollAddOptionPlaceholder")}
                      className="min-w-0 flex-1 rounded-lg px-2.5 py-1 text-xs outline-none disabled:opacity-55"
                      style={{
                        background: "var(--bg-sidebar)",
                        border: "1px solid var(--border-subtle)",
                        color: "var(--text-primary)",
                      }}
                    />
                    <button
                      type="button"
                      disabled={pollBusy || !newPollOption.trim()}
                      onClick={(e) => {
                        e.stopPropagation();
                        void submitNewPollOption();
                      }}
                      className="hover-menu inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-xs font-semibold transition disabled:opacity-45"
                      style={{
                        background: "var(--accent-soft)",
                        color: "var(--accent)",
                      }}
                      title={t("addOptionBtn")}
                    >
                      {pollBusy ? (
                        <Loader2 className="h-3 w-3 animate-spin" />
                      ) : (
                        <Plus className="h-3 w-3" />
                      )}
                      <span>{t("pollAddOptionSubmitBtn")}</span>
                    </button>
                  </div>
                )}

                {pollError && (
                  <div
                    role="alert"
                    onClick={(e) => e.stopPropagation()}
                    className="mt-2 flex items-start justify-between gap-2 rounded-lg px-2.5 py-1.5 text-[11px] leading-snug"
                    style={{
                      background: "var(--danger-bg)",
                      color: "var(--danger-text)",
                      border: "1px solid rgba(244, 63, 94, 0.35)",
                    }}
                  >
                    <span className="min-w-0 break-words">{pollError}</span>
                    <button
                      type="button"
                      onClick={() => setPollError(null)}
                      title={t("close")}
                      aria-label={t("close")}
                      className="shrink-0 rounded p-0.5 opacity-80 transition hover:opacity-100"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                )}

                {(totalVotes > 0 || canLockPoll) && (
                  <div
                    className="mt-2 flex flex-wrap items-center justify-between gap-2 border-t pt-1.5 text-[11px]"
                    style={{
                      borderColor: msg.is_outgoing
                        ? "rgba(255,255,255,0.12)"
                        : "var(--border-subtle)",
                      color: "var(--text-secondary)",
                    }}
                    onClick={(e) => e.stopPropagation()}
                  >
                    <span className="tabular-nums">
                      {totalVotes > 0 ? t("pollTotalVotes", { count: totalVotes }) : ""}
                    </span>
                    {canLockPoll &&
                      (confirmLockPoll ? (
                        <div className="flex items-center gap-1.5">
                          <span className="text-[11px] font-medium text-rose-400">
                            {t("pollLockConfirmPrompt")}
                          </span>
                          <button
                            type="button"
                            disabled={pollBusy}
                            onClick={() => {
                              if (!onLockPoll || pollBusy) return;
                              setPollError(null);
                              setPollBusy(true);
                              void Promise.resolve(onLockPoll(msg))
                                .then((res) => {
                                  if (
                                    res === false ||
                                    (typeof res === "string" && res.trim().length > 0)
                                  ) {
                                    setConfirmLockPoll(false);
                                    setPollError(
                                      formatPollErrorMessage(
                                        typeof res === "string" ? res : "",
                                        lang,
                                      ),
                                    );
                                  } else {
                                    setConfirmLockPoll(false);
                                  }
                                })
                                .catch((err) => {
                                  setConfirmLockPoll(false);
                                  setPollError(formatPollErrorMessage(err, lang));
                                })
                                .finally(() => setPollBusy(false));
                            }}
                            className="inline-flex items-center gap-1 rounded-md bg-rose-600 px-2 py-0.5 text-[11px] font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
                          >
                            {pollBusy ? (
                              <Loader2 className="h-3 w-3 animate-spin" />
                            ) : (
                              <Lock className="h-3 w-3" />
                            )}
                            <span>{t("pollLockConfirmBtn")}</span>
                          </button>
                          <button
                            type="button"
                            disabled={pollBusy}
                            onClick={() => setConfirmLockPoll(false)}
                            className="hover-menu rounded-md p-0.5 transition disabled:opacity-45"
                            title={t("cancel")}
                          >
                            <X className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      ) : (
                        <button
                          type="button"
                          disabled={pollBusy}
                          onClick={() => setConfirmLockPoll(true)}
                          className="hover-menu inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11px] font-medium transition hover:text-rose-400 disabled:opacity-45"
                        >
                          <Lock className="h-3 w-3" />
                          <span>{t("pollLockBtn")}</span>
                        </button>
                      ))}
                  </div>
                )}
              </div>
            );
          })()
        ) : callData ? (
          <div
            className="my-0.5 flex min-w-[220px] max-w-[320px] items-center gap-3 rounded-xl px-3 py-2"
            style={{
              background: msg.is_outgoing ? "rgba(0,0,0,0.16)" : "var(--bg-surface)",
              border: "1px solid var(--border-subtle)",
            }}
          >
            <div
              className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full"
              style={{
                background:
                  callData.outcome === "missed" || callData.outcome === "declined"
                    ? "var(--danger-bg)"
                    : "var(--accent-soft)",
                color:
                  callData.outcome === "missed" || callData.outcome === "declined"
                    ? "var(--danger-text)"
                    : "var(--accent)",
              }}
            >
              {callData.callType === "video" ? (
                <Video className="h-4 w-4" />
              ) : callData.outcome === "missed" ? (
                <PhoneMissed className="h-4 w-4" />
              ) : msg.is_outgoing ? (
                <PhoneOutgoing className="h-4 w-4" />
              ) : (
                <PhoneIncoming className="h-4 w-4" />
              )}
            </div>
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13.5px] font-semibold">
                {callData.callType === "video"
                  ? t("callCardVideoTitle")
                  : t("callCardVoiceTitle")}
              </div>
              <div
                className="text-xs tabular-nums"
                style={{
                  color:
                    callData.outcome === "missed" || callData.outcome === "declined"
                      ? "var(--danger-text)"
                      : "var(--text-secondary)",
                }}
              >
                {callData.durationSecs > 0
                  ? t("callCardCompleted", {
                      duration: formatCallDuration(callData.durationSecs),
                    })
                  : callData.outcome === "missed"
                    ? t("callCardMissed")
                    : callData.outcome === "cancelled"
                      ? t("callCardCancelled")
                      : callData.outcome === "declined"
                        ? t("callCardDeclined")
                        : t("callCardVoiceTitle")}
              </div>
            </div>
            {threadKind === "direct" && onStartVoiceCall && (
              <button
                type="button"
                disabled={Boolean(callDisabledReason)}
                onClick={() =>
                  onStartVoiceCall(
                    msg.thread_id,
                    msg.is_outgoing ? undefined : msg.sender_name,
                  )
                }
                title={
                  callDisabledReason === "relay_enabled"
                    ? t("callDisabledRelayTooltip")
                    : callDisabledReason === "busy"
                      ? t("callAlreadyActiveTooltip")
                      : t("callCardCallBack")
                }
                className="hover-menu inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-xs font-semibold transition disabled:cursor-not-allowed disabled:opacity-45"
                style={{
                  background: "var(--bg-sidebar)",
                  border: "1px solid var(--border-subtle)",
                  color: "var(--accent)",
                }}
              >
                <Phone className="h-3.5 w-3.5" />
                <span>{t("callCardCallBack")}</span>
              </button>
            )}
          </div>
        ) : isBuzz ? (
          <div className="my-0.5 inline-flex items-center gap-2 rounded-xl bg-amber-500/20 px-3 py-1.5 font-extrabold tracking-wider text-amber-400 ring-1 ring-amber-400/40">
            <Zap className="h-4 w-4 animate-pulse fill-amber-400 text-amber-300" />
            <span className="text-[16px]">BUZZ!</span>
          </div>
        ) : (
          displayText && (
            <div
              className={`msg-body-text whitespace-pre-wrap wrap-anywhere ${
                isPureEmoticon ? "py-0.5 leading-none" : "leading-[1.38]"
              } ${
                msg.is_recalled ? "italic opacity-65" : ""
              } ${replaceWithTranslation && translation?.loading ? "opacity-75" : ""}`}
            >
              {renderLinkedText(
                displayText,
                msg.is_outgoing,
                emojiStyle,
                mentionMembers,
                onOpenProfile,
                selectMode,
                t("viewMemberProfileTitle"),
                highlightQuery,
                isPureEmoticon,
                threadKind === "group",
              )}
              {useInlineMeta && (
                <span
                  aria-hidden="true"
                  className="pointer-events-none inline-block h-[11px] select-none align-baseline"
                  style={{ width: `${inlineMetaWidthPx}px` }}
                />
              )}
            </div>
          )
        )}

        {hasTranslationPanel && translation && (
          <div
            className="mt-2 border-t pt-1.5"
            style={{
              borderColor: msg.is_outgoing
                ? "rgba(255,255,255,0.22)"
                : "var(--border)",
            }}
          >
            <div className="mb-0.5 flex items-center justify-between gap-2 text-xs opacity-85">
              <span className="flex items-center gap-1 font-medium">
                <Languages className="h-3.5 w-3.5" />
                {translation.loading
                  ? t("translatingDots")
                  : translation.error
                    ? t("translateError")
                    : translation.note
                      ? t("translateHeader")
                      : `${t("translateHeader")}${translation.src ? ` · ${translation.src}` : ""}${
                          translation.targetLang ? ` → ${translation.targetLang}` : ""
                        }`}
              </span>
              {replaceWithTranslation && hasVisibleTranslation ? (
                <button
                  type="button"
                  onClick={() => setShowOriginal(false)}
                  className="hover:underline"
                >
                  {t("viewTranslation")}
                </button>
              ) : (
                onHideTranslation &&
                !translation.loading && (
                  <button
                    type="button"
                    onClick={() => onHideTranslation(msg.msg_id)}
                    className="hover:underline"
                  >
                    {t("hideTranslation")}
                  </button>
                )
              )}
            </div>
            {translation.loading ? (
              <div className="flex items-center gap-1.5 text-[13.5px] opacity-80">
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                <span>{t("translatingBody")}</span>
              </div>
            ) : translation.error ? (
              <div
                className="text-[13px]"
                style={{ color: msg.is_outgoing ? "#fecaca" : "var(--danger-text)" }}
              >
                {translation.error}
              </div>
            ) : translation.note ? (
              <div
                className="text-[13px] opacity-85"
                style={{
                  color: msg.is_outgoing ? "rgba(255,255,255,0.86)" : "var(--text-secondary)",
                }}
              >
                {translation.note}
              </div>
            ) : (
              <div className="whitespace-pre-wrap wrap-anywhere text-[15px] leading-[1.5] opacity-95">
                {translation.text}
              </div>
            )}
          </div>
        )}

        {useInlineMeta ? (
          <span
            className="absolute right-2.5 select-none"
            style={{ bottom: "var(--bubble-py)" }}
          >
            {timestampEl}
          </span>
        ) : (
          <div className="mt-0.5 flex items-center justify-end gap-2">
            {visibleReactions.length > 0 && (
              <div className="mr-auto flex flex-wrap gap-1">
                {visibleReactions.map((r) => (
                  <button
                    key={r.emoji}
                    type="button"
                    disabled={!canInteractReactions}
                    aria-pressed={r.me}
                    title={
                      r.me
                        ? t("reactToggleRemoveTitle")
                        : t("reactWithTitle", { emoji: r.emoji })
                    }
                    onClick={(e) => {
                      e.stopPropagation();
                      if (!canInteractReactions) return;
                      onReact(msg, r.emoji);
                    }}
                    className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[13px] transition hover:opacity-85 disabled:cursor-default disabled:hover:opacity-100"
                    style={{
                      background: r.me ? "var(--accent-soft)" : "var(--chip-bg)",
                      border: r.me ? "1px solid var(--accent)" : "1px solid var(--chip-border)",
                      color: r.me ? "var(--accent)" : undefined,
                      fontWeight: r.me ? 600 : undefined,
                    }}
                  >
                    <EmojiGlyph emoji={r.emoji} size={16} animated={false} />
                    <span>{r.count}</span>
                  </button>
                ))}
              </div>
            )}
            {replaceWithTranslation && translation?.loading && (
              <span
                className="flex items-center gap-1 text-[11px] opacity-80"
                style={{ color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-3 w-3 animate-spin" />
                {t("translatingDots")}
              </span>
            )}
            {isReplaced && (
              <button
                type="button"
                onClick={() => setShowOriginal(true)}
                title={t("viewOrigTextTitle")}
                className="flex items-center gap-1 text-[11px] opacity-75 hover:opacity-100 hover:underline"
                style={{ color: "var(--text-secondary)" }}
              >
                <Languages className="h-3 w-3" />
                <span>{t("translatedOrigLabel")}</span>
              </button>
            )}
            {effectiveBlockedStickerRule && tempRevealed && (
              <button
                type="button"
                onClick={() => setTempRevealed(false)}
                className="flex items-center gap-1 rounded-full px-1.5 py-0.5 text-[11px] opacity-80 hover:opacity-100 hover:underline"
                style={{
                  background: isPlainSticker ? "var(--pill-bg)" : undefined,
                  color: isPlainSticker ? "var(--pill-text)" : "var(--text-secondary)",
                }}
              >
                <EyeOff className="h-3 w-3" />
                <span>{t("blockedStickerHideAgain")}</span>
              </button>
            )}
            {timestampEl}
          </div>
        )}

        {msg.send_status === "failed" && (
          <div className="mt-1.5 flex items-center justify-end gap-2 text-xs">
            <span style={{ color: "var(--danger-text)" }}>
              {msg.send_error || t("sendErrorFallback")}
            </span>
            {onRetrySend && (
              <button
                type="button"
                onClick={() => onRetrySend(msg)}
                className="font-semibold underline hover:opacity-80"
                style={{ color: "var(--accent)" }}
              >
                {t("retryShort")}
              </button>
            )}
            {onDiscardFailed && (
              <button
                type="button"
                onClick={() => onDiscardFailed(msg.msg_id)}
                className="opacity-75 underline hover:opacity-100"
              >
                {t("delete")}
              </button>
            )}
          </div>
        )}

        {replyCount > 0 && onOpenThread && (
          <button
            type="button"
            onClick={() => onOpenThread(msg)}
            className="mt-1.5 -mx-1 flex w-[calc(100%+0.5rem)] items-center justify-between gap-3 rounded-xl border-t px-2 py-1.5 text-left text-[13px] font-medium transition hover:bg-white/5"
            style={{
              borderColor: msg.is_outgoing ? "rgba(255,255,255,0.16)" : "var(--border-subtle)",
              color: msg.is_outgoing ? "var(--link-out)" : "var(--accent)",
            }}
          >
            <span className="flex items-center gap-1.5">
              <MessagesSquare className="h-3.5 w-3.5 shrink-0" />
              <span>{t("replyCountLabel", { count: replyCount })}</span>
            </span>
            <span className="text-xs opacity-85">{t("viewThreadLink")}</span>
          </button>
        )}

        {/* Fixed viewport-clamped Context menu */}
        {menuPos &&
          typeof document !== "undefined" &&
          createPortal(
            <div
              data-teza-floating-popup="true"
              onClick={(e) => e.stopPropagation()}
              className={`fixed z-[80] max-h-[calc(100vh-16px)] overflow-y-auto rounded-xl py-1 shadow-2xl ${
                hasBlockStickerActions ? "w-60" : "w-52"
              }`}
              style={{
                left: `${menuPos.x}px`,
                top: `${menuPos.y}px`,
                background: "var(--bg-sidebar)",
                border: "1px solid var(--border-subtle)",
              }}
            >
            {!msg.is_recalled && !msg.send_status && (
              <button
                onClick={() => {
                  onReply(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <CornerUpLeft className="h-3.5 w-3.5" /> {t("replyTitle")}
              </button>
            )}
            {!msg.is_recalled && !msg.send_status && onOpenThread && (
              <button
                onClick={() => {
                  onOpenThread(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <MessagesSquare className="h-3.5 w-3.5" />{" "}
                {replyCount > 0 ? t("viewThreadReplies", { count: replyCount }) : t("openThreadReplies")}
              </button>
            )}
            {!msg.is_recalled &&
              !msg.send_status &&
              onTranslate &&
              hasTranslatableBody && (
                <button
                  onClick={() => {
                    if (replaceWithTranslation && hasVisibleTranslation) {
                      setShowOriginal((v) => !v);
                    } else if (hasVisibleTranslation) {
                      onHideTranslation?.(msg.msg_id);
                    } else {
                      onTranslate(msg);
                    }
                    setMenuPos(null);
                  }}
                  className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                >
                  <Languages className="h-3.5 w-3.5" />{" "}
                  {replaceWithTranslation && hasVisibleTranslation
                    ? showOriginal
                      ? t("showTranslationOnly")
                      : t("viewOriginalTextMenu")
                    : hasVisibleTranslation
                      ? t("hideTranslation")
                      : t("translateMessage")}
                </button>
              )}
            {!msg.is_recalled && !msg.send_status && onTogglePin && (
              <button
                onClick={() => {
                  onTogglePin(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Pin className="h-3.5 w-3.5" />{" "}
                {msg.is_pinned ? t("unpinMessage") : t("pinMessage")}
              </button>
            )}
            {!msg.is_recalled && !msg.send_status && onForward && (
              <button
                onClick={() => {
                  onForward(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Forward className="h-3.5 w-3.5" /> {t("forwardMessage")}
              </button>
            )}
            {!msg.send_status && onStartSelect && (
              <button
                onClick={() => {
                  onStartSelect(msg.msg_id);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <CheckSquare className="h-3.5 w-3.5" /> {t("selectMultiple")}
              </button>
            )}
            {canSaveMedia && (
              <button
                onClick={handleSaveMedia}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Download className="h-3.5 w-3.5" /> {saveMediaLabel}
              </button>
            )}
            {canCopyImage && (
              <button
                onClick={handleCopyImage}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <ImageIcon className="h-3.5 w-3.5" /> {t("copyImage")}
              </button>
            )}
            <button
              onClick={handleCopy}
              className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
            >
              <Copy className="h-3.5 w-3.5" /> {t("copyText")}
            </button>
            {(hasBlockStickerActions || (effectiveBlockedStickerRule && tempRevealed)) && (
              <div
                className="my-1 border-t"
                style={{ borderColor: "var(--border-subtle)" }}
              />
            )}
            {effectiveBlockedStickerRule && tempRevealed && (
              <>
                <button
                  onClick={() => {
                    setTempRevealed(false);
                    setMenuPos(null);
                  }}
                  className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                >
                  <EyeOff className="h-3.5 w-3.5 shrink-0" /> {t("blockedStickerHideAgain")}
                </button>
                {onUnblockSticker && (
                  <button
                    onClick={() => {
                      onUnblockSticker(effectiveBlockedStickerRule.id);
                      setMenuPos(null);
                    }}
                    className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                    style={{ color: "var(--accent)" }}
                  >
                    <Eye className="h-3.5 w-3.5 shrink-0" /> {t("blockedStickerUnblock")}
                  </button>
                )}
              </>
            )}
            {canBlockStickerSingle && onBlockSticker && (
              <>
                <button
                  onClick={() => {
                    onBlockSticker(msg, { kind: "sticker", global: false });
                    setMenuPos(null);
                  }}
                  className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                >
                  <EyeOff className="h-3.5 w-3.5 shrink-0" /> {t("blockedStickerHideInChat")}
                </button>
                <button
                  onClick={() => {
                    onBlockSticker(msg, { kind: "sticker", global: true });
                    setMenuPos(null);
                  }}
                  className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
                >
                  <Ban className="h-3.5 w-3.5 shrink-0" /> {t("blockedStickerHideEverywhere")}
                </button>
              </>
            )}
            {canBlockStickerCategory && onBlockSticker && (
              <button
                onClick={() => {
                  onBlockSticker(msg, { kind: "category", global: false });
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px]"
              >
                <Ban className="h-3.5 w-3.5 shrink-0" /> {t("blockedStickerHidePackInChat")}
              </button>
            )}
            {msg.is_outgoing && !msg.is_recalled && !msg.send_status && (
              <button
                onClick={() => {
                  onRecall(msg);
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-amber-500"
              >
                <RotateCcw className="h-3.5 w-3.5" /> {t("recallMessage")}
              </button>
            )}
            {(!msg.send_status || msg.send_status === "failed" || !discardDisabled) && (
              <button
                onClick={() => {
                  if (msg.send_status && onDiscardFailed) {
                    onDiscardFailed(msg.msg_id);
                  } else {
                    onDeleteLocal(msg);
                  }
                  setMenuPos(null);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-1.5 text-left text-[13.5px] text-rose-500"
              >
                <Trash2 className="h-3.5 w-3.5" /> {t("deleteOnDevice")}
              </button>
            )}
          </div>,
            document.body,
          )}
      </div>
    </div>
  );
});
