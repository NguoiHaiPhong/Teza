import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { BarChart3, CalendarClock, Clock, CornerUpLeft, FileText, Image as ImageIcon, Loader2, Paperclip, Pencil, Scissors, Search, Send, Smile, Sparkles, Users, X, Zap } from "lucide-react";
import { bridge, isTauri, MAX_ATTACHMENT_BYTES } from "../bridge";
import { useI18n, type TranslationKey } from "../i18n";
import type { MessageDto, PendingAttachment, StickerItem, ThreadMemberDto } from "../types";
import { extFromMime, extractAttachmentFromClipboard } from "../utils/clipboardParse";
import { shouldFocusComposerOnReply } from "../utils/downloadState";
import { EMOJI_CATEGORIES, searchEmojis } from "../utils/emojiCatalog";
import { useEmojiStyle } from "../utils/emojiStyleContext";
import {
  extractCompletedMentionSpans,
  filterMentionCandidates,
  findActiveMentionQuery,
  isMentionAllMember,
  reconcileCompletedMentions,
  type CompletedMentionSpan,
} from "../utils/mentions";
import { isImeComposingEvent } from "../utils/searchNormalize";
import {
  convertRetroShortcutsToEmoji,
  RETRO_EMOTICONS,
  searchRetroEmoticons,
} from "../utils/retroEmoticons";
import {
  extractStickerSuggestQuery,
  sharedStickerSuggestCache,
  STICKER_SUGGEST_DEBOUNCE_MS,
  STICKER_SUGGEST_LIMIT,
} from "../utils/stickerSuggest";
import { buildAttachmentSendBatch } from "../utils/outboxState";
import { RelayImage } from "./RelayImage";
import { EmojiGlyph, RetroEmoticon } from "./RetroEmoticon";
import { ThreadAvatar } from "./ThreadAvatar";

const RECENT_EMOJIS_KEY = "teza.recentEmojis";
const MAX_RECENT_EMOJIS = 24;
const STICKER_QUICK_TAGS = ["Haha", "Hi", "Love", "OK", "Thanks", "Cute"];
const STICKER_CATEGORY_TABS = ["Tất cả", "Gần đây", "Gợi ý", "Mặc định"] as const;
const STICKER_CAT_LABEL_KEYS: Record<(typeof STICKER_CATEGORY_TABS)[number], TranslationKey> = {
  "Tất cả": "folderAll",
  "Gần đây": "recentUsedLabel",
  "Gợi ý": "stickerCatSuggested",
  "Mặc định": "stickerCatDefault",
};
const EMOJI_CAT_LABEL_KEYS: Record<string, TranslationKey> = {
  smileys: "emojiCatSmileys",
  gestures: "emojiCatGestures",
  hearts: "emojiCatHearts",
  nature: "emojiCatNature",
  food: "emojiCatFood",
  work: "emojiCatWork",
  activities: "emojiCatActivities",
};

function readRecentEmojis(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENT_EMOJIS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string").slice(0, MAX_RECENT_EMOJIS) : [];
  } catch {
    return [];
  }
}

function saveRecentEmoji(emoji: string): string[] {
  const next = [emoji, ...readRecentEmojis().filter((item) => item !== emoji)].slice(0, MAX_RECENT_EMOJIS);
  try {
    window.localStorage.setItem(RECENT_EMOJIS_KEY, JSON.stringify(next));
  } catch {
    // Ignore storage quota errors in restricted webviews.
  }
  return next;
}

interface Props {
  threadKey?: string;
  replyTo: MessageDto | null;
  focusSignal?: number;
  draft?: string;
  onDraftChange?: (val: string, threadKey?: string) => void;
  pendingAttachments?: PendingAttachment[];
  onAddPendingAttachments?: (atts: PendingAttachment[]) => void;
  onRemovePendingAttachment?: (index: number) => void;
  onClearPendingAttachments?: () => void;
  mentionMembers?: ThreadMemberDto[];
  onSendSticker?: (sticker: StickerItem) => Promise<unknown>;
  onCancelReply: () => void;
  onSendText: (text: string) => Promise<boolean | void>;
  onSendAttachment?: (att: PendingAttachment, caption: string) => Promise<boolean | void | unknown>;
  onBuzz?: () => Promise<void>;
  onOpenReminder?: () => void;
  onOpenPoll?: () => void;
  onOpenSchedule?: () => void;
  onCaptureScreen?: () => void;
  capturingScreen?: boolean;
  onEditPendingImage?: (attachmentId?: string) => void;
  buzzCooldownSec?: number;
  placeholder?: string;
  sendOnEnter?: boolean;
  stickerSuggestEnabled?: boolean;
  globalPasteFallback?: boolean;
  isLocked?: boolean;
  onError: (err: unknown) => void;
}

