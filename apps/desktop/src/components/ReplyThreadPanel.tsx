import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Info, LocateFixed, MessagesSquare } from "lucide-react";
import { useI18n } from "../i18n";
import type { MessageDto, PendingAttachment, StickerItem, ThreadKind, ThreadMemberDto } from "../types";
import {
  matchBlockedStickerRule,
  type BlockedStickerDisplayMode,
  type BlockedStickerKind,
  type BlockedStickerRule,
} from "../utils/blockedStickers";
import type { MentionCandidate } from "../utils/mentions";
import { isImeComposingEvent } from "../utils/searchNormalize";
import { formatDayLabel } from "../utils/threadState";
import { Composer } from "./Composer";
import { MessageBubble, type MessageTranslationState } from "./MessageBubble";
import { RelayImage } from "./RelayImage";

function dayKey(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

interface Props {
  threadTitle: string;
  rootMessage: MessageDto;
  replies: MessageDto[];
  threadKind: ThreadKind;
  peerReadTs?: number;
  pendingAttachments: PendingAttachment[];
  onAddPendingAttachments: (atts: PendingAttachment[]) => void;
  onRemovePendingAttachment: (index: number) => void;
  onClearPendingAttachments: () => void;
  onEditPendingImage?: (attachmentId?: string) => void;
  draft?: string;
  onDraftChange?: (val: string, threadKey?: string) => void;
  globalPasteFallback?: boolean;
  isLocked?: boolean;
  translationsByMsg: Record<string, MessageTranslationState>;
  replaceWithTranslation: boolean;
  drawerOpen: boolean;
  onToggleDrawer: () => void;
  onClose: () => void;
  onJumpToMsgInMainChat: (msgId: string) => void;
  onSendThreadReply: (text: string, quoteTarget: MessageDto) => Promise<boolean | void>;
  onSendThreadAttachment: (att: PendingAttachment, caption: string, rootMsgId: string) => Promise<boolean>;
  onBuzz: () => Promise<void>;
  buzzCooldownSec: number;
  sendOnEnter?: boolean;
  stickerSuggestEnabled?: boolean;
  hasBlockingOverlay?: boolean;
  getSenderAvatar?: (senderId: string) => string | undefined;
  onSaveFile?: (url: string, name: string, opts?: { threadId?: string; msgId?: string }) => void;
  onRetrySend?: (msg: MessageDto) => void;
  onDiscardFailed?: (msgId: string) => void;
  activeUploadIds?: ReadonlySet<string>;
  onReact: (msg: MessageDto, emoji: string) => void;
  onRecall: (msg: MessageDto) => void;
  onDeleteLocal: (msg: MessageDto) => void;
  onOpenImage: (url: string, title: string, opts?: { threadId?: string; msgId?: string }) => void;
  onOpenProfile: (userId: string) => void;
  onForward: (msg: MessageDto) => void;
  onTranslate: (msg: MessageDto) => void;
  onHideTranslation: (msgId: string) => void;
  mentionMembers?: ThreadMemberDto[];
  displayMentionMembers?: MentionCandidate[];
  onSendSticker?: (sticker: StickerItem) => Promise<void>;
  onTogglePin?: (msg: MessageDto) => void;
  blockedStickerRules?: readonly BlockedStickerRule[];
  blockedStickerMode?: BlockedStickerDisplayMode;
  onBlockSticker?: (
    msg: MessageDto,
    opts: { kind: BlockedStickerKind; global: boolean },
  ) => void;
  onUnblockSticker?: (ruleId: string) => void;
  onError: (err: unknown) => void;
}

export function ReplyThreadPanel({
  threadTitle,
  rootMessage,
  replies,
  threadKind,
  peerReadTs,
  pendingAttachments,
  onAddPendingAttachments,
  onRemovePendingAttachment,
  onClearPendingAttachments,
  onEditPendingImage,
  draft,
  onDraftChange,
  globalPasteFallback = true,
  isLocked = false,
  translationsByMsg,
  replaceWithTranslation,
  drawerOpen,
  onToggleDrawer,
  onClose,
  onJumpToMsgInMainChat,
  onSendThreadReply,
  onSendThreadAttachment,
  onBuzz,
  buzzCooldownSec,
  sendOnEnter = true,
  stickerSuggestEnabled = true,
  hasBlockingOverlay = false,
  getSenderAvatar,
  onSaveFile,
  onRetrySend,
  onDiscardFailed,
  activeUploadIds,
  onReact,
  onRecall,
  onDeleteLocal,
  onOpenImage,
  onOpenProfile,
  onForward,
  onTranslate,
  onHideTranslation,
  mentionMembers,
  displayMentionMembers,
  onSendSticker,
  onTogglePin,
  blockedStickerRules,
  blockedStickerMode = "collapse",
  onBlockSticker,
  onUnblockSticker,
  onError,
}: Props) {
  const { lang, t } = useI18n();
  const [replyTarget, setReplyTarget] = useState<MessageDto>(rootMessage);
  const [replyFocusSeq, setReplyFocusSeq] = useState(0);
  const [threadHighlightId, setThreadHighlightId] = useState<string | null>(null);
  const listContainerRef = useRef<HTMLDivElement | null>(null);
  const listBottomRef = useRef<HTMLDivElement | null>(null);
  const prevCountRef = useRef(replies.length);
  const highlightTimerRef = useRef<number | null>(null);

  const visibleReplies = useMemo(() => {
    if (
      blockedStickerMode !== "hide" ||
      !blockedStickerRules ||
      blockedStickerRules.length === 0
    ) {
      return replies;
    }
    return replies.filter(
      (m) =>
        m.is_outgoing ||
        m.is_recalled ||
        matchBlockedStickerRule(m, blockedStickerRules) === null,
    );
  }, [replies, blockedStickerMode, blockedStickerRules]);

  useEffect(() => {
    return () => {
      if (highlightTimerRef.current !== null) {
        window.clearTimeout(highlightTimerRef.current);
        highlightTimerRef.current = null;
      }
    };
  }, []);

  const handleJumpSmart = (msgId: string) => {
    if (replies.some((r) => r.msg_id === msgId) && listContainerRef.current) {
      const el = listContainerRef.current.querySelector<HTMLElement>(`#msg-${CSS.escape(msgId)}`);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "center" });
        setThreadHighlightId(msgId);
        if (highlightTimerRef.current !== null) {
          window.clearTimeout(highlightTimerRef.current);
        }
        highlightTimerRef.current = window.setTimeout(() => {
          highlightTimerRef.current = null;
          setThreadHighlightId(null);
        }, 1600);
        return;
      }
    }
    onJumpToMsgInMainChat(msgId);
  };

  const rootMessageRef = useRef(rootMessage);
  rootMessageRef.current = rootMessage;

  useEffect(() => {
    setReplyTarget(rootMessageRef.current);
  }, [rootMessage.msg_id]);

  const effectiveReplyTarget = useMemo(() => {
    if (replyTarget.msg_id === rootMessage.msg_id) {
      return rootMessage;
    }
    return replies.find((r) => r.msg_id === replyTarget.msg_id) ?? replyTarget;
  }, [replyTarget, rootMessage, replies]);

  useEffect(() => {
    setReplyFocusSeq((n) => n + 1);
  }, [rootMessage.msg_id]);

  useEffect(() => {
    if (isLocked) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (
        e.key !== "Escape" ||
        e.defaultPrevented ||
        hasBlockingOverlay ||
        isImeComposingEvent(e)
      ) {
        return;
      }
      if (document.querySelector("[data-teza-floating-popup]")) return;
      e.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose, hasBlockingOverlay, isLocked]);

  useEffect(() => {
    if (replies.length > prevCountRef.current) {
      listBottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevCountRef.current = replies.length;
  }, [replies.length]);

  const participantCount = useMemo(() => {
    const ids = new Set<string>();
    if (rootMessage.sender_id || rootMessage.sender_name) {
      ids.add(rootMessage.sender_id || rootMessage.sender_name);
    }
    for (const r of replies) {
      ids.add(r.sender_id || r.sender_name);
    }
    return ids.size;
  }, [rootMessage, replies]);

  const rootTr = translationsByMsg[rootMessage.msg_id];
  const rootText =
    replaceWithTranslation && rootTr?.text && !rootTr.skipped
      ? rootTr.text
      : rootMessage.text || rootMessage.attachment_name;

  const rootTypeLower = rootMessage.msg_type.toLowerCase();
  const rootIsSticker =
    rootTypeLower.includes("sticker") ||
    rootTypeLower === "5" ||
    rootTypeLower === "35" ||
    rootTypeLower === "36";
  const isRootImage =
    !!rootMessage.attachment_url &&
    !rootIsSticker &&
    (rootTypeLower.includes("photo") ||
      rootTypeLower === "2" ||
      rootMessage.attachment_url.startsWith("data:image/") ||
      /\.(png|jpe?g|webp|gif|bmp)(\?|$)/i.test(rootMessage.attachment_url));

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header
        className="flex h-14 shrink-0 items-center justify-between px-4"
        style={{
          background: "var(--bg-sidebar)",
          borderBottom: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex min-w-0 items-center gap-3">
          <button
            type="button"
            onClick={onClose}
            title="Quay lại đoạn chat chính (Esc)"
            className="hover-menu flex items-center gap-1.5 rounded-lg px-2 py-1.5 text-[13.5px] font-medium transition"
            style={{ color: "var(--accent)" }}
          >
            <ArrowLeft className="h-4 w-4" />
            <span>{t("back")}</span>
          </button>
          <div className="h-5 w-px" style={{ background: "var(--border-subtle)" }} />
          <div className="min-w-0">
            <div className="flex items-center gap-1.5">
              <MessagesSquare className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
              <h2 className="truncate text-[15.5px] font-semibold">
                {t("replyThreadTitle", { count: replies.length, title: threadTitle })}
              </h2>
            </div>
            <p className="text-xs" style={{ color: "var(--text-secondary)" }}>
              {t("replyParticipantsCount", { count: participantCount })}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            onClick={() => onJumpToMsgInMainChat(rootMessage.msg_id)}
            title={t("jumpToRootTitle")}
            className="hover-menu flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <LocateFixed className="h-4 w-4" />
            <span>{t("rootMsgBtn")}</span>
          </button>
          <button
            type="button"
            onClick={onToggleDrawer}
            title={t("threadInfoTitle")}
            className="rounded-lg p-2 hover:opacity-80"
            style={{ color: drawerOpen ? "var(--accent)" : "var(--text-secondary)" }}
          >
            <Info className="h-4 w-4" />
          </button>
        </div>
      </header>

      {/* Pinned Root Message Banner */}
      <div
        className="shrink-0 px-6 py-3"
        style={{
          background: "var(--bg-surface)",
          borderBottom: "1px solid var(--border-subtle)",
        }}
      >
        <div
          className="flex items-start gap-3 border-l-2 pl-3"
          style={{ borderColor: "var(--accent)" }}
        >
          {isRootImage && rootMessage.attachment_url && (
            <button
              type="button"
              onClick={() =>
                onOpenImage(
                  rootMessage.attachment_url!,
                  rootMessage.attachment_name || t("photoAlt"),
                  { threadId: rootMessage.thread_id, msgId: rootMessage.msg_id },
                )
              }
              className="h-12 w-12 shrink-0 overflow-hidden rounded-lg"
            >
              <RelayImage
                src={rootMessage.attachment_url}
                threadId={rootMessage.thread_id}
                msgId={rootMessage.msg_id}
                alt={rootMessage.attachment_name || t("photoAlt")}
                referrerPolicy="no-referrer"
                className="h-full w-full object-cover"
              />
            </button>
          )}
          <div className="min-w-0 flex-1">
            <div className="mb-0.5 flex items-center justify-between gap-2 text-xs">
              <button
                type="button"
                disabled={!rootMessage.sender_id}
                onClick={() => rootMessage.sender_id && onOpenProfile(rootMessage.sender_id)}
                className="font-semibold hover:underline"
                style={{ color: "var(--accent)" }}
              >
                {rootMessage.sender_name}
              </button>
              <span style={{ color: "var(--text-secondary)" }}>{t("rootMsgLabel")}</span>
            </div>
            <div className="selectable-text line-clamp-3 whitespace-pre-wrap wrap-anywhere text-[14.5px] leading-relaxed">
              {rootText}
            </div>
          </div>
        </div>
      </div>

      {/* Thread Replies Stream */}
      <div ref={listContainerRef} className="relative flex-1 overflow-y-auto px-5 py-2.5">
        {visibleReplies.length === 0 ? (
          <div
            className="flex h-full flex-col items-center justify-center px-6 text-center text-[14px]"
            style={{ color: "var(--text-secondary)" }}
          >
            {t("noRepliesYetHint")}
          </div>
        ) : (
          visibleReplies.map((m, idx) => {
            const prev = idx > 0 ? visibleReplies[idx - 1] : null;
            const next = idx < visibleReplies.length - 1 ? visibleReplies[idx + 1] : null;
            const showDate = !prev || dayKey(prev.ts) !== dayKey(m.ts);
            const grouped =
              !!prev &&
              !showDate &&
              prev.sender_id === m.sender_id &&
              prev.is_outgoing === m.is_outgoing &&
              m.ts - prev.ts < 300_000;
            const nextShowDate = !next || dayKey(m.ts) !== dayKey(next.ts);
            const groupedWithNext =
              !!next &&
              !nextShowDate &&
              next.sender_id === m.sender_id &&
              next.is_outgoing === m.is_outgoing &&
              next.ts - m.ts < 300_000;
            return (
              <div key={m.msg_id}>
                {showDate && (
                  <div className="my-2.5 flex justify-center">
                    <span
                      className="rounded-full px-3 py-0.5 text-xs font-medium shadow-xs"
                      style={{ background: "var(--pill-bg)", color: "var(--pill-text)" }}
                    >
                      {formatDayLabel(m.ts, lang)}
                    </span>
                  </div>
                )}
                <MessageBubble
                  msg={m}
                  threadKind={threadKind}
                  groupedWithPrev={grouped}
                  groupedWithNext={groupedWithNext}
                  highlighted={threadHighlightId === m.msg_id}
                  isSeen={
                    m.is_outgoing &&
                    !m.is_recalled &&
                    !m.send_status &&
                    (threadKind === "cloud" || (peerReadTs ?? 0) >= m.ts)
                  }
                  senderAvatar={getSenderAvatar?.(m.sender_id)}
                  translation={translationsByMsg[m.msg_id]}
                  replaceWithTranslation={replaceWithTranslation}
                  hideQuote={m.quote?.msg_id === rootMessage.msg_id}
                  onReply={(target) => {
                    setReplyTarget(target);
                    setReplyFocusSeq((n) => n + 1);
                  }}
                  onReact={onReact}
                  onRecall={onRecall}
                  onDeleteLocal={onDeleteLocal}
                  onJumpToMsg={handleJumpSmart}
                  onOpenImage={onOpenImage}
                  onOpenProfile={onOpenProfile}
                  mentionMembers={displayMentionMembers ?? mentionMembers}
                  onForward={onForward}
                  onTranslate={onTranslate}
                  onHideTranslation={onHideTranslation}
                  onTogglePin={onTogglePin}
                  onSaveFile={onSaveFile}
                  onRetrySend={onRetrySend}
                  onDiscardFailed={onDiscardFailed}
                  discardDisabled={activeUploadIds?.has(m.msg_id) ?? false}
                  onCopyError={onError}
                  blockedStickerRule={
                    blockedStickerRules ? matchBlockedStickerRule(m, blockedStickerRules) : null
                  }
                  blockedStickerMode={blockedStickerMode}
                  onBlockSticker={onBlockSticker}
                  onUnblockSticker={onUnblockSticker}
                />
              </div>
            );
          })
        )}
        <div ref={listBottomRef} />
      </div>

      {/* Full Composer inside Thread (text + emoji/stickers + images/files + @mention + buzz) */}
      <Composer
        threadKey={`${rootMessage.thread_id}:thread:${rootMessage.msg_id}`}
        replyTo={
          effectiveReplyTarget.msg_id !== rootMessage.msg_id ? effectiveReplyTarget : null
        }
        focusSignal={replyFocusSeq}
        draft={draft}
        onDraftChange={onDraftChange}
        pendingAttachments={pendingAttachments}
        onAddPendingAttachments={onAddPendingAttachments}
        onRemovePendingAttachment={onRemovePendingAttachment}
        onClearPendingAttachments={onClearPendingAttachments}
        onEditPendingImage={onEditPendingImage}
        mentionMembers={mentionMembers}
        onSendSticker={onSendSticker}
        onCancelReply={() => setReplyTarget(rootMessage)}
        onSendText={async (text) => {
          const ok = await onSendThreadReply(text, effectiveReplyTarget);
          if (ok !== false) {
            setReplyTarget(rootMessage);
          }
          return ok;
        }}
        onSendAttachment={async (att, cap) => {
          return await onSendThreadAttachment(att, cap, rootMessage.msg_id);
        }}
        onBuzz={onBuzz}
        buzzCooldownSec={buzzCooldownSec}
        sendOnEnter={sendOnEnter}
        stickerSuggestEnabled={stickerSuggestEnabled}
        globalPasteFallback={globalPasteFallback && !isLocked && !hasBlockingOverlay}
        isLocked={isLocked}
        placeholder={t("replyInThreadPlaceholder", { name: effectiveReplyTarget.sender_name })}
        onError={onError}
      />
    </div>
  );
}