const EMPTY_ATTACHMENTS: PendingAttachment[] = [];

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function Composer({
  threadKey,
  replyTo,
  focusSignal = 0,
  draft: controlledDraft,
  onDraftChange,
  pendingAttachments = EMPTY_ATTACHMENTS,
  onAddPendingAttachments,
  onRemovePendingAttachment,
  onClearPendingAttachments,
  mentionMembers = [],
  onSendSticker,
  onCancelReply,
  onSendText,
  onSendAttachment,
  onBuzz,
  onOpenReminder,
  onOpenPoll,
  onOpenSchedule,
  onCaptureScreen,
  capturingScreen = false,
  onEditPendingImage,
  buzzCooldownSec = 0,
  placeholder,
  sendOnEnter = true,
  stickerSuggestEnabled = true,
  globalPasteFallback = false,
  isLocked = false,
  onError,
}: Props) {
  const [draft, setLocalDraft] = useState(controlledDraft ?? "");
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const threadKeyRef = useRef(threadKey);
  const mentionMembersRef = useRef(mentionMembers);
  mentionMembersRef.current = mentionMembers;
  const restoredDraftSeedRef = useRef<string | null>(controlledDraft ? controlledDraft : null);
  const onDraftChangeRef = useRef(onDraftChange);
  onDraftChangeRef.current = onDraftChange;
  const draftTimerRef = useRef<number | null>(null);
  const lastHeightRef = useRef<number>(0);

  const flushDraft = (val: string, targetKey: string | undefined) => {
    if (draftTimerRef.current !== null) {
      window.clearTimeout(draftTimerRef.current);
      draftTimerRef.current = null;
    }
    onDraftChangeRef.current?.(val, targetKey);
  };

  useEffect(() => {
    const prevKey = threadKeyRef.current;
    if (prevKey !== threadKey) {
      if (draftTimerRef.current !== null) {
        flushDraft(draftRef.current, prevKey);
      }
      threadKeyRef.current = threadKey;
      lastHeightRef.current = 0;
      if (textareaRef.current) {
        textareaRef.current.style.height = "";
      }
      const nextDraft = controlledDraft ?? "";
      restoredDraftSeedRef.current = nextDraft || null;
      setLocalDraft(nextDraft);
      setCursorPos(0);
      setEmojiOpen(false);
      setMentionDismissedAt(null);
      setCompletedMentions(
        extractCompletedMentionSpans(nextDraft, mentionMembersRef.current),
      );
      setSuggestedStickers([]);
      setSuggestedForQuery(null);
      setSuggestDismissedFor(null);
      return;
    }
    if (
      controlledDraft !== undefined &&
      controlledDraft !== draftRef.current &&
      draftTimerRef.current === null
    ) {
      const prevVal = draftRef.current;
      setLocalDraft(controlledDraft);
      if (!controlledDraft) {
        restoredDraftSeedRef.current = null;
        setCursorPos(0);
        setMentionDismissedAt(null);
        setCompletedMentions([]);
      } else {
        if (!prevVal) {
          restoredDraftSeedRef.current = controlledDraft;
        }
        setCompletedMentions((prev) => {
          const reconciled = reconcileCompletedMentions(prev, prevVal, controlledDraft);
          if (reconciled.length > 0 || prevVal) return reconciled;
          return extractCompletedMentionSpans(controlledDraft, mentionMembersRef.current);
        });
      }
    }
  }, [threadKey, controlledDraft]);

  useEffect(() => {
    return () => {
      if (draftTimerRef.current !== null) {
        window.clearTimeout(draftTimerRef.current);
        draftTimerRef.current = null;
        onDraftChangeRef.current?.(draftRef.current, threadKeyRef.current);
      }
    };
  }, []);

  const setDraft = (next: string | ((prev: string) => string)) => {
    const prevVal = draftRef.current;
    const val = typeof next === "function" ? next(prevVal) : next;
    restoredDraftSeedRef.current = null;
    setLocalDraft(val);
    draftRef.current = val;
    setCompletedMentions((prev) => reconcileCompletedMentions(prev, prevVal, val));
    if (draftTimerRef.current !== null) {
      window.clearTimeout(draftTimerRef.current);
      draftTimerRef.current = null;
    }
    if (!onDraftChangeRef.current) return;
    if (!val) {
      onDraftChangeRef.current(val, threadKeyRef.current);
      return;
    }
    const scheduledKey = threadKeyRef.current;
    draftTimerRef.current = window.setTimeout(() => {
      draftTimerRef.current = null;
      onDraftChangeRef.current?.(val, scheduledKey);
    }, 700);
  };
  const { t } = useI18n();
  const { emojiStyle } = useEmojiStyle();
  const [sending, setSending] = useState(false);
  const [emojiOpen, setEmojiOpen] = useState(false);
  const [pickerMode, setPickerMode] = useState<"emoji" | "sticker">("emoji");
  const [recentEmojis, setRecentEmojis] = useState<string[]>(() => readRecentEmojis());
  const [emojiTab, setEmojiTab] = useState<string>(() => {
    if (emojiStyle === "retro") return "retro";
    return readRecentEmojis().length > 0 ? "recent" : (EMOJI_CATEGORIES[0]?.id ?? "smileys");
  });
  useEffect(() => {
    if (emojiStyle === "retro") {
      setEmojiTab("retro");
    } else {
      setEmojiTab((cur) =>
        cur === "retro"
          ? readRecentEmojis().length > 0
            ? "recent"
            : (EMOJI_CATEGORIES[0]?.id ?? "smileys")
          : cur,
      );
    }
  }, [emojiStyle]);
  const [emojiQuery, setEmojiQuery] = useState("");
  const [stickers, setStickers] = useState<StickerItem[] | null>(null);
  const [stickersLoading, setStickersLoading] = useState(false);
  const [stickerQuery, setStickerQuery] = useState("");
  const [stickerCategory, setStickerCategory] = useState<(typeof STICKER_CATEGORY_TABS)[number]>("Tất cả");
  const [searchedStickers, setSearchedStickers] = useState<StickerItem[] | null>(null);
  const [stickersSearching, setStickersSearching] = useState(false);
  const [suggestedStickers, setSuggestedStickers] = useState<StickerItem[]>([]);
  const [suggestedForQuery, setSuggestedForQuery] = useState<string | null>(null);
  const [suggestDismissedFor, setSuggestDismissedFor] = useState<string | null>(null);
  const suggestStripRef = useRef<HTMLDivElement | null>(null);
  const [cursorPos, setCursorPos] = useState(0);
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionDismissedAt, setMentionDismissedAt] = useState<number | null>(null);
  const [completedMentions, setCompletedMentions] = useState<CompletedMentionSpan[]>(() =>
    extractCompletedMentionSpans(controlledDraft ?? "", mentionMembers),
  );
  const isComposingRef = useRef(false);
  const mentionListboxRef = useRef<HTMLDivElement | null>(null);
  const emojiPickerRef = useRef<HTMLDivElement | null>(null);
  const emojiButtonRef = useRef<HTMLButtonElement | null>(null);
  const mentionItemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const prevReplyIdRef = useRef<string | null>(replyTo?.msg_id ?? null);
  const prevFocusSignalRef = useRef<number>(focusSignal);

  useEffect(() => {
    const seedDraft = restoredDraftSeedRef.current;
    if (!seedDraft || draftRef.current !== seedDraft || mentionMembers.length === 0) {
      return;
    }
    const seeded = extractCompletedMentionSpans(seedDraft, mentionMembers);
    if (seeded.length === 0) return;
    setCompletedMentions((prev) => {
      if (
        prev.length === seeded.length &&
        prev.every((s, idx) => s.start === seeded[idx].start && s.text === seeded[idx].text)
      ) {
        return prev;
      }
      const byKey = new Map<string, CompletedMentionSpan>();
      for (const s of prev) byKey.set(`${s.start}:${s.text}`, s);
      for (const s of seeded) byKey.set(`${s.start}:${s.text}`, s);
      return Array.from(byKey.values());
    });
  }, [mentionMembers]);

  useEffect(() => {
    const nextReplyId = replyTo?.msg_id ?? null;
    const shouldFocus = shouldFocusComposerOnReply(
      prevReplyIdRef.current,
      nextReplyId,
      prevFocusSignalRef.current,
      focusSignal,
    );
    prevReplyIdRef.current = nextReplyId;
    prevFocusSignalRef.current = focusSignal;
    if (!shouldFocus) return;
    const rafId = window.requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el || el.disabled) return;
      el.focus();
      const len = el.value.length;
      el.setSelectionRange(len, len);
      setCursorPos(len);
    });
    return () => window.cancelAnimationFrame(rafId);
  }, [replyTo?.msg_id, focusSignal]);

  useEffect(() => {
    if (!emojiOpen || isLocked) return;
    const onOutsidePointer = (event: MouseEvent | PointerEvent) => {
      const target = event.target instanceof Node ? event.target : null;
      if (!target) return;
      if (emojiPickerRef.current?.contains(target)) return;
      if (emojiButtonRef.current?.contains(target)) return;
      setEmojiOpen(false);
    };
    const onWindowKeyDown = (event: KeyboardEvent) => {
      if (
        event.key === "Escape" &&
        !event.defaultPrevented &&
        !isImeComposingEvent(event)
      ) {
        event.preventDefault();
        event.stopPropagation();
        setEmojiOpen(false);
      }
    };
    window.addEventListener("pointerdown", onOutsidePointer, true);
    window.addEventListener("mousedown", onOutsidePointer, true);
    window.addEventListener("keydown", onWindowKeyDown, true);
    return () => {
      window.removeEventListener("pointerdown", onOutsidePointer, true);
      window.removeEventListener("mousedown", onOutsidePointer, true);
      window.removeEventListener("keydown", onWindowKeyDown, true);
    };
  }, [emojiOpen, isLocked]);

  useEffect(() => {
    if (!emojiOpen || pickerMode !== "sticker" || stickers !== null || !onSendSticker) return;
    let cancelled = false;
    setStickersLoading(true);
    void bridge
      .listStickers()
      .then((list) => {
        if (!cancelled) setStickers(list);
      })
      .catch((err) => {
        if (!cancelled) {
          setStickers([]);
          onError(err);
        }
      })
      .finally(() => {
        if (!cancelled) setStickersLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [emojiOpen, pickerMode, stickers, onSendSticker, onError]);

  useEffect(() => {
    if (!emojiOpen || pickerMode !== "sticker" || !onSendSticker) return;
    const trimmed = stickerQuery.trim();
    if (!trimmed) {
      setSearchedStickers(null);
      setStickersSearching(false);
      return;
    }
    let cancelled = false;
    setStickersSearching(true);
    const timer = window.setTimeout(() => {
      void bridge
        .searchStickers(trimmed)
        .then((list) => {
          if (!cancelled) setSearchedStickers(list);
        })
        .catch(() => {
          if (!cancelled) setSearchedStickers([]);
        })
        .finally(() => {
          if (!cancelled) setStickersSearching(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [emojiOpen, pickerMode, stickerQuery, onSendSticker]);

  const activeEmojiCategory = useMemo(
    () => EMOJI_CATEGORIES.find((cat) => cat.id === emojiTab) ?? EMOJI_CATEGORIES[0],
    [emojiTab],
  );

  const visibleRetroEmoticons = useMemo(() => {
    const trimmed = emojiQuery.trim();
    if (trimmed) {
      return searchRetroEmoticons(trimmed);
    }
    return emojiTab === "retro" ? [...RETRO_EMOTICONS] : [];
  }, [emojiQuery, emojiTab]);

  const visibleEmojis = useMemo(() => {
    const trimmed = emojiQuery.trim();
    if (trimmed) return searchEmojis(trimmed);
    if (emojiTab === "retro") return [];
    if (emojiTab === "recent") return recentEmojis;
    return activeEmojiCategory ? activeEmojiCategory.items.map((item) => item.emoji) : [];
  }, [emojiQuery, emojiTab, recentEmojis, activeEmojiCategory]);

  const visibleStickers = useMemo(() => {
    if (stickerQuery.trim()) return searchedStickers ?? [];
    if (!stickers) return [];
    if (stickerCategory === "Tất cả") return stickers;
    return stickers.filter((s) => (s.category ?? "Mặc định") === stickerCategory);
  }, [stickerQuery, searchedStickers, stickers, stickerCategory]);

  const insertEmojiAtCaret = (emoji: string) => {
    const input = textareaRef.current;
    const rawStart =
      input && typeof input.selectionStart === "number" ? input.selectionStart : Math.min(cursorPos, draft.length);
    const rawEnd = input && typeof input.selectionEnd === "number" ? input.selectionEnd : rawStart;
    const safeStart = Math.max(0, Math.min(rawStart, draft.length));
    const safeEnd = Math.max(safeStart, Math.min(rawEnd, draft.length));
    const nextText = `${draft.slice(0, safeStart)}${emoji}${draft.slice(safeEnd)}`;
    const nextCursor = safeStart + emoji.length;
    setDraft(nextText);
    setCursorPos(nextCursor);
    setRecentEmojis(saveRecentEmoji(emoji));
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(nextCursor, nextCursor);
      }
    });
  };

  const rawMentionMatch = useMemo(() => {
    if (mentionMembers.length === 0) return null;
    return findActiveMentionQuery(draft, cursorPos);
  }, [draft, cursorPos, mentionMembers.length]);

  const mentionMatch = useMemo(() => {
    if (!rawMentionMatch) return null;
    if (mentionDismissedAt === rawMentionMatch.start) return null;
    const isCompleted = completedMentions.some(
      (span) =>
        span.start === rawMentionMatch.start &&
        cursorPos >= span.start + span.text.length &&
        draft.slice(span.start, span.start + span.text.length) === span.text,
    );
    if (isCompleted) {
      return null;
    }
    return rawMentionMatch;
  }, [rawMentionMatch, mentionDismissedAt, completedMentions, cursorPos, draft]);

  const mentionMatches = useMemo(() => {
    if (!mentionMatch) return [];
    return filterMentionCandidates(mentionMembers, mentionMatch.query, 8);
  }, [mentionMatch, mentionMembers]);

  const mentionListboxId = useId();
  const mentionOpen = mentionMatches.length > 0 && Boolean(mentionMatch);
  const safeMentionIndex = mentionOpen
    ? Math.max(0, Math.min(mentionIndex, mentionMatches.length - 1))
    : -1;
  const activeMentionOptionId =
    safeMentionIndex >= 0 ? `${mentionListboxId}-opt-${safeMentionIndex}` : undefined;

  const hasPendingAttachments = pendingAttachments.length > 0;
  const singlePendingAttachment =
    pendingAttachments.length === 1 ? pendingAttachments[0] : null;
  const totalStagedBytes = useMemo(
    () => pendingAttachments.reduce((acc, att) => acc + att.sizeBytes, 0),
    [pendingAttachments],
  );
  const submittingAttachmentsRef = useRef(false);

  const activeSuggestQuery = useMemo(
    () =>
      extractStickerSuggestQuery(draft, {
        enabled: stickerSuggestEnabled && Boolean(onSendSticker),
        hasPendingAttachment: hasPendingAttachments,
        mentionOpen,
        emojiOpen,
      }),
    [draft, stickerSuggestEnabled, onSendSticker, hasPendingAttachments, mentionOpen, emojiOpen],
  );

  useEffect(() => {
    if (suggestDismissedFor !== null && activeSuggestQuery !== suggestDismissedFor) {
      setSuggestDismissedFor(null);
    }
  }, [activeSuggestQuery, suggestDismissedFor]);

  useEffect(() => {
    if (!activeSuggestQuery || activeSuggestQuery === suggestDismissedFor || !onSendSticker) {
      setSuggestedStickers([]);
      setSuggestedForQuery(null);
      return;
    }
    const cached = sharedStickerSuggestCache.get(activeSuggestQuery);
    if (cached !== undefined) {
      setSuggestedStickers(cached);
      setSuggestedForQuery(activeSuggestQuery);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void bridge
        .suggestStickers(activeSuggestQuery, STICKER_SUGGEST_LIMIT)
        .then((items) => {
          sharedStickerSuggestCache.set(activeSuggestQuery, items);
          if (!cancelled) {
            setSuggestedStickers(items);
            setSuggestedForQuery(activeSuggestQuery);
          }
        })
        .catch(() => {
          if (!cancelled) {
            setSuggestedStickers([]);
            setSuggestedForQuery(null);
          }
        });
    }, STICKER_SUGGEST_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [activeSuggestQuery, suggestDismissedFor, onSendSticker]);

  const visibleSuggestedStickers = useMemo(() => {
    if (
      !activeSuggestQuery ||
      activeSuggestQuery === suggestDismissedFor ||
      suggestedForQuery !== activeSuggestQuery ||
      mentionOpen ||
      emojiOpen
    ) {
      return [];
    }
    return suggestedStickers;
  }, [activeSuggestQuery, suggestDismissedFor, suggestedForQuery, mentionOpen, emojiOpen, suggestedStickers]);

  const handleSendSuggestedSticker = (sticker: StickerItem) => {
    setStickers((prev) =>
      prev
        ? [
            { ...sticker, category: "Gần đây" },
            ...prev.filter((item) => !(item.id === sticker.id && item.cate_id === sticker.cate_id)),
          ]
        : prev,
    );
    setDraft("");
    setCursorPos(0);
    setMentionDismissedAt(null);
    setCompletedMentions([]);
    setSuggestedStickers([]);
    setSuggestedForQuery(null);
    setSuggestDismissedFor(null);
    void onSendSticker?.(sticker);
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
    });
  };

  useEffect(() => {
    if (mentionMatch && mentionMatches.length > 0) {
      setEmojiOpen(false);
    }
  }, [mentionMatch, mentionMatches.length]);

  useEffect(() => {
    if (!rawMentionMatch) {
      if (mentionDismissedAt !== null) setMentionDismissedAt(null);
      return;
    }
    if (mentionDismissedAt !== null && rawMentionMatch.start !== mentionDismissedAt) {
      setMentionDismissedAt(null);
    }
  }, [rawMentionMatch, mentionDismissedAt]);

  useEffect(() => {
    if (!mentionOpen || !mentionMatch || isLocked) return;
    const startAt = mentionMatch.start;
    const onOutsidePointer = (event: MouseEvent | PointerEvent) => {
      const target = event.target instanceof Node ? event.target : null;
      if (!target) return;
      if (mentionListboxRef.current?.contains(target)) return;
      if (textareaRef.current?.contains(target)) return;
      setMentionDismissedAt(startAt);
    };
    const onWindowKeyDown = (event: KeyboardEvent) => {
      if (
        event.key !== "Escape" ||
        event.defaultPrevented ||
        isImeComposingEvent(event)
      ) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      setMentionDismissedAt(startAt);
    };
    window.addEventListener("pointerdown", onOutsidePointer, true);
    window.addEventListener("mousedown", onOutsidePointer, true);
    window.addEventListener("keydown", onWindowKeyDown, true);
    return () => {
      window.removeEventListener("pointerdown", onOutsidePointer, true);
      window.removeEventListener("mousedown", onOutsidePointer, true);
      window.removeEventListener("keydown", onWindowKeyDown, true);
    };
  }, [mentionOpen, mentionMatch, isLocked]);

  useEffect(() => {
    setMentionIndex(0);
  }, [mentionMatch?.start, mentionMatch?.query, mentionMatches.length]);

  useEffect(() => {
    if (!mentionMatch || mentionMatches.length === 0 || safeMentionIndex < 0) return;
    const activeEl = mentionItemRefs.current[safeMentionIndex];
    activeEl?.scrollIntoView({ block: "nearest" });
  }, [safeMentionIndex, mentionMatch, mentionMatches.length]);

  const syncCursorFromEl = (el: HTMLTextAreaElement) => {
    const pos = typeof el.selectionStart === "number" ? el.selectionStart : el.value.length;
    setCursorPos(pos);
  };

  const insertMention = (member: ThreadMemberDto) => {
    if (!mentionMatch) return;
    const currentDraft = draftRef.current;
    let end = Math.max(mentionMatch.start, Math.min(cursorPos, currentDraft.length));
    const tailWord = currentDraft.slice(end).match(/^[\p{L}\p{N}\p{M}_]+/u);
    if (tailWord) {
      end += tailWord[0].length;
    }
    const hasTrailingSpace = end < currentDraft.length && currentDraft[end] === " ";
    const inserted = `@${member.name}${hasTrailingSpace ? "" : " "}`;
    const nextText = `${currentDraft.slice(0, mentionMatch.start)}${inserted}${currentDraft.slice(end)}`;
    const nextCursor = mentionMatch.start + inserted.length + (hasTrailingSpace ? 1 : 0);
    setDraft(nextText);
    setCursorPos(nextCursor);
    setMentionDismissedAt(null);
    setCompletedMentions((prev) => [
      ...prev.filter((span) => span.start !== mentionMatch.start),
      { start: mentionMatch.start, text: `@${member.name} ` },
    ]);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(nextCursor, nextCursor);
      }
    });
  };

  const syncTextareaHeight = (forceMeasure = false) => {
    const el = textareaRef.current;
    if (!el) return;
    const val = el.value;
    if (!val) {
      if (lastHeightRef.current !== 0 || el.style.height !== "") {
        el.style.height = "";
        lastHeightRef.current = 0;
      }
      return;
    }
    if (!forceMeasure && lastHeightRef.current === 0 && !val.includes("\n") && val.length < 28) {
      if (el.style.height !== "") {
        el.style.height = "";
      }
      return;
    }
    el.style.height = "auto";
    const nextH = Math.min(el.scrollHeight, 140);
    if (nextH !== lastHeightRef.current) {
      el.style.height = `${nextH}px`;
      lastHeightRef.current = nextH;
    } else if (lastHeightRef.current > 0) {
      el.style.height = `${lastHeightRef.current}px`;
    }
  };

  useLayoutEffect(() => {
    syncTextareaHeight();
  }, [draft, placeholder, pendingAttachments.length]);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let lastWidth = el.clientWidth;
    const ro = new ResizeObserver(() => {
      const nextWidth = el.clientWidth;
      if (nextWidth === lastWidth) return;
      lastWidth = nextWidth;
      syncTextareaHeight(true);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const attachRawFiles = (files: File[]) => {
    if (!onAddPendingAttachments || !onSendAttachment || files.length === 0) return;
    const staged: PendingAttachment[] = [];
    let firstError: Error | null = null;
    for (const file of files) {
      if (file.size === 0) {
        if (!firstError) {
          firstError = new Error(t("composerAttachmentEmptyError"));
        }
        continue;
      }
      if (file.size > MAX_ATTACHMENT_BYTES) {
        if (!firstError) {
          firstError = new Error(
            t("composerAttachmentTooLargeError", {
              name: file.name,
              size: formatBytes(file.size),
              maxMb: MAX_ATTACHMENT_BYTES / (1024 * 1024),
            }),
          );
        }
        continue;
      }
      const rawName = (file.name || "").trim();
      const hasExt = /\.[A-Za-z0-9]{1,10}$/.test(rawName);
      const fileName = !rawName
        ? `clipboard-${Date.now()}.${extFromMime(file.type)}`
        : !hasExt && file.type.startsWith("image/")
          ? `${rawName}.${extFromMime(file.type)}`
          : rawName;
      const isImage =
        file.type.startsWith("image/") || /\.(png|jpe?g|webp|gif|bmp)$/i.test(fileName);
      const previewUrl = isImage ? URL.createObjectURL(file) : null;
      staged.push({
        id: `raw-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
        kind: "raw",
        file,
        fileName,
        sizeBytes: file.size,
        isImage,
        previewUrl,
      });
    }
    if (staged.length > 0) {
      onAddPendingAttachments(staged);
    }
    if (firstError) {
      onError(firstError);
    }
  };

  const attachRawFilesRef = useRef(attachRawFiles);
  attachRawFilesRef.current = attachRawFiles;

  useEffect(() => {
    if (!globalPasteFallback || isLocked) return;
    const onWindowPaste = (e: ClipboardEvent) => {
      if (e.defaultPrevented) return;
      const active = document.activeElement;
      if (
        active instanceof HTMLElement &&
        (active.tagName === "INPUT" ||
          active.tagName === "TEXTAREA" ||
          active.tagName === "SELECT" ||
          active.isContentEditable)
      ) {
        return;
      }
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest("input, textarea, select, [contenteditable]")) return;

      const file = extractAttachmentFromClipboard(e.clipboardData);
      if (file) {
        e.preventDefault();
        attachRawFilesRef.current([file]);
      }
    };
    window.addEventListener("paste", onWindowPaste);
    return () => window.removeEventListener("paste", onWindowPaste);
  }, [globalPasteFallback, isLocked]);

  const handleAttachClick = async () => {
    if (!onAddPendingAttachments || !onSendAttachment) return;
    if (isTauri()) {
      try {
        const meta = await bridge.pickAttachmentFile();
        if (!meta) return;
        onAddPendingAttachments([
          {
            id: `path-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
            kind: "path",
            path: meta.path,
            fileName: meta.file_name,
            sizeBytes: meta.size_bytes,
            isImage: meta.is_image,
          },
        ]);
      } catch (err) {
        onError(err);
      }
      return;
    }
    fileInputRef.current?.click();
  };

  const handleSubmit = async () => {
    const submitThreadKey = threadKeyRef.current;
    if (hasPendingAttachments && onSendAttachment) {
      if (submittingAttachmentsRef.current) return;
      submittingAttachmentsRef.current = true;
      const rawCap = draft;
      const cap = emojiStyle === "retro" ? convertRetroShortcutsToEmoji(rawCap) : rawCap;
      const batch = buildAttachmentSendBatch(pendingAttachments, cap);
      const enqueuedIndices: number[] = [];
      try {
        for (let i = 0; i < batch.length; i++) {
          const { att, caption } = batch[i];
          try {
            const ok = await onSendAttachment(att, caption);
            if (ok !== false && ok !== null) {
              enqueuedIndices.push(i);
            }
          } catch {
            // Keep un-enqueued attachments intact if optimistic enqueue fails
          }
        }
        if (threadKeyRef.current === submitThreadKey) {
          if (enqueuedIndices.length === batch.length) {
            onClearPendingAttachments?.();
            setDraft("");
            setCursorPos(0);
            setMentionDismissedAt(null);
            setCompletedMentions([]);
          } else if (enqueuedIndices.length > 0) {
            for (let k = enqueuedIndices.length - 1; k >= 0; k--) {
              onRemovePendingAttachment?.(enqueuedIndices[k]);
            }
            if (enqueuedIndices[0] === 0) {
              setDraft("");
              setCursorPos(0);
              setMentionDismissedAt(null);
              setCompletedMentions([]);
            }
          }
        }
      } finally {
        submittingAttachmentsRef.current = false;
      }
      return;
    }

    if (sending) return;
    if (!draft.trim()) return;
    setSending(true);
    const rawText = draft;
    const text = emojiStyle === "retro" ? convertRetroShortcutsToEmoji(rawText) : rawText;
    setDraft("");
    setCursorPos(0);
    setMentionDismissedAt(null);
    setCompletedMentions([]);
    try {
      const ok = await onSendText(text);
      if (ok === false && threadKeyRef.current === submitThreadKey) {
        setDraft((cur) => (cur ? cur : rawText));
      }
    } catch {
      if (threadKeyRef.current === submitThreadKey) {
        setDraft((cur) => (cur ? cur : rawText));
      }
    } finally {
      setSending(false);
    }
  };

  return (
    <footer
      className="shrink-0"
      style={{ background: "var(--bg-sidebar)", borderTop: "1px solid var(--border-subtle)" }}
    >
      {replyTo && (
        <div
          className="flex items-center justify-between gap-2 px-4 py-2 text-[13px]"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-2">
            <CornerUpLeft className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
            <div className="min-w-0 border-l-2 pl-2" style={{ borderColor: "var(--accent)" }}>
              <div className="font-semibold" style={{ color: "var(--accent)" }}>
                {t("replyingTo")} {replyTo.sender_name}
              </div>
              <div className="truncate" style={{ color: "var(--text-secondary)" }}>
                {replyTo.text}
              </div>
            </div>
          </div>
          <button onClick={onCancelReply} className="rounded-lg p-1 hover:opacity-80">
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {singlePendingAttachment ? (
        <div
          className="flex items-center justify-between gap-3 px-4 py-2"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex min-w-0 items-center gap-3">
            {singlePendingAttachment.kind === "raw" && singlePendingAttachment.previewUrl ? (
              <img
                src={singlePendingAttachment.previewUrl}
                alt={singlePendingAttachment.fileName}
                className="h-12 w-12 shrink-0 rounded-lg object-cover"
              />
            ) : (
              <div
                className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg"
                style={{ background: "var(--bg-surface)" }}
              >
                {singlePendingAttachment.isImage ? (
                  <ImageIcon className="h-5 w-5" style={{ color: "var(--accent)" }} />
                ) : (
                  <FileText className="h-5 w-5" style={{ color: "var(--accent)" }} />
                )}
              </div>
            )}
            <div className="min-w-0 text-xs">
              <div className="flex items-center gap-1.5 font-medium">
                {singlePendingAttachment.isImage ? (
                  <ImageIcon className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                ) : (
                  <FileText className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                )}
                <span className="truncate">{singlePendingAttachment.fileName}</span>
                <span className="shrink-0" style={{ color: "var(--text-secondary)" }}>
                  ({formatBytes(singlePendingAttachment.sizeBytes)})
                </span>
              </div>
              <div style={{ color: "var(--text-secondary)" }}>
                {t("enterToNewlineHint")}
              </div>
            </div>
          </div>
          <div className="flex items-center gap-1">
            {singlePendingAttachment.isImage && onEditPendingImage && (
              <button
                type="button"
                onClick={() => onEditPendingImage(singlePendingAttachment.id)}
                title={t("editImageBtn")}
                aria-label={t("editImageBtn")}
                className="hover-surface rounded-lg p-1.5 transition"
                style={{ color: "var(--accent)" }}
              >
                <Pencil className="h-4 w-4" />
              </button>
            )}
            <button
              type="button"
              onClick={() =>
                onRemovePendingAttachment
                  ? onRemovePendingAttachment(0)
                  : onClearPendingAttachments?.()
              }
              title={t("removeStagedAttachmentTitle")}
              aria-label={t("removeStagedAttachmentTitle")}
              className="rounded-lg p-1 hover:opacity-80"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
      ) : hasPendingAttachments ? (
        <div
          className="px-4 py-2"
          style={{ borderBottom: "1px solid var(--border-subtle)" }}
        >
          <div className="flex items-center justify-between gap-2 text-xs">
            <div className="flex min-w-0 items-center gap-2">
              <span
                className="shrink-0 rounded-full px-2 py-0.5 text-[11px] font-semibold"
                style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
              >
                {t("stagedAttachmentsCount", { count: pendingAttachments.length })}
              </span>
              <span className="shrink-0 text-[11.5px]" style={{ color: "var(--text-secondary)" }}>
                ({formatBytes(totalStagedBytes)})
              </span>
              <span
                className="hidden truncate text-[11.5px] sm:inline"
                style={{ color: "var(--text-secondary)" }}
              >
                · {t("stagedAttachmentsCaptionHint")}
              </span>
            </div>
            {onClearPendingAttachments && (
              <button
                type="button"
                onClick={onClearPendingAttachments}
                className="hover-surface shrink-0 rounded-lg px-2 py-1 text-[11.5px] font-medium transition"
                style={{ color: "var(--danger-text)" }}
              >
                {t("clearAllStagedAttachments")}
              </button>
            )}
          </div>
          <div className="mt-2 flex max-h-36 flex-wrap gap-2 overflow-y-auto pr-0.5">
            {pendingAttachments.map((att, idx) => {
              const itemKey =
                att.id ??
                (att.kind === "path"
                  ? `path-${att.path}`
                  : `raw-${att.previewUrl ?? att.fileName}-${idx}`);
              return (
                <div
                  key={itemKey}
                  className="flex max-w-[250px] items-center gap-2 rounded-xl px-2 py-1.5"
                  style={{
                    background: "var(--bg-surface)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  {att.kind === "raw" && att.previewUrl ? (
                    <img
                      src={att.previewUrl}
                      alt={att.fileName}
                      className="h-10 w-10 shrink-0 rounded-lg object-cover"
                    />
                  ) : (
                    <div
                      className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg"
                      style={{ background: "var(--bg-hover)" }}
                    >
                      {att.isImage ? (
                        <ImageIcon className="h-4 w-4" style={{ color: "var(--accent)" }} />
                      ) : (
                        <FileText className="h-4 w-4" style={{ color: "var(--accent)" }} />
                      )}
                    </div>
                  )}
                  <div className="min-w-0 flex-1 text-xs">
                    <div className="truncate font-medium" title={att.fileName}>
                      {att.fileName}
                    </div>
                    <div className="text-[11px]" style={{ color: "var(--text-secondary)" }}>
                      {formatBytes(att.sizeBytes)}
                    </div>
                  </div>
                  {att.isImage && onEditPendingImage && (
                    <button
                      type="button"
                      onClick={() => onEditPendingImage(att.id)}
                      title={t("editImageBtn")}
                      aria-label={t("editImageBtn")}
                      className="hover-menu shrink-0 rounded-md p-1 transition hover:opacity-80"
                      style={{ color: "var(--accent)" }}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => onRemovePendingAttachment?.(idx)}
                    title={t("removeStagedAttachmentTitle")}
                    aria-label={t("removeStagedAttachmentTitle")}
                    className="hover-menu shrink-0 rounded-md p-1 transition hover:opacity-80"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      ) : null}

      <div className="relative flex items-end gap-2 px-4 py-3">
        {visibleSuggestedStickers.length > 0 && (
          <div
            data-teza-floating-popup="true"
            role="region"
            aria-label={t("stickerSuggestHeader")}
            className="absolute bottom-full left-4 right-4 z-20 mb-2 flex items-center gap-2 rounded-2xl px-2.5 py-1.5 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div
              className="flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-[11px] font-semibold"
              style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
              title={t("stickerSuggestHeader")}
            >
              <Sparkles className="h-3.5 w-3.5 shrink-0" />
              <span className="hidden sm:inline">{t("stickerSuggestHeader")}</span>
            </div>
            <div
              ref={suggestStripRef}
              onWheel={(e) => {
                const el = suggestStripRef.current;
                if (el && Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
                  el.scrollLeft += e.deltaY;
                }
              }}
              className="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto py-0.5"
            >
              {visibleSuggestedStickers.map((s, idx) => (
                <button
                  key={`sugg-${s.cate_id}-${s.id}-${idx}`}
                  type="button"
                  title={t("stickerSuggestSendTitle")}
                  onMouseDown={(e) => {
                    if (e.button === 0) e.preventDefault();
                  }}
                  onClick={() => handleSendSuggestedSticker(s)}
                  className="hover-surface flex h-14 w-14 shrink-0 items-center justify-center rounded-xl p-1 transition hover:scale-110"
                >
                  <RelayImage
                    src={s.url}
                    alt={`Sticker ${s.id}`}
                    loading="lazy"
                    decoding="async"
                    referrerPolicy="no-referrer"
                    className="h-12 w-12 object-contain"
                  />
                </button>
              ))}
            </div>
            <button
              type="button"
              onMouseDown={(e) => {
                if (e.button === 0) e.preventDefault();
              }}
              onClick={() => setSuggestDismissedFor(activeSuggestQuery)}
              title={t("stickerSuggestDismiss")}
              aria-label={t("stickerSuggestDismiss")}
              className="hover-menu shrink-0 rounded-lg p-1.5 transition"
              style={{ color: "var(--text-secondary)" }}
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        )}

        {mentionOpen && (
          <div
            ref={mentionListboxRef}
            data-teza-floating-popup="true"
            id={mentionListboxId}
            role="listbox"
            aria-label={t("mentionMembersHeader")}
            className="absolute bottom-full left-4 z-30 mb-2 max-h-60 w-72 max-w-[calc(100%-2rem)] overflow-y-auto rounded-2xl p-1.5 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div
              className="px-2 py-1 text-[11px] font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              {t("mentionMembersHeader")}
            </div>
            {mentionMatches.map((m, idx) => {
              const isAll = isMentionAllMember(m);
              const showRole = Boolean(m.role && m.role !== "Thành viên");
              return (
                <button
                  key={m.id || `${m.name}-${idx}`}
                  id={`${mentionListboxId}-opt-${idx}`}
                  ref={(el) => {
                    mentionItemRefs.current[idx] = el;
                  }}
                  type="button"
                  role="option"
                  aria-selected={idx === safeMentionIndex}
                  onMouseEnter={() => setMentionIndex(idx)}
                  onMouseDown={(e) => {
                    if (e.button !== 0) return;
                    e.preventDefault();
                    if (isComposingRef.current) {
                      isComposingRef.current = false;
                      textareaRef.current?.blur();
                    }
                    insertMention(m);
                  }}
                  className="flex w-full items-center gap-2.5 rounded-xl px-2.5 py-1.5 text-left transition"
                  style={{
                    background: idx === safeMentionIndex ? "var(--bg-hover)" : "transparent",
                  }}
                >
                  {isAll ? (
                    <div
                      className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full"
                      style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                    >
                      <Users className="h-3.5 w-3.5" />
                    </div>
                  ) : (
                    <ThreadAvatar avatar={m.avatar} title={m.name} kind="direct" size="xs" />
                  )}
                  <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium">
                    {isAll ? `@${m.name}` : m.name}
                  </span>
                  {showRole && (
                    <span
                      className="shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold"
                      style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                    >
                      {m.role}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        )}

        {emojiOpen && (
          <div
            ref={emojiPickerRef}
            data-teza-floating-popup="true"
            className="absolute bottom-full left-4 z-30 mb-2 w-[360px] max-w-[calc(100vw-2rem)] rounded-2xl p-3 shadow-2xl"
            style={{ background: "var(--bg-sidebar)", border: "1px solid var(--border-subtle)" }}
          >
            <div
              className="mb-2 flex items-center justify-between gap-2 pb-2"
              style={{ borderBottom: "1px solid var(--border-subtle)" }}
            >
              <div className="flex gap-1">
                <button
                  type="button"
                  onClick={() => setPickerMode("emoji")}
                  className="rounded-lg px-2.5 py-1 text-xs font-semibold transition"
                  style={{
                    background: pickerMode === "emoji" ? "var(--accent-soft)" : "transparent",
                    color: pickerMode === "emoji" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  Emoji
                </button>
                {onSendSticker && (
                  <button
                    type="button"
                    onClick={() => setPickerMode("sticker")}
                    className="rounded-lg px-2.5 py-1 text-xs font-semibold transition"
                    style={{
                      background: pickerMode === "sticker" ? "var(--accent-soft)" : "transparent",
                      color: pickerMode === "sticker" ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    {t("stickerTabLabel")}
                  </button>
                )}
              </div>
              <button
                type="button"
                onClick={() => setEmojiOpen(false)}
                className="hover-menu rounded p-1"
                style={{ color: "var(--text-secondary)" }}
                aria-label={t("closeEmojiPanelLabel")}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>

            {pickerMode === "emoji" ? (
              <>
                <div
                  className="mb-2 flex items-center gap-1.5 rounded-xl px-2.5 py-1.5"
                  style={{ background: "var(--bg-input)", border: "1px solid var(--border-subtle)" }}
                >
                  <Search className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--text-muted)" }} />
                  <input
                    type="text"
                    value={emojiQuery}
                    onChange={(e) => setEmojiQuery(e.target.value)}
                    placeholder={t("searchEmojiPlaceholder")}
                    className="min-w-0 flex-1 bg-transparent text-xs outline-none"
                    style={{ color: "var(--text-primary)" }}
                  />
                  {emojiQuery && (
                    <button
                      type="button"
                      onClick={() => setEmojiQuery("")}
                      className="rounded p-0.5 transition hover:opacity-80"
                      style={{ color: "var(--text-secondary)" }}
                      aria-label={t("clearEmojiSearchLabel")}
                    >
                      <X className="h-3 w-3" />
                    </button>
                  )}
                </div>

                <div className="mb-1.5 flex items-center gap-1 overflow-x-auto pb-1">
                  <button
                    type="button"
                    title={t("emojiCatRetro")}
                    onClick={() => {
                      setEmojiQuery("");
                      setEmojiTab("retro");
                    }}
                    className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg transition"
                    style={{
                      background: !emojiQuery.trim() && emojiTab === "retro" ? "var(--accent-soft)" : "transparent",
                      color: !emojiQuery.trim() && emojiTab === "retro" ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    <RetroEmoticon id="smile" size={18} animated={false} />
                  </button>
                  <button
                    type="button"
                    title={t("recentUsedLabel")}
                    onClick={() => {
                      setEmojiQuery("");
                      setEmojiTab("recent");
                    }}
                    className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg text-sm transition"
                    style={{
                      background: !emojiQuery.trim() && emojiTab === "recent" ? "var(--accent-soft)" : "transparent",
                      color: !emojiQuery.trim() && emojiTab === "recent" ? "var(--accent)" : "var(--text-secondary)",
                    }}
                  >
                    🕒
                  </button>
                  {EMOJI_CATEGORIES.map((cat) => {
                    const active = !emojiQuery.trim() && emojiTab === cat.id;
                    const catLabel = EMOJI_CAT_LABEL_KEYS[cat.id] ? t(EMOJI_CAT_LABEL_KEYS[cat.id]) : cat.label;
                    return (
                      <button
                        key={cat.id}
                        type="button"
                        title={catLabel}
                        onClick={() => {
                          setEmojiQuery("");
                          setEmojiTab(cat.id);
                        }}
                        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-lg text-sm transition"
                        style={{
                          background: active ? "var(--accent-soft)" : "transparent",
                          color: active ? "var(--accent)" : "var(--text-secondary)",
                        }}
                      >
                        <EmojiGlyph emoji={cat.icon} size={17} animated={false} />
                      </button>
                    );
                  })}
                </div>

                {(() => {
                  const searching = Boolean(emojiQuery.trim());
                  const showRetroInSearch = searching && emojiStyle === "retro";
                  const retroGlyphSet = showRetroInSearch
                    ? new Set(visibleRetroEmoticons.map((r) => r.emoji))
                    : null;
                  const filteredUnicode = retroGlyphSet
                    ? visibleEmojis.filter((e) => !retroGlyphSet.has(e))
                    : visibleEmojis;
                  const showingRetroOnly = !searching && emojiTab === "retro";
                  const totalCount = showingRetroOnly
                    ? visibleRetroEmoticons.length
                    : (showRetroInSearch ? visibleRetroEmoticons.length : 0) + filteredUnicode.length;

                  return (
                    <>
                      <div
                        className="mb-1.5 flex items-center justify-between text-[11px] font-medium"
                        style={{ color: "var(--text-muted)" }}
                      >
                        <span>
                          {searching
                            ? t("searchResultsCount", { count: totalCount })
                            : emojiTab === "retro"
                              ? t("emojiCatRetro")
                              : emojiTab === "recent"
                                ? t("recentUsedLabel")
                                : activeEmojiCategory
                                  ? EMOJI_CAT_LABEL_KEYS[activeEmojiCategory.id]
                                    ? t(EMOJI_CAT_LABEL_KEYS[activeEmojiCategory.id])
                                    : activeEmojiCategory.label
                                  : ""}
                        </span>
                      </div>

                      {totalCount === 0 ? (
                        <div
                          className="flex h-44 items-center justify-center text-center text-xs"
                          style={{ color: "var(--text-secondary)" }}
                        >
                          {searching ? t("noEmojiFound") : t("noRecentEmoji")}
                        </div>
                      ) : (
                        <>
                          <div className="grid max-h-56 grid-cols-8 gap-1 overflow-y-auto pr-1">
                            {(showingRetroOnly || showRetroInSearch) &&
                              visibleRetroEmoticons.map((item) => (
                                <button
                                  key={`retro-${item.id}`}
                                  type="button"
                                  title={`${item.label} (${item.primaryShortcut})`}
                                  onClick={() => insertEmojiAtCaret(item.emoji)}
                                  className="hover-menu flex h-8 w-8 items-center justify-center rounded-lg transition hover:scale-110"
                                >
                                  <RetroEmoticon id={item.id} size={23} />
                                </button>
                              ))}
                            {!showingRetroOnly &&
                              filteredUnicode.map((emoji) => (
                                <button
                                  key={emoji}
                                  type="button"
                                  onClick={() => insertEmojiAtCaret(emoji)}
                                  className="hover-menu flex h-8 w-8 items-center justify-center rounded-lg text-base transition hover:scale-110"
                                >
                                  <EmojiGlyph emoji={emoji} size={21} animated={false} />
                                </button>
                              ))}
                          </div>
                          {(showingRetroOnly || showRetroInSearch) && (
                            <div
                              className="mt-1.5 truncate border-t pt-1 text-[10.5px]"
                              style={{
                                borderColor: "var(--border-subtle)",
                                color: "var(--text-muted)",
                              }}
                            >
                              {t("retroShortcutHint")}
                            </div>
                          )}
                        </>
                      )}
                    </>
                  );
                })()}
              </>
            ) : (
              <>
                <div
                  className="mb-2 flex items-center gap-1.5 rounded-xl px-2.5 py-1.5"
                  style={{ background: "var(--bg-input)", border: "1px solid var(--border-subtle)" }}
                >
                  <Search className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--text-muted)" }} />
                  <input
                    type="text"
                    value={stickerQuery}
                    onChange={(e) => setStickerQuery(e.target.value)}
                    placeholder={t("searchStickerPlaceholder")}
                    className="min-w-0 flex-1 bg-transparent text-xs outline-none"
                    style={{ color: "var(--text-primary)" }}
                  />
                  {stickerQuery && (
                    <button
                      type="button"
                      onClick={() => setStickerQuery("")}
                      className="rounded p-0.5 transition hover:opacity-80"
                      style={{ color: "var(--text-secondary)" }}
                      aria-label={t("clearStickerSearchLabel")}
                    >
                      <X className="h-3 w-3" />
                    </button>
                  )}
                </div>

                <div className="mb-2 flex flex-wrap gap-1">
                  {STICKER_QUICK_TAGS.map((tag) => {
                    const active = stickerQuery.trim().toLowerCase() === tag.toLowerCase();
                    return (
                      <button
                        key={tag}
                        type="button"
                        onClick={() => setStickerQuery(active ? "" : tag)}
                        className="rounded-full px-2 py-0.5 text-[11px] font-medium transition"
                        style={{
                          background: active ? "var(--accent-soft)" : "var(--bg-hover)",
                          color: active ? "var(--accent)" : "var(--text-secondary)",
                        }}
                      >
                        {tag}
                      </button>
                    );
                  })}
                </div>

                {!stickerQuery.trim() && (
                  <div className="mb-2 flex gap-1 overflow-x-auto pb-0.5">
                    {STICKER_CATEGORY_TABS.map((cat) => {
                      const active = stickerCategory === cat;
                      return (
                        <button
                          key={cat}
                          type="button"
                          onClick={() => setStickerCategory(cat)}
                          className="shrink-0 rounded-lg px-2 py-1 text-[11px] font-medium transition"
                          style={{
                            background: active ? "var(--accent-soft)" : "transparent",
                            color: active ? "var(--accent)" : "var(--text-secondary)",
                          }}
                        >
                          {t(STICKER_CAT_LABEL_KEYS[cat])}
                        </button>
                      );
                    })}
                  </div>
                )}

                {(stickerQuery.trim() ? stickersSearching : stickersLoading) ? (
                  <div
                    className="flex h-48 items-center justify-center gap-2 text-xs"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Loader2 className="h-4 w-4 animate-spin" />
                    <span>{stickerQuery.trim() ? t("searchingStickers") : t("loadingStickers")}</span>
                  </div>
                ) : visibleStickers.length === 0 ? (
                  <div
                    className="flex h-48 items-center justify-center text-center text-xs"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {stickerQuery.trim() ? t("noStickerFound") : t("noStickersAvailable")}
                  </div>
                ) : (
                  <div className="grid max-h-60 grid-cols-4 gap-2 overflow-y-auto pr-1">
                    {visibleStickers.map((s, idx) => (
                      <button
                        key={`${s.cate_id}-${s.id}-${idx}`}
                        type="button"
                        onClick={() => {
                          setStickers((prev) =>
                            prev
                              ? [
                                  { ...s, category: "Gần đây" },
                                  ...prev.filter((item) => !(item.id === s.id && item.cate_id === s.cate_id)),
                                ]
                              : prev,
                          );
                          setEmojiOpen(false);
                          void onSendSticker?.(s);
                        }}
                        className="hover-surface flex items-center justify-center rounded-xl p-1.5 transition hover:scale-105"
                      >
                        <RelayImage
                          src={s.url}
                          alt={`Sticker ${s.id}`}
                          loading="lazy"
                          decoding="async"
                          referrerPolicy="no-referrer"
                          className="h-14 w-14 object-contain"
                        />
                      </button>
                    ))}
                  </div>
                )}
              </>
            )}
          </div>
        )}

        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            if (files.length > 0) {
              attachRawFiles(files);
            }
          }}
        />
        {onSendAttachment && onAddPendingAttachments && (
          <button
            type="button"
            onClick={() => void handleAttachClick()}
            title={t("composerAttachTitle")}
            className="hover-surface rounded-lg p-2 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <Paperclip className="h-5 w-5" />
          </button>
        )}

        {onCaptureScreen && (
          <button
            type="button"
            disabled={capturingScreen || sending}
            onClick={() => {
              setEmojiOpen(false);
              onCaptureScreen();
            }}
            title={t("composerScreenshotTitle")}
            className="hover-surface rounded-lg p-2 transition disabled:opacity-50"
            style={{ color: "var(--text-secondary)" }}
          >
            {capturingScreen ? (
              <Loader2 className="h-5 w-5 animate-spin" />
            ) : (
              <Scissors className="h-5 w-5" />
            )}
          </button>
        )}

        <button
          ref={emojiButtonRef}
          type="button"
          onClick={() =>
            setEmojiOpen((v) => {
              const next = !v;
              if (next && mentionMatch) {
                setMentionDismissedAt(mentionMatch.start);
              }
              return next;
            })
          }
          title={t("composerEmojiTitle")}
          className="hover-surface rounded-lg p-2 transition"
          style={{ color: emojiOpen ? "var(--accent)" : "var(--text-secondary)" }}
        >
          <Smile className="h-5 w-5" />
        </button>

        {onBuzz && (
          <button
            type="button"
            disabled={sending || buzzCooldownSec > 0}
            onClick={() => {
              setEmojiOpen(false);
              void onBuzz();
            }}
            title={
              buzzCooldownSec > 0
                ? t("composerBuzzCooldown", { secs: buzzCooldownSec })
                : t("composerBuzzTitle")
            }
            className="hover-surface relative rounded-lg p-2 transition disabled:cursor-not-allowed disabled:opacity-45"
            style={{ color: buzzCooldownSec > 0 ? "var(--text-secondary)" : "#f59e0b" }}
          >
            <Zap className="h-5 w-5" />
            {buzzCooldownSec > 0 && (
              <span className="absolute -right-1 -top-1 rounded-full bg-amber-500/90 px-1 text-[10px] font-bold text-slate-950">
                {buzzCooldownSec}
              </span>
            )}
          </button>
        )}

        {onOpenReminder && (
          <button
            type="button"
            onClick={() => {
              setEmojiOpen(false);
              onOpenReminder();
            }}
            title={t("composerReminderTitle")}
            className="hover-surface rounded-lg p-2 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <CalendarClock className="h-5 w-5" />
          </button>
        )}

        {onOpenPoll && (
          <button
            type="button"
            onClick={() => {
              setEmojiOpen(false);
              onOpenPoll();
            }}
            title={t("composerPollTitle")}
            className="hover-surface rounded-lg p-2 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <BarChart3 className="h-5 w-5" />
          </button>
        )}

        <textarea
          ref={textareaRef}
          rows={1}
          value={draft}
          role="combobox"
          aria-multiline="true"
          aria-autocomplete="list"
          aria-haspopup="listbox"
          aria-expanded={mentionOpen}
          aria-controls={mentionOpen ? mentionListboxId : undefined}
          aria-activedescendant={activeMentionOptionId}
          onChange={(e) => {
            const val = e.target.value;
            setDraft(val);
            syncCursorFromEl(e.currentTarget);
          }}
          onFocus={(e) => {
            syncCursorFromEl(e.currentTarget);
          }}
          onClick={(e) => {
            syncCursorFromEl(e.currentTarget);
          }}
          onKeyUp={(e) => {
            syncCursorFromEl(e.currentTarget);
          }}
          onSelect={(e) => {
            syncCursorFromEl(e.currentTarget);
          }}
          onPaste={(e) => {
            const pastedFile = extractAttachmentFromClipboard(e.clipboardData);
            if (pastedFile) {
              e.preventDefault();
              attachRawFiles([pastedFile]);
            }
          }}
          onCompositionStart={() => {
            isComposingRef.current = true;
          }}
          onCompositionUpdate={(e) => {
            isComposingRef.current = true;
            syncCursorFromEl(e.currentTarget);
          }}
          onCompositionEnd={(e) => {
            isComposingRef.current = false;
            const val = e.currentTarget.value;
            if (val !== draftRef.current) {
              setDraft(val);
            }
            syncCursorFromEl(e.currentTarget);
          }}
          onKeyDown={(e) => {
            const isComposing =
              isComposingRef.current || e.nativeEvent.isComposing || e.keyCode === 229;
            if (isComposing) {
              return;
            }
            if (mentionMatches.length > 0 && mentionMatch) {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setMentionIndex(
                  ((safeMentionIndex >= 0 ? safeMentionIndex : 0) + 1) % mentionMatches.length,
                );
                return;
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setMentionIndex(
                  ((safeMentionIndex >= 0 ? safeMentionIndex : 0) - 1 + mentionMatches.length) %
                    mentionMatches.length,
                );
                return;
              }
              if (
                e.key === "Tab" ||
                (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey)
              ) {
                e.preventDefault();
                insertMention(mentionMatches[safeMentionIndex] ?? mentionMatches[0]);
                return;
              }
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setMentionDismissedAt(mentionMatch.start);
                return;
              }
            }
            if (visibleSuggestedStickers.length > 0 && e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              setSuggestDismissedFor(activeSuggestQuery);
              return;
            }
            const shouldSubmit = sendOnEnter
              ? e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey
              : e.key === "Enter" && (e.ctrlKey || e.metaKey);
            if (shouldSubmit) {
              e.preventDefault();
              setEmojiOpen(false);
              void handleSubmit();
            }
          }}
          placeholder={
            hasPendingAttachments
              ? t("composerCaptionPlaceholder")
              : placeholder || t("composerPlaceholder")
          }
          className="min-h-[40px] max-h-[140px] flex-1 resize-none overflow-y-auto rounded-xl px-3.5 py-2 text-[15px] leading-6 outline-none"
          style={{ background: "var(--bg-surface)", color: "var(--text-primary)" }}
        />

        {onOpenSchedule && (
          <button
            type="button"
            onClick={() => {
              setEmojiOpen(false);
              onOpenSchedule();
            }}
            title={t("composerScheduleTitle")}
            className="hover-surface rounded-xl p-2.5 transition"
            style={{ color: "var(--text-secondary)" }}
          >
            <Clock className="h-4 w-4" />
          </button>
        )}

        <button
          type="button"
          onClick={() => {
            setEmojiOpen(false);
            void handleSubmit();
          }}
          disabled={sending || (!draft.trim() && !hasPendingAttachments)}
          title={t("composerSendTitle")}
          className="tf-btn rounded-xl p-2.5 disabled:cursor-not-allowed disabled:opacity-40"
        >
          <Send className="h-4 w-4" />
        </button>
      </div>
    </footer>
  );
}
