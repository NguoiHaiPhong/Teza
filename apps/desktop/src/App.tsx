import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { check, type Update } from "@tauri-apps/plugin-updater";
import {
  ArrowDownToLine,
  Bell,
  BellOff,
  Bookmark,
  ChevronDown,
  FileText,
  Image as ImageIcon,
  Info,
  Languages,
  LogOut,
  MessageSquare,
  MessagesSquare,
  Moon,
  Pin,
  PinOff,
  RefreshCw,
  Search,
  Settings,
  Sparkles,
  Sun,
  UploadCloud,
  User,
  Users,
  Wifi,
  X,
  Zap,
} from "lucide-react";
import { bridge, isTauri } from "./bridge";
import {
  BUZZ_COOLDOWN_MS,
  BUZZ_TEXT,
  isBuzzMessage,
  playBuzzSound,
} from "./utils/buzz";
import { Composer } from "./components/Composer";
import { ForwardModal } from "./components/ForwardModal";
import { MediaLightbox } from "./components/MediaLightbox";
import { MessageBubble, type MessageTranslationState } from "./components/MessageBubble";
import { QrLoginModal } from "./components/QrLoginModal";
import { QuickSearchModal } from "./components/QuickSearchModal";
import { ReplyThreadPanel } from "./components/ReplyThreadPanel";
import { ThreadAvatar } from "./components/ThreadAvatar";
import {
  TRANSLATE_LANG_OPTIONS,
  TranslateSettingsModal,
} from "./components/TranslateSettingsModal";
import { TriforceEffects } from "./components/TriforceEffects";
import { UpdateModal } from "./components/UpdateModal";
import { UserProfileModal } from "./components/UserProfileModal";
import type {
  AccountProfile,
  ConnectionStatus,
  FolderFilter,
  MessageDto,
  PendingAttachment,
  ThemeMode,
  ThreadDetailsDto,
  ThreadDto,
  TranslateConfigSummary,
} from "./types";

const FOLDERS: { id: FolderFilter; label: string; icon: typeof MessageSquare }[] = [
  { id: "all", label: "Tất cả", icon: MessagesSquare },
  { id: "unread", label: "Chưa đọc", icon: MessageSquare },
  { id: "direct", label: "Cá nhân", icon: User },
  { id: "group", label: "Nhóm", icon: Users },
  { id: "cloud", label: "Cloud", icon: Bookmark },
];

function dayKey(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

function formatDayLabel(ts: number): string {
  const d = new Date(ts > 1e12 ? ts : ts * 1000);
  const today = new Date();
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(d.getTime()) === dayKey(today.getTime())) return "Hôm nay";
  if (dayKey(d.getTime()) === dayKey(yest.getTime())) return "Hôm qua";
  return d.toLocaleDateString("vi-VN");
}

function formatThreadTime(ts: number): string {
  if (!ts) return "";
  const ms = ts > 1e12 ? ts : ts * 1000;
  const d = new Date(ms);
  const now = new Date();
  if (dayKey(ms) === dayKey(now.getTime())) {
    return d.toLocaleTimeString("vi-VN", { hour: "2-digit", minute: "2-digit" });
  }
  const yest = new Date(Date.now() - 86_400_000);
  if (dayKey(ms) === dayKey(yest.getTime())) {
    return "Hôm qua";
  }
  return d.toLocaleDateString("vi-VN", { day: "2-digit", month: "2-digit" });
}

function upsertMsg(list: MessageDto[], incoming: MessageDto): MessageDto[] {
  const idx = list.findIndex((m) => m.msg_id === incoming.msg_id);
  if (idx >= 0) {
    const next = [...list];
    next[idx] = incoming;
    return next;
  }
  return [...list, incoming].sort((a, b) => a.ts - b.ts);
}

function compareThreads(a: ThreadDto, b: ThreadDto): number {
  const aUnread = a.unread_count > 0 ? 1 : 0;
  const bUnread = b.unread_count > 0 ? 1 : 0;
  if (bUnread !== aUnread) return bUnread - aUnread;
  if (b.last_ts !== a.last_ts) return b.last_ts - a.last_ts;
  return (b.is_pinned ? 1 : 0) - (a.is_pinned ? 1 : 0);
}

function bumpThreadList(list: ThreadDto[], msg: MessageDto, incUnread: boolean): ThreadDto[] {
  const rawPreview = (
    msg.text || (msg.attachment_name ? `[Tệp: ${msg.attachment_name}]` : `[${msg.msg_type}]`)
  )
    .replace(/\s+/g, " ")
    .trim();
  const idx = list.findIndex((t) => t.id === msg.thread_id);
  if (idx < 0) return list;
  const target = list[idx];
  let preview = rawPreview;
  if (rawPreview) {
    if (msg.is_outgoing) {
      preview = rawPreview.startsWith("Bạn:") ? rawPreview : `Bạn: ${rawPreview}`;
    } else if (
      target.kind === "group" &&
      msg.sender_name &&
      msg.sender_name !== "Bạn" &&
      !/^\d{6,}$/.test(msg.sender_name.trim())
    ) {
      preview = `${msg.sender_name}: ${rawPreview}`;
    }
  }
  const next = [...list];
  next[idx] = {
    ...target,
    last_preview: msg.ts >= target.last_ts ? preview : target.last_preview,
    last_ts: Math.max(target.last_ts, msg.ts),
    unread_count: msg.is_outgoing
      ? 0
      : incUnread
        ? target.unread_count + 1
        : target.unread_count,
  };
  return next.sort(compareThreads);
}

export default function App() {
  const [theme, setTheme] = useState<ThemeMode>(() => {
    const saved = localStorage.getItem("litechat.theme");
    return saved === "night-blue" || saved === "dark" || saved === "light" || saved === "triforce"
      ? saved
      : "triforce";
  });
  const [fxEnabled, setFxEnabled] = useState<boolean>(
    () => localStorage.getItem("litechat.fx") !== "0",
  );
  const [authChecking, setAuthChecking] = useState(true);
  const [authed, setAuthed] = useState(false);
  const [qrSvg, setQrSvg] = useState("");
  const [qrStatus, setQrStatus] = useState("Đang khởi tạo mã QR...");
  const [qrError, setQrError] = useState<string | null>(null);
  const [connStatus, setConnStatus] = useState<ConnectionStatus>("connecting");
  const [syncing, setSyncing] = useState(false);
  const [toastError, setToastError] = useState<string | null>(null);

  const [folder, setFolder] = useState<FolderFilter>("all");
  const [threads, setThreads] = useState<ThreadDto[]>([]);
  const [filterText, setFilterText] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [messages, setMessages] = useState<MessageDto[]>([]);
  const [hasMoreHistory, setHasMoreHistory] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [replyTo, setReplyTo] = useState<MessageDto | null>(null);
  const [activeThreadRootId, setActiveThreadRootId] = useState<string | null>(null);
  const [localThreadLinks, setLocalThreadLinks] = useState<Record<string, string>>(() => {
    try {
      const raw = localStorage.getItem("litechat.threadLinks");
      return raw ? (JSON.parse(raw) as Record<string, string>) : {};
    } catch {
      return {};
    }
  });
  const [pendingAttachment, setPendingAttachment] = useState<PendingAttachment | null>(null);
  const [highlightedId, setHighlightedId] = useState<string | null>(null);
  const [lightbox, setLightbox] = useState<{ url: string; title: string } | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(true);
  const [quickSearchOpen, setQuickSearchOpen] = useState(false);
  const [threadDetails, setThreadDetails] = useState<ThreadDetailsDto | null>(null);
  const [detailsLoading, setDetailsLoading] = useState(false);
  const [detailsTab, setDetailsTab] = useState<"media" | "files">("media");
  const [threadMenu, setThreadMenu] = useState<{ thread: ThreadDto; x: number; y: number } | null>(
    null,
  );
  const [myProfile, setMyProfile] = useState<AccountProfile | null>(null);
  const [profileUid, setProfileUid] = useState<string | null>(null);
  const [forwardMsg, setForwardMsg] = useState<MessageDto | null>(null);
  const [translateCfg, setTranslateCfg] = useState<TranslateConfigSummary | null>(null);
  const [translateModalOpen, setTranslateModalOpen] = useState(false);
  const [translationsByMsg, setTranslationsByMsg] = useState<
    Record<string, MessageTranslationState>
  >({});
  const [threadAutoTranslate, setThreadAutoTranslate] = useState<Record<string, string>>({});
  const [threadTranslating, setThreadTranslating] = useState(false);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [showScrollDown, setShowScrollDown] = useState(false);
  const [unreadBelow, setUnreadBelow] = useState(0);
  const [dragHover, setDragHover] = useState(false);
  const [buzzEnabled, setBuzzEnabled] = useState<boolean>(
    () => localStorage.getItem("litechat.buzz") !== "0",
  );
  const [buzzShaking, setBuzzShaking] = useState(false);
  const [buzzToast, setBuzzToast] = useState<{
    threadId: string;
    threadTitle: string;
    senderName: string;
  } | null>(null);
  const [buzzCooldownUntil, setBuzzCooldownUntil] = useState<Record<string, number>>({});
  const [buzzNow, setBuzzNow] = useState(() => Date.now());
  const [appVersion, setAppVersion] = useState("0.1.0");
  const [updateModalOpen, setUpdateModalOpen] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [lastUpdateCheckAt, setLastUpdateCheckAt] = useState<number | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState<number>(() => {
    const raw = Number(localStorage.getItem("litechat.sidebarWidth"));
    return Number.isFinite(raw) && raw >= 240 && raw <= 440 ? raw : 308;
  });

  const selectedIdRef = useRef<string | null>(null);
  const folderRef = useRef<FolderFilter>(folder);
  const threadsRef = useRef<ThreadDto[]>(threads);
  const buzzEnabledRef = useRef<boolean>(buzzEnabled);
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const pendingJumpRef = useRef<{ threadId: string; msgId: string; ts: number } | null>(null);
  const bootedRef = useRef(false);
  const translationsRef = useRef(translationsByMsg);
  const inFlightTranslateRef = useRef<Set<string>>(new Set());

  selectedIdRef.current = selectedId;
  folderRef.current = folder;
  threadsRef.current = threads;
  buzzEnabledRef.current = buzzEnabled;
  translationsRef.current = translationsByMsg;

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("litechat.theme", theme);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem("litechat.fx", fxEnabled ? "1" : "0");
  }, [fxEnabled]);

  useEffect(() => {
    localStorage.setItem("litechat.buzz", buzzEnabled ? "1" : "0");
  }, [buzzEnabled]);

  const checkForUpdates = useCallback(async (silent = false) => {
    if (!isTauri()) {
      if (!silent) {
        setUpdateError("Tính năng tự động cập nhật chỉ hoạt động trên bản cài đặt desktop.");
      }
      return;
    }
    setCheckingUpdate(true);
    if (!silent) setUpdateError(null);
    try {
      const update = await check();
      setAvailableUpdate(update);
      setLastUpdateCheckAt(Date.now());
    } catch (err) {
      if (!silent) setUpdateError(String(err));
    } finally {
      setCheckingUpdate(false);
    }
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    void getVersion()
      .then(setAppVersion)
      .catch(() => undefined);
    const timer = window.setTimeout(() => {
      void checkForUpdates(true);
    }, 4500);
    return () => window.clearTimeout(timer);
  }, [checkForUpdates]);

  const activeBuzzUntil = selectedId ? buzzCooldownUntil[selectedId] ?? 0 : 0;
  const buzzCooldownSec =
    activeBuzzUntil > buzzNow ? Math.ceil((activeBuzzUntil - buzzNow) / 1000) : 0;

  useEffect(() => {
    if (activeBuzzUntil <= Date.now()) return;
    const timer = window.setInterval(() => {
      const now = Date.now();
      setBuzzNow(now);
      if (now >= activeBuzzUntil) {
        window.clearInterval(timer);
      }
    }, 500);
    return () => window.clearInterval(timer);
  }, [activeBuzzUntil]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setQuickSearchOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const showError = useCallback((err: unknown) => {
    setToastError(String(err));
    setTimeout(() => setToastError(null), 5000);
  }, []);

  const refreshQr = useCallback(async () => {
    setQrError(null);
    setQrStatus("Đang lấy mã QR...");
    try {
      const res = await bridge.startQr();
      setQrSvg(res.qr_svg);
      setQrStatus("Quét mã trên điện thoại...");
    } catch (err) {
      setQrError(String(err));
      setQrStatus("Không thể kết nối máy chủ QR");
    }
  }, []);

  const resetSessionUi = useCallback(() => {
    setMyProfile(null);
    setProfileUid(null);
    setForwardMsg(null);
    setTranslationsByMsg({});
    setThreadAutoTranslate({});
    setThreads([]);
    setMessages([]);
    setSelectedId(null);
    setReplyTo(null);
    setActiveThreadRootId(null);
    setPendingAttachment(null);
    setThreadDetails(null);
    setThreadMenu(null);
    setShowScrollDown(false);
    setUnreadBelow(0);
  }, []);

  useEffect(() => {
    if (!authed) return;
    void bridge
      .getTranslateConfig()
      .then(setTranslateCfg)
      .catch(() => undefined);
  }, [authed]);

  const isTranslatableText = useCallback((m: MessageDto) => {
    if (m.is_recalled) return false;
    const t = m.text.trim();
    if (!t) return false;
    const exactSkip = [
      "[Nhãn dán]",
      "[Hình ảnh]",
      "[Video]",
      "[Tin nhắn thoại]",
      "[Tệp đính kèm]",
      "[Tin nhắn đã thu hồi]",
      "[Thiệp/Danh thiếp]",
      "[Vị trí]",
      "[Cuộc gọi]",
    ];
    return !exactSkip.includes(t);
  }, []);

  const translateBatchMessages = useCallback(
    async (targetMsgs: MessageDto[], targetLang: string, singleMode = false) => {
      const pending = targetMsgs.filter((m) => {
        if (!isTranslatableText(m)) return false;
        const cur = translationsRef.current[m.msg_id];
        if (!singleMode && cur && cur.targetLang === targetLang && (cur.text || cur.loading)) {
          return false;
        }
        return !inFlightTranslateRef.current.has(`${m.msg_id}:${targetLang}`);
      });
      if (pending.length === 0) return;

      for (const m of pending) {
        inFlightTranslateRef.current.add(`${m.msg_id}:${targetLang}`);
      }
      setTranslationsByMsg((prev) => {
        const next = { ...prev };
        for (const m of pending) {
          next[m.msg_id] = { targetLang, loading: true };
        }
        return next;
      });
      if (!singleMode) setThreadTranslating(true);

      try {
        const results = await bridge.translateMessages(
          pending.map((m) => ({ msg_id: m.msg_id, text: m.text })),
          targetLang,
        );
        const byId = new Map(results.map((r) => [r.msg_id, r]));
        setTranslationsByMsg((prev) => {
          const next = { ...prev };
          for (const m of pending) {
            const hit = byId.get(m.msg_id);
            if (hit) {
              next[m.msg_id] = {
                text: hit.translated_text,
                src: hit.detected_source_lang,
                targetLang,
                loading: false,
              };
            } else {
              delete next[m.msg_id];
            }
          }
          return next;
        });
      } catch (err) {
        const msg = String(err);
        setTranslationsByMsg((prev) => {
          const next = { ...prev };
          for (const m of pending) {
            if (singleMode) {
              next[m.msg_id] = { targetLang, loading: false, error: msg };
            } else {
              delete next[m.msg_id];
            }
          }
          return next;
        });
        showError(msg);
      } finally {
        for (const m of pending) {
          inFlightTranslateRef.current.delete(`${m.msg_id}:${targetLang}`);
        }
        if (!singleMode) setThreadTranslating(false);
      }
    },
    [isTranslatableText, showError],
  );

  const handleTranslateSingle = useCallback(
    (msg: MessageDto) => {
      const targetLang =
        (selectedIdRef.current && threadAutoTranslate[selectedIdRef.current]) ||
        translateCfg?.target_lang ||
        "VI";
      void translateBatchMessages([msg], targetLang, true);
    },
    [threadAutoTranslate, translateCfg, translateBatchMessages],
  );

  const handleToggleThreadTranslate = useCallback(() => {
    if (!selectedId) return;
    const current = threadAutoTranslate[selectedId];
    if (current) {
      setThreadAutoTranslate((prev) => {
        const next = { ...prev };
        delete next[selectedId];
        return next;
      });
      setTranslationsByMsg((prev) => {
        const next = { ...prev };
        for (const m of messages) {
          delete next[m.msg_id];
        }
        return next;
      });
    } else {
      const lang = translateCfg?.target_lang || "VI";
      setThreadAutoTranslate((prev) => ({ ...prev, [selectedId]: lang }));
    }
  }, [selectedId, threadAutoTranslate, translateCfg, messages]);

  const syncRemoteThreads = useCallback(async () => {
    setSyncing(true);
    try {
      const list = await bridge.syncThreads(folderRef.current);
      setThreads(list);
      if (!selectedIdRef.current && list.length > 0) {
        setSelectedId(list[0].id);
      }
    } catch (err) {
      showError(err);
    } finally {
      setSyncing(false);
    }
  }, [showError]);

  useEffect(() => {
    if (bootedRef.current) return;
    bootedRef.current = true;
    void bridge
      .authStatus()
      .then((res) => {
        if (res.status === "authenticated") {
          setMyProfile(res.profile);
          setAuthed(true);
          void syncRemoteThreads();
        } else {
          if (res.status === "expired") {
            setQrError(res.reason);
          }
          void refreshQr();
        }
      })
      .catch((err) => {
        setQrError(String(err));
        void refreshQr();
      })
      .finally(() => {
        setAuthChecking(false);
      });
  }, [refreshQr, syncRemoteThreads]);

  useEffect(() => {
    if (!isTauri()) return;
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    const setupListeners = async () => {
      const fns = await Promise.all([
        listen<string | null>("core://qr-scanned", (ev) => {
          setQrStatus(
            ev.payload
              ? `Đã quét (${ev.payload}) — vui lòng bấm Xác nhận trên điện thoại`
              : "Đã quét mã — vui lòng Xác nhận trên điện thoại",
          );
        }),
        listen<string>("core://auth-stage", (ev) => {
          setQrStatus(ev.payload);
        }),
        listen<AccountProfile>("core://auth-authenticated", (ev) => {
          setMyProfile(ev.payload);
          setAuthed(true);
          setQrError(null);
          void syncRemoteThreads();
        }),
        listen<string>("core://auth-error", (ev) => {
          setQrError(ev.payload);
          setQrStatus("Đăng nhập thất bại");
        }),
        listen<string>("core://auth-expired", (ev) => {
          setAuthed(false);
          resetSessionUi();
          setConnStatus("reconnecting");
          setQrError(ev.payload);
          void refreshQr();
        }),
        listen<ConnectionStatus>("core://connection-status", (ev) => {
          setConnStatus(ev.payload);
        }),
        listen<MessageDto>("core://message-new", (ev) => {
          const msg = ev.payload;
          setThreads((prev) =>
            bumpThreadList(prev, msg, msg.thread_id !== selectedIdRef.current && !msg.is_outgoing),
          );
          if (msg.thread_id === selectedIdRef.current) {
            setMessages((prev) => upsertMsg(prev, msg));
            const el = scrollContainerRef.current;
            const nearBottom = !el || el.scrollHeight - el.scrollTop - el.clientHeight < 180;
            if (nearBottom || msg.is_outgoing) {
              setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
            } else {
              setUnreadBelow((n) => n + 1);
            }
          }
          if (!msg.is_outgoing && isBuzzMessage(msg) && buzzEnabledRef.current) {
            const matchedThread = threadsRef.current.find((t) => t.id === msg.thread_id);
            const isRecent = Math.abs(Date.now() - msg.ts) < 90_000;
            if (!matchedThread?.is_muted && isRecent) {
              playBuzzSound();
              setBuzzShaking(true);
              setTimeout(() => setBuzzShaking(false), 1200);
              setBuzzToast({
                threadId: msg.thread_id,
                threadTitle: matchedThread?.title || msg.sender_name || msg.thread_id,
                senderName: msg.sender_name || matchedThread?.title || "Liên hệ",
              });
              setTimeout(() => setBuzzToast(null), 5000);
              if (isTauri()) {
                void getCurrentWindow()
                  .requestUserAttention(2)
                  .catch(() => undefined);
              }
            }
          }
        }),
        listen<MessageDto>("core://message-updated", (ev) => {
          const msg = ev.payload;
          if (msg.thread_id === selectedIdRef.current) {
            setMessages((prev) => upsertMsg(prev, msg));
          }
        }),
        listen<{ thread_id: string; msg_id: string }>("core://message-deleted", (ev) => {
          if (ev.payload.thread_id === selectedIdRef.current) {
            setMessages((prev) => prev.filter((m) => m.msg_id !== ev.payload.msg_id));
          }
        }),
        listen("core://threads-updated", () => {
          void bridge.listThreads(folderRef.current).then(setThreads);
        }),
      ]);

      if (cancelled) {
        fns.forEach((fn) => fn());
      } else {
        unlisteners.push(...fns);
      }
    };

    void setupListeners();
    return () => {
      cancelled = true;
      unlisteners.forEach((fn) => fn());
    };
  }, [syncRemoteThreads, refreshQr]);

  useEffect(() => {
    if (!authed) return;
    void bridge.listThreads(folder).then((list) => {
      setThreads(list);
      if (!selectedIdRef.current && list.length > 0) {
        setSelectedId(list[0].id);
      }
    });
  }, [authed, folder]);

  useEffect(() => {
    if (!authed || !drawerOpen || !selectedId) {
      return;
    }
    let cancelled = false;
    setDetailsLoading(true);
    void bridge
      .getThreadDetails(selectedId)
      .then((res) => {
        if (!cancelled) setThreadDetails(res);
      })
      .catch(() => {
        if (!cancelled) setThreadDetails(null);
      })
      .finally(() => {
        if (!cancelled) setDetailsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [authed, drawerOpen, selectedId]);

  useEffect(() => {
    if (!threadMenu) return;
    const close = () => setThreadMenu(null);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [threadMenu]);

  const jumpToMsg = useCallback(
    async (msgId: string) => {
      const highlightEl = (targetId: string) => {
        const el = document.getElementById(`msg-${targetId}`);
        if (el) {
          el.scrollIntoView({ behavior: "smooth", block: "center" });
          setHighlightedId(targetId);
          setTimeout(() => setHighlightedId(null), 1800);
          return true;
        }
        return false;
      };
      if (highlightEl(msgId)) return;
      const tid = selectedIdRef.current;
      if (!tid) return;
      try {
        const found = await bridge.getMessageById(tid, msgId);
        if (found && selectedIdRef.current === tid) {
          const around = await bridge.loadMessagesAround(tid, found.ts);
          if (selectedIdRef.current === tid) {
            setMessages(around.messages);
            setHasMoreHistory(around.has_more);
            setTimeout(() => highlightEl(msgId), 60);
          }
        } else {
          showError("Không tìm thấy tin nhắn gốc trong lịch sử cục bộ");
        }
      } catch (err) {
        showError(err);
      }
    },
    [showError],
  );

  useEffect(() => {
    if (!authed || !isTauri()) return;
    let active = true;
    let unlisten: UnlistenFn | undefined;
    void getCurrentWebview()
      .onDragDropEvent(async (event) => {
        if (!active) return;
        const payload = event.payload;
        if (payload.type === "enter" || payload.type === "over") {
          setDragHover(true);
          return;
        }
        if (payload.type === "leave") {
          setDragHover(false);
          return;
        }
        if (payload.type !== "drop") return;
        setDragHover(false);
        const paths = payload.paths ?? [];
        if (paths.length === 0) return;
        try {
          if (paths.length === 1) {
            const meta = await bridge.inspectAttachmentPath(paths[0]);
            if (!active) return;
            setPendingAttachment({
              kind: "path",
              path: meta.path,
              fileName: meta.file_name,
              sizeBytes: meta.size_bytes,
              isImage: meta.is_image,
            });
          } else {
            const tid = selectedIdRef.current;
            if (!tid) return;
            for (const p of paths) {
              const meta = await bridge.inspectAttachmentPath(p);
              if (!active) return;
              const sent = await bridge.sendAttachmentPath(tid, meta.path, "");
              setMessages((prev) => upsertMsg(prev, sent));
              setThreads((prev) => bumpThreadList(prev, sent, false));
            }
            setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
          }
        } catch (err) {
          showError(err);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => {
      active = false;
      setDragHover(false);
      unlisten?.();
    };
  }, [authed, showError]);

  useEffect(() => {
    if (!authed || !selectedId) return;
    let cancelled = false;
    setReplyTo(null);
    setActiveThreadRootId(null);
    setPendingAttachment(null);
    setHasMoreHistory(false);
    setLoadingOlder(false);
    setLoadingMessages(true);
    setShowScrollDown(false);
    setUnreadBelow(0);
    void bridge.markRead(selectedId);
    setThreads((prev) => prev.map((t) => (t.id === selectedId ? { ...t, unread_count: 0 } : t)));
    void bridge
      .loadMessages(selectedId, undefined, true)
      .then(async (page) => {
        if (cancelled) return;
        const pending = pendingJumpRef.current;
        if (pending && pending.threadId === selectedId) {
          pendingJumpRef.current = null;
          if (!page.messages.some((m) => m.msg_id === pending.msgId)) {
            const around = await bridge.loadMessagesAround(selectedId, pending.ts);
            if (cancelled) return;
            setMessages(around.messages);
            setHasMoreHistory(around.has_more);
          } else {
            setMessages(page.messages);
            setHasMoreHistory(page.has_more);
          }
          setTimeout(() => void jumpToMsg(pending.msgId), 60);
        } else {
          setMessages(page.messages);
          setHasMoreHistory(page.has_more);
          setTimeout(() => bottomRef.current?.scrollIntoView(), 40);
        }
      })
      .finally(() => {
        if (!cancelled) setLoadingMessages(false);
      });
    return () => {
      cancelled = true;
    };
  }, [authed, selectedId, jumpToMsg]);

  const activeThreadLang = selectedId ? threadAutoTranslate[selectedId] : undefined;
  useEffect(() => {
    if (!authed || !selectedId || !activeThreadLang || messages.length === 0) return;
    void translateBatchMessages(messages, activeThreadLang);
  }, [authed, selectedId, activeThreadLang, messages, translateBatchMessages]);

  const loadOlderMessages = useCallback(async () => {
    if (!selectedId || loadingOlder || !hasMoreHistory || messages.length === 0) return;
    const oldest = messages[0];
    const container = scrollContainerRef.current;
    const prevScrollHeight = container?.scrollHeight ?? 0;
    const prevScrollTop = container?.scrollTop ?? 0;

    setLoadingOlder(true);
    try {
      const page = await bridge.loadMessages(selectedId, oldest.ts, false, oldest.msg_id);
      if (selectedIdRef.current !== selectedId) return;
      if (page.messages.length > 0) {
        setMessages((prev) => {
          const seen = new Set(prev.map((m) => m.msg_id));
          const fresh = page.messages.filter((m) => !seen.has(m.msg_id));
          return fresh.length > 0 ? [...fresh, ...prev] : prev;
        });
        requestAnimationFrame(() => {
          if (container) {
            const delta = container.scrollHeight - prevScrollHeight;
            container.scrollTop = prevScrollTop + delta;
          }
        });
      }
      setHasMoreHistory(page.has_more);
    } catch (err) {
      showError(err);
    } finally {
      setLoadingOlder(false);
    }
  }, [selectedId, loadingOlder, hasMoreHistory, messages, showError]);

  const handleSelectSearchResult = useCallback(
    async (hit: MessageDto) => {
      if (hit.thread_id === selectedId) {
        if (messages.some((m) => m.msg_id === hit.msg_id)) {
          setTimeout(() => jumpToMsg(hit.msg_id), 40);
        } else {
          try {
            const around = await bridge.loadMessagesAround(hit.thread_id, hit.ts);
            setMessages(around.messages);
            setHasMoreHistory(around.has_more);
            setTimeout(() => jumpToMsg(hit.msg_id), 60);
          } catch (err) {
            showError(err);
          }
        }
      } else {
        pendingJumpRef.current = { threadId: hit.thread_id, msgId: hit.msg_id, ts: hit.ts };
        setSelectedId(hit.thread_id);
      }
    },
    [selectedId, messages, jumpToMsg, showError],
  );

  const visibleThreads = useMemo(() => {
    const q = filterText.trim().toLowerCase();
    const base = q ? threads.filter((t) => t.title.toLowerCase().includes(q)) : [...threads];
    return base.sort(compareThreads);
  }, [threads, filterText]);

  const activeThread = useMemo(
    () => threads.find((t) => t.id === selectedId) ?? null,
    [threads, selectedId],
  );

  const handleTogglePin = async (target: ThreadDto) => {
    const nextPinned = !target.is_pinned;
    try {
      await bridge.togglePin(target.id, nextPinned);
      setThreads((prev) =>
        prev.map((t) => (t.id === target.id ? { ...t, is_pinned: nextPinned } : t)).sort(compareThreads),
      );
    } catch (err) {
      showError(err);
    }
  };

  const handleToggleMute = async (target: ThreadDto) => {
    const nextMuted = !target.is_muted;
    try {
      await bridge.toggleMute(target.id, nextMuted);
      setThreads((prev) =>
        prev.map((t) => (t.id === target.id ? { ...t, is_muted: nextMuted } : t)),
      );
    } catch (err) {
      showError(err);
    }
  };

  const handleMarkUnread = async (target: ThreadDto) => {
    try {
      await bridge.markUnread(target.id);
      setThreads((prev) =>
        prev
          .map((t) =>
            t.id === target.id ? { ...t, unread_count: Math.max(1, t.unread_count) } : t,
          )
          .sort(compareThreads),
      );
    } catch (err) {
      showError(err);
    }
  };

  const handleSendText = async (text: string) => {
    if (!selectedId) return;
    try {
      const msg = replyTo
        ? await bridge.sendQuote(selectedId, text, replyTo)
        : await bridge.sendText(selectedId, text);
      setReplyTo(null);
      setMessages((prev) => upsertMsg(prev, msg));
      setThreads((prev) => bumpThreadList(prev, msg, false));
      setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
    } catch (err) {
      showError(err);
    }
  };

  const handleSendThreadReply = useCallback(
    async (text: string, quoteTarget: MessageDto) => {
      if (!selectedId) return;
      try {
        const msg = await bridge.sendQuote(selectedId, text, quoteTarget);
        setMessages((prev) => upsertMsg(prev, msg));
        setThreads((prev) => bumpThreadList(prev, msg, false));
      } catch (err) {
        showError(err);
      }
    },
    [selectedId, showError],
  );

  const handleReactMessage = useCallback(
    (target: MessageDto, emoji: string) => {
      if (!selectedId) return;
      void bridge
        .reactMessage(selectedId, target.msg_id, emoji)
        .then((updated) => setMessages((list) => upsertMsg(list, updated)))
        .catch(showError);
    },
    [selectedId, showError],
  );

  const handleRecallMessage = useCallback(
    (target: MessageDto) => {
      if (!selectedId) return;
      void bridge
        .recallMessage(selectedId, target.msg_id)
        .then((updated) => setMessages((list) => upsertMsg(list, updated)))
        .catch(showError);
    },
    [selectedId, showError],
  );

  const handleDeleteLocalMessage = useCallback(
    (target: MessageDto) => {
      if (!selectedId) return;
      void bridge
        .deleteMessageLocal(selectedId, target.msg_id)
        .then(() => setMessages((list) => list.filter((x) => x.msg_id !== target.msg_id)))
        .catch(showError);
    },
    [selectedId, showError],
  );

  const replyGraph = useMemo(() => {
    const byId = new Map<string, MessageDto>();
    for (const m of messages) {
      byId.set(m.msg_id, m);
    }
    const resolveRoot = (msgId: string): string => {
      let cur = msgId;
      const visited = new Set<string>([cur]);
      while (true) {
        const node = byId.get(cur);
        const parentId = node?.quote?.msg_id || localThreadLinks[cur];
        if (!parentId || !byId.has(parentId) || visited.has(parentId)) {
          return cur;
        }
        visited.add(parentId);
        cur = parentId;
      }
    };
    const repliesByRoot: Record<string, MessageDto[]> = {};
    for (const m of messages) {
      const parentId = m.quote?.msg_id || localThreadLinks[m.msg_id];
      if (!parentId) continue;
      const rootId = byId.has(parentId) ? resolveRoot(parentId) : parentId;
      if (!repliesByRoot[rootId]) repliesByRoot[rootId] = [];
      repliesByRoot[rootId].push(m);
    }
    return { byId, resolveRoot, repliesByRoot };
  }, [messages, localThreadLinks]);

  const activeThreadRootMsg = useMemo(() => {
    if (!activeThreadRootId) return null;
    const hit = replyGraph.byId.get(activeThreadRootId);
    if (hit) return hit;
    const childList = replyGraph.repliesByRoot[activeThreadRootId];
    const firstQuote = childList?.[0]?.quote;
    if (!firstQuote) return null;
    return {
      msg_id: activeThreadRootId,
      thread_id: selectedId ?? "",
      sender_id: "",
      sender_name: firstQuote.sender_name || "Tin nhắn gốc",
      text: firstQuote.snippet,
      msg_type: "chat.text",
      ts: childList[0].ts,
      is_outgoing: false,
      is_recalled: false,
      is_pinned: false,
      quote: null,
      reactions: [],
      attachment_url: null,
      attachment_name: null,
    } satisfies MessageDto;
  }, [activeThreadRootId, replyGraph, selectedId]);

  const handleOpenReplyThread = useCallback(
    (target: MessageDto) => {
      const hasOwnReplies = (replyGraph.repliesByRoot[target.msg_id]?.length ?? 0) > 0;
      const rootId =
        hasOwnReplies || !target.quote?.msg_id
          ? target.msg_id
          : replyGraph.byId.has(target.quote.msg_id)
            ? replyGraph.resolveRoot(target.quote.msg_id)
            : target.quote.msg_id;
      setActiveThreadRootId(rootId);
    },
    [replyGraph],
  );

  const handleSendAttachment = async (
    att: PendingAttachment,
    caption: string,
  ): Promise<MessageDto | null> => {
    if (!selectedId) return null;
    try {
      const msg =
        att.kind === "path"
          ? await bridge.sendAttachmentPath(selectedId, att.path, caption)
          : await bridge.sendRawAttachment(
              selectedId,
              att.fileName,
              new Uint8Array(await att.file.arrayBuffer()),
              caption,
              att.previewUrl,
            );
      setMessages((prev) => upsertMsg(prev, msg));
      setThreads((prev) => bumpThreadList(prev, msg, false));
      setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
      return msg;
    } catch (err) {
      showError(err);
      return null;
    }
  };

  const handleSendThreadAttachment = async (
    att: PendingAttachment,
    caption: string,
    rootMsgId: string,
  ) => {
    const sent = await handleSendAttachment(att, caption);
    if (!sent) return;
    setLocalThreadLinks((prev) => {
      const entries = Object.entries({ ...prev, [sent.msg_id]: rootMsgId }).slice(-500);
      const next = Object.fromEntries(entries);
      try {
        localStorage.setItem("litechat.threadLinks", JSON.stringify(next));
      } catch {
        // Ignore quota limits
      }
      return next;
    });
  };

  const handleSendBuzz = async () => {
    if (!selectedId) return;
    const now = Date.now();
    const until = buzzCooldownUntil[selectedId] ?? 0;
    if (until > now) {
      showError(`Vui lòng chờ ${Math.ceil((until - now) / 1000)} giây trước khi gửi BUZZ tiếp.`);
      return;
    }
    try {
      const msg = await bridge.sendText(selectedId, BUZZ_TEXT);
      setBuzzCooldownUntil((prev) => ({ ...prev, [selectedId]: Date.now() + BUZZ_COOLDOWN_MS }));
      setBuzzNow(Date.now());
      setMessages((prev) => upsertMsg(prev, msg));
      setThreads((prev) => bumpThreadList(prev, msg, false));
      setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
    } catch (err) {
      showError(err);
    }
  };

  const cycleTheme = () => {
    setTheme((prev) =>
      prev === "triforce"
        ? "night-blue"
        : prev === "night-blue"
          ? "dark"
          : prev === "dark"
            ? "light"
            : "triforce",
    );
  };

  if (authChecking) {
    return (
      <>
        <TriforceEffects enabled={fxEnabled} />
        <div
          className="relative z-10 flex h-full w-full flex-col items-center justify-center gap-3 text-sm"
          style={{ background: "var(--bg-stage)", color: "var(--text-secondary)" }}
        >
          <RefreshCw className="h-6 w-6 animate-spin" style={{ color: "var(--accent)" }} />
          <span>Đang xác minh phiên đăng nhập...</span>
        </div>
      </>
    );
  }

  if (!authed) {
    return (
      <>
        <TriforceEffects enabled={fxEnabled} />
        <div className="relative z-10 h-full w-full">
          <QrLoginModal
            qrSvg={qrSvg}
            statusText={qrStatus}
            errorText={qrError}
            onRefresh={() => void refreshQr()}
            onEnterDemo={() => setAuthed(true)}
          />
        </div>
      </>
    );
  }

  return (
    <div className={`relative flex h-full w-full overflow-hidden ${buzzShaking ? "buzz-shaking" : ""}`}>
      <TriforceEffects enabled={fxEnabled} />
      {/* Column 1: Folder Rail */}
      <aside
        className="relative z-10 flex w-[78px] shrink-0 flex-col items-center justify-between py-3 backdrop-blur-md"
        style={{ background: "var(--bg-rail)", borderRight: "1px solid var(--border-subtle)" }}
      >
        <div className="flex w-full flex-col items-center gap-1.5 px-1.5">
          {FOLDERS.map((item) => {
            const Icon = item.icon;
            const active = folder === item.id;
            return (
              <button
                key={item.id}
                onClick={() => setFolder(item.id)}
                className="hover-rail flex w-full flex-col items-center gap-1 rounded-xl py-2 text-xs font-medium transition"
                style={{
                  background: active ? "var(--rail-active-bg)" : "transparent",
                  color: active ? "var(--rail-text-active)" : "var(--rail-text)",
                }}
              >
                <Icon className="h-5 w-5" />
                <span className="truncate">{item.label}</span>
              </button>
            );
          })}
        </div>

        <div className="flex flex-col items-center gap-2">
          {myProfile?.uid && (
            <button
              type="button"
              onClick={() => setProfileUid(myProfile.uid)}
              title={
                myProfile.display_name
                  ? `Tài khoản: ${myProfile.display_name}`
                  : "Thông tin tài khoản của bạn"
              }
              className="rounded-full p-0.5 transition hover:opacity-85"
            >
              <ThreadAvatar
                avatar={myProfile.avatar}
                title={myProfile.display_name || myProfile.uid}
                kind="direct"
                size="sm"
              />
            </button>
          )}
          <div
            title={`WebSocket: ${connStatus}`}
            className="flex items-center justify-center p-1.5"
            style={{
              color:
                connStatus === "connected"
                  ? "#10b981"
                  : connStatus === "connecting"
                    ? "#f59e0b"
                    : "#ef4444",
            }}
          >
            <Wifi className="h-4 w-4" />
          </div>
          <button
            onClick={() => setBuzzEnabled((v) => !v)}
            title={
              buzzEnabled
                ? "Âm thanh & rung khi nhận BUZZ: Bật (nhấn để tắt)"
                : "Âm thanh & rung khi nhận BUZZ: Tắt (nhấn để bật)"
            }
            className="hover-rail rounded-lg p-2 transition"
            style={{ color: buzzEnabled ? "#f59e0b" : "var(--rail-text)" }}
          >
            <Zap className="h-5 w-5" />
          </button>
          <button
            onClick={() => setTranslateModalOpen(true)}
            title="Cài đặt dịch thuật tin nhắn (DeepL / Gemini / Azure)"
            className="hover-rail rounded-lg p-2 transition"
            style={{ color: "var(--rail-text)" }}
          >
            <Languages className="h-5 w-5" />
          </button>
          <button
            onClick={() => setUpdateModalOpen(true)}
            title={
              availableUpdate
                ? `Có bản cập nhật mới v${availableUpdate.version} — Nhấn để cập nhật`
                : `Kiểm tra cập nhật (hiện tại v${appVersion})`
            }
            className="hover-rail relative rounded-lg p-2 transition"
            style={{ color: availableUpdate ? "var(--accent)" : "var(--rail-text)" }}
          >
            <ArrowDownToLine className="h-5 w-5" />
            {availableUpdate && (
              <span className="absolute right-1.5 top-1.5 h-2 w-2 rounded-full bg-emerald-400" />
            )}
          </button>
          <button
            onClick={() => setFxEnabled((v) => !v)}
            title={fxEnabled ? "Hiệu ứng tiên sáng: Bật (nhấn để tắt)" : "Hiệu ứng tiên sáng: Tắt (nhấn để bật)"}
            className="hover-rail rounded-lg p-2 transition"
            style={{ color: fxEnabled ? "var(--rail-text-active)" : "var(--rail-text)" }}
          >
            <Sparkles className="h-5 w-5" />
          </button>
          <button
            onClick={cycleTheme}
            title={`Đổi giao diện (hiện tại: ${
              theme === "triforce"
                ? "Obsidian Vàng Kim"
                : theme === "night-blue"
                  ? "Night Blue"
                  : theme === "dark"
                    ? "Dark"
                    : "Light"
            })`}
            className="hover-rail rounded-lg p-2 transition"
            style={{ color: "var(--rail-text)" }}
          >
            {theme === "light" ? <Moon className="h-5 w-5" /> : <Sun className="h-5 w-5" />}
          </button>
          <button
            onClick={() => {
              void bridge.logout().then(() => {
                setAuthed(false);
                resetSessionUi();
                void refreshQr();
              });
            }}
            title="Đăng xuất"
            className="hover-rail rounded-lg p-2 transition"
            style={{ color: "var(--rail-text)" }}
          >
            <LogOut className="h-5 w-5" />
          </button>
        </div>
      </aside>

      {/* Column 2: Resizable Thread List */}
      <section
        className="relative z-10 flex shrink-0 flex-col backdrop-blur-md"
        style={{
          width: `${sidebarWidth}px`,
          background: "var(--bg-sidebar)",
          borderRight: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex items-center gap-1.5 p-3">
          <div
            className="flex flex-1 items-center gap-2 rounded-full px-3 py-1.5"
            style={{ background: "var(--bg-surface)" }}
          >
            <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
            <input
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              placeholder="Tìm hội thoại..."
              className="w-full bg-transparent text-[15px] outline-none"
            />
          </div>
          <button
            onClick={() => void syncRemoteThreads()}
            title="Đồng bộ lại danh bạ"
            className="hover-menu rounded-lg p-2 transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            <RefreshCw className={`h-4 w-4 ${syncing ? "animate-spin" : ""}`} />
          </button>
          <button
            onClick={() => setQuickSearchOpen(true)}
            title="Tìm kiếm toàn cục (Ctrl+K)"
            className="hover-menu rounded-lg px-2 py-1.5 text-xs font-medium transition"
            style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
          >
            Ctrl+K
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-2 pb-2">
          {visibleThreads.length === 0 ? (
            <div className="px-4 py-12 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {filterText.trim()
                ? "Không tìm thấy hội thoại phù hợp"
                : folder === "unread"
                  ? "Không có tin nhắn chưa đọc"
                  : "Chưa có cuộc hội thoại nào"}
            </div>
          ) : (
            visibleThreads.map((t) => {
              const active = t.id === selectedId;
              const unread = t.unread_count > 0;
              const fallbackPreview =
                t.kind === "group"
                  ? "Nhóm trò chuyện"
                  : t.kind === "cloud"
                    ? "Lưu trữ cá nhân & ghi chú nhanh"
                    : "Nhấn để trò chuyện";
              return (
                <div
                  key={t.id}
                  role="button"
                  tabIndex={0}
                  onClick={() => setSelectedId(t.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setSelectedId(t.id);
                    }
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setThreadMenu({ thread: t, x: e.clientX, y: e.clientY });
                  }}
                  className={`tf-row mb-0.5 flex w-full cursor-pointer items-center gap-3 rounded-xl px-2.5 py-2.5 text-left transition ${
                    active ? "" : "hover-surface"
                  }`}
                  style={{
                    background: active ? "var(--bg-active)" : undefined,
                    color: active ? "var(--text-on-active)" : "var(--text-primary)",
                  }}
                >
                  {t.kind === "direct" ? (
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        setProfileUid(t.id);
                      }}
                      title="Tra cứu thông tin tài khoản"
                      className="shrink-0 rounded-full transition hover:opacity-85"
                    >
                      <ThreadAvatar avatar={t.avatar} title={t.title} kind={t.kind} size="md" />
                    </button>
                  ) : (
                    <ThreadAvatar avatar={t.avatar} title={t.title} kind={t.kind} size="md" />
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2">
                      <span
                        className={`truncate text-[15px] ${unread ? "font-bold" : "font-semibold"}`}
                        style={{
                          color: active
                            ? "var(--text-on-active)"
                            : unread
                              ? "var(--text-primary)"
                              : "var(--list-title)",
                        }}
                      >
                        {t.title}
                      </span>
                      <div
                        className="flex shrink-0 items-center gap-1 text-xs"
                        style={{
                          color: active ? "var(--text-secondary-on-active)" : "var(--text-secondary)",
                        }}
                      >
                        {t.is_muted && <BellOff className="h-3 w-3 opacity-75" />}
                        {t.is_pinned && <Pin className="h-3 w-3 opacity-80" />}
                        {t.last_ts > 0 && <span>{formatThreadTime(t.last_ts)}</span>}
                      </div>
                    </div>
                    <div className="mt-0.5 flex items-center justify-between gap-2">
                      <span
                        className={`truncate text-[13.5px] ${unread ? "font-medium" : ""}`}
                        style={{
                          color: active
                            ? "var(--text-secondary-on-active)"
                            : unread
                              ? "var(--text-primary)"
                              : "var(--text-secondary)",
                        }}
                      >
                        {t.last_preview || fallbackPreview}
                      </span>
                      {unread && (
                        <span
                          className="shrink-0 rounded-full px-1.5 py-0.5 text-xs font-bold"
                          style={{
                            background: active
                              ? "rgba(255,255,255,0.2)"
                              : t.is_muted
                                ? "var(--chip-bg)"
                                : "var(--accent)",
                            color: active
                              ? "var(--text-on-active)"
                              : t.is_muted
                                ? "var(--text-secondary)"
                                : "var(--text-on-accent)",
                          }}
                        >
                          {t.unread_count > 99 ? "99+" : t.unread_count}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>

        <div
          role="separator"
          aria-orientation="vertical"
          title="Kéo để thay đổi độ rộng danh sách"
          onPointerDown={(e) => {
            e.preventDefault();
            const startX = e.clientX;
            const startW = sidebarWidth;
            const onMove = (ev: PointerEvent) => {
              const next = Math.min(440, Math.max(240, startW + (ev.clientX - startX)));
              setSidebarWidth(next);
            };
            const onUp = (ev: PointerEvent) => {
              const finalW = Math.min(440, Math.max(240, startW + (ev.clientX - startX)));
              localStorage.setItem("litechat.sidebarWidth", String(finalW));
              window.removeEventListener("pointermove", onMove);
              window.removeEventListener("pointerup", onUp);
            };
            window.addEventListener("pointermove", onMove);
            window.addEventListener("pointerup", onUp);
          }}
          className="absolute inset-y-0 -right-1 z-20 w-2 cursor-col-resize hover:bg-sky-500/30"
        />
      </section>

      {/* Column 3: Chat Stage */}
      <main className="relative z-10 flex min-w-0 flex-1 flex-col">
        {buzzToast && (
          <button
            type="button"
            onClick={() => {
              setSelectedId(buzzToast.threadId);
              setActiveThreadRootId(null);
              setBuzzToast(null);
            }}
            className="absolute left-1/2 top-16 z-40 flex -translate-x-1/2 items-center gap-2 rounded-full bg-amber-500 px-4 py-2 text-[13px] font-bold text-slate-950 shadow-2xl transition hover:scale-105"
          >
            <Zap className="h-4 w-4 fill-slate-950" />
            <span>
              {buzzToast.senderName} vừa gửi BUZZ! ({buzzToast.threadTitle})
            </span>
          </button>
        )}
        {toastError && (
          <div className="absolute right-4 top-16 z-40 rounded-xl bg-rose-600/95 px-4 py-2 text-xs text-white shadow-lg">
            {toastError}
          </div>
        )}

        {dragHover && activeThread && (
          <div className="pointer-events-none absolute inset-0 z-40 flex flex-col items-center justify-center gap-2 border-2 border-dashed border-sky-400 bg-sky-950/75 text-white backdrop-blur-xs">
            <UploadCloud className="h-10 w-10 text-sky-300" />
            <div className="text-sm font-semibold">Thả tệp hoặc hình ảnh vào đây để gửi</div>
            <div className="text-xs text-sky-200/80">Hỗ trợ thả 1 hoặc nhiều tệp (tối đa 100 MB/tệp)</div>
          </div>
        )}

        {isTauri() && connStatus === "reconnecting" && (
          <div className="flex shrink-0 items-center justify-center gap-2 bg-amber-500/20 px-3 py-1 text-xs font-medium text-amber-500">
            <RefreshCw className="h-3.5 w-3.5 animate-spin" />
            <span>Mất kết nối máy chủ — đang tự động kết nối lại...</span>
          </div>
        )}

        {activeThread ? (
          activeThreadRootMsg ? (
            <ReplyThreadPanel
              threadTitle={activeThread.title}
              rootMessage={activeThreadRootMsg}
              replies={replyGraph.repliesByRoot[activeThreadRootMsg.msg_id] ?? []}
              threadKind={activeThread.kind}
              pendingAttachment={pendingAttachment}
              onPendingAttachmentChange={setPendingAttachment}
              translationsByMsg={translationsByMsg}
              replaceWithTranslation={Boolean(activeThreadLang)}
              drawerOpen={drawerOpen}
              onToggleDrawer={() => setDrawerOpen((v) => !v)}
              onClose={() => setActiveThreadRootId(null)}
              onJumpToMsgInMainChat={(msgId) => {
                setActiveThreadRootId(null);
                setTimeout(() => void jumpToMsg(msgId), 60);
              }}
              onSendThreadReply={handleSendThreadReply}
              onSendThreadAttachment={handleSendThreadAttachment}
              onBuzz={handleSendBuzz}
              buzzCooldownSec={buzzCooldownSec}
              onReact={handleReactMessage}
              onRecall={handleRecallMessage}
              onDeleteLocal={handleDeleteLocalMessage}
              onOpenImage={(url, title) => setLightbox({ url, title })}
              onOpenProfile={(uid) => setProfileUid(uid)}
              onForward={(target) => setForwardMsg(target)}
              onTranslate={handleTranslateSingle}
              onHideTranslation={(msgId) =>
                setTranslationsByMsg((prev) => {
                  const next = { ...prev };
                  delete next[msgId];
                  return next;
                })
              }
              onError={showError}
            />
          ) : (
            <>
              <header
              className="flex h-14 shrink-0 items-center justify-between px-4"
              style={{
                background: "var(--bg-sidebar)",
                borderBottom: "1px solid var(--border-subtle)",
              }}
            >
              <div className="flex min-w-0 items-center gap-3">
                {activeThread.kind === "direct" ? (
                  <button
                    type="button"
                    onClick={() => setProfileUid(activeThread.id)}
                    title="Tra cứu thông tin tài khoản"
                    className="shrink-0 rounded-full transition hover:opacity-85"
                  >
                    <ThreadAvatar
                      avatar={activeThread.avatar}
                      title={activeThread.title}
                      kind={activeThread.kind}
                      size="sm"
                    />
                  </button>
                ) : (
                  <ThreadAvatar
                    avatar={activeThread.avatar}
                    title={activeThread.title}
                    kind={activeThread.kind}
                    size="sm"
                  />
                )}
                <div className="min-w-0">
                  <h2 className="truncate text-[16px] font-semibold">{activeThread.title}</h2>
                  <p className="text-[13px]" style={{ color: "var(--text-secondary)" }}>
                    {activeThread.kind === "group"
                      ? "Nhóm trò chuyện"
                      : activeThread.kind === "cloud"
                        ? "Lưu trữ cá nhân"
                        : "Trò chuyện 1-1"}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={handleToggleThreadTranslate}
                  title={
                    activeThreadLang
                      ? "Tắt dịch toàn bộ hội thoại"
                      : "Dịch toàn bộ hội thoại"
                  }
                  className="rounded-lg p-2 transition hover:opacity-80"
                  style={{
                    color: activeThreadLang ? "var(--accent)" : "var(--text-secondary)",
                    background: activeThreadLang ? "var(--accent-soft)" : "transparent",
                  }}
                >
                  <Languages className="h-4 w-4" />
                </button>
                <button
                  onClick={() => setQuickSearchOpen(true)}
                  className="rounded-lg p-2 hover:opacity-80"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <Search className="h-4 w-4" />
                </button>
                <button
                  onClick={() => {
                    if (activeThreadRootId) {
                      setActiveThreadRootId(null);
                      setDrawerOpen(true);
                    } else {
                      setDrawerOpen((v) => !v);
                    }
                  }}
                  title="Thông tin hội thoại"
                  className="rounded-lg p-2 hover:opacity-80"
                  style={{
                    color: drawerOpen && !activeThreadRootId ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <Info className="h-4 w-4" />
                </button>
              </div>
            </header>

            {activeThreadLang && (
              <div
                className="flex shrink-0 flex-wrap items-center justify-between gap-2 px-4 py-2 text-[13px]"
                style={{
                  background: "var(--bg-surface)",
                  borderBottom: "1px solid var(--border-subtle)",
                }}
              >
                <div className="flex items-center gap-2">
                  <Languages className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                  <span className="font-medium">
                    Đang dịch hội thoại sang:
                  </span>
                  <select
                    value={activeThreadLang}
                    onChange={(e) => {
                      const nextLang = e.target.value;
                      setThreadAutoTranslate((prev) => ({
                        ...prev,
                        [activeThread.id]: nextLang,
                      }));
                    }}
                    className="rounded-lg px-2 py-0.5 text-[13px] font-semibold outline-none"
                    style={{
                      background: "var(--bg-sidebar)",
                      border: "1px solid var(--border)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {TRANSLATE_LANG_OPTIONS.map((opt) => (
                      <option key={opt.code} value={opt.code}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                  {threadTranslating && (
                    <span className="flex items-center gap-1 text-xs" style={{ color: "var(--text-secondary)" }}>
                      <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                      Đang dịch...
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={() => setTranslateModalOpen(true)}
                    className="hover-menu flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <Settings className="h-3.5 w-3.5" />
                    API: {translateCfg?.provider?.toUpperCase() || "DEEPL"}
                  </button>
                  <button
                    type="button"
                    onClick={handleToggleThreadTranslate}
                    className="hover-menu rounded-lg px-2.5 py-1 text-xs font-semibold transition"
                    style={{ color: "var(--accent)" }}
                  >
                    Không dịch nữa
                  </button>
                </div>
              </div>
            )}

            <div
              ref={scrollContainerRef}
              onScroll={(e) => {
                const el = e.currentTarget;
                if (el.scrollTop < 120 && hasMoreHistory && !loadingOlder) {
                  void loadOlderMessages();
                }
                const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
                const farUp = distFromBottom > 220;
                setShowScrollDown(farUp);
                if (!farUp) setUnreadBelow(0);
              }}
              className="relative flex-1 overflow-y-auto px-6 py-4"
            >
              {loadingMessages && messages.length === 0 && (
                <div className="flex h-full items-center justify-center gap-2 text-[14px]" style={{ color: "var(--text-secondary)" }}>
                  <RefreshCw className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
                  <span>Đang tải tin nhắn...</span>
                </div>
              )}
              {!loadingMessages && messages.length === 0 && (
                <div className="flex h-full items-center justify-center text-[14px]" style={{ color: "var(--text-secondary)" }}>
                  Chưa có tin nhắn nào. Hãy gửi lời chào đầu tiên!
                </div>
              )}
              {hasMoreHistory && (
                <div className="mb-3 flex justify-center">
                  <button
                    type="button"
                    disabled={loadingOlder}
                    onClick={() => void loadOlderMessages()}
                    className="rounded-full px-3.5 py-1 text-[13px] font-medium shadow-xs transition hover:opacity-85 disabled:opacity-60"
                    style={{ background: "var(--pill-bg)", color: "var(--pill-text)" }}
                  >
                    {loadingOlder ? "Đang tải tin nhắn cũ hơn..." : "Tải thêm tin nhắn cũ"}
                  </button>
                </div>
              )}
              {messages.map((m, idx) => {
                const prev = idx > 0 ? messages[idx - 1] : null;
                const showDate = !prev || dayKey(prev.ts) !== dayKey(m.ts);
                const groupedWithPrev =
                  !!prev &&
                  !showDate &&
                  prev.sender_id === m.sender_id &&
                  prev.is_outgoing === m.is_outgoing &&
                  m.ts - prev.ts < 120_000;

                return (
                  <div key={m.msg_id}>
                    {showDate && (
                      <div className="my-4 flex justify-center">
                        <span
                          className="rounded-full px-3 py-0.5 text-xs font-medium shadow-xs"
                          style={{ background: "var(--pill-bg)", color: "var(--pill-text)" }}
                        >
                          {formatDayLabel(m.ts)}
                        </span>
                      </div>
                    )}
                    <MessageBubble
                      msg={m}
                      threadKind={activeThread.kind}
                      groupedWithPrev={groupedWithPrev}
                      highlighted={highlightedId === m.msg_id}
                      translation={translationsByMsg[m.msg_id]}
                      replaceWithTranslation={Boolean(activeThreadLang)}
                      replyCount={replyGraph.repliesByRoot[m.msg_id]?.length ?? 0}
                      onReply={(target) => setReplyTo(target)}
                      onReact={handleReactMessage}
                      onRecall={handleRecallMessage}
                      onDeleteLocal={handleDeleteLocalMessage}
                      onJumpToMsg={(msgId) => void jumpToMsg(msgId)}
                      onOpenImage={(url, title) => setLightbox({ url, title })}
                      onOpenProfile={(uid) => setProfileUid(uid)}
                      onForward={(target) => setForwardMsg(target)}
                      onTranslate={handleTranslateSingle}
                      onHideTranslation={(msgId) =>
                        setTranslationsByMsg((prev) => {
                          const next = { ...prev };
                          delete next[msgId];
                          return next;
                        })
                      }
                      onOpenThread={handleOpenReplyThread}
                    />
                  </div>
                );
              })}
              <div ref={bottomRef} />
            </div>

            {showScrollDown && (
              <button
                type="button"
                onClick={() => {
                  bottomRef.current?.scrollIntoView({ behavior: "smooth" });
                  setUnreadBelow(0);
                  setShowScrollDown(false);
                }}
                title="Cuộn xuống tin nhắn mới nhất"
                className="absolute bottom-20 right-6 z-20 flex items-center gap-1.5 rounded-full px-3 py-2 text-xs font-medium shadow-lg transition hover:opacity-90"
                style={{
                  background: "var(--bg-sidebar)",
                  color: "var(--text-primary)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <ChevronDown className="h-4 w-4" style={{ color: "var(--accent)" }} />
                {unreadBelow > 0 && (
                  <span
                    className="rounded-full px-1.5 py-0.5 text-[10px] font-bold"
                    style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                  >
                    {unreadBelow}
                  </span>
                )}
              </button>
            )}

            <Composer
              replyTo={replyTo}
              pendingAttachment={pendingAttachment}
              onPendingAttachmentChange={setPendingAttachment}
              onCancelReply={() => setReplyTo(null)}
              onSendText={handleSendText}
              onSendAttachment={handleSendAttachment}
              onBuzz={handleSendBuzz}
              buzzCooldownSec={buzzCooldownSec}
              onError={showError}
            />
          </>
          )
        ) : (
          <div
            className="flex flex-1 items-center justify-center text-sm"
            style={{ color: "var(--text-secondary)" }}
          >
            Chọn một cuộc trò chuyện để bắt đầu
          </div>
        )}
      </main>

      {/* Right Info Drawer */}
      {drawerOpen && activeThread && (
        <aside
          className="fixed inset-y-0 right-0 z-30 flex w-80 shrink-0 flex-col overflow-hidden p-4 shadow-2xl backdrop-blur-md lg:static lg:z-10 lg:shadow-none"
          style={{
            background: "var(--bg-sidebar)",
            borderLeft: "1px solid var(--border-subtle)",
          }}
        >
          <div className="flex items-center justify-between">
            <h3
              className="text-[13px] font-semibold uppercase tracking-wider"
              style={{ color: "var(--text-secondary)" }}
            >
              Thông tin hội thoại
            </h3>
            <button
              type="button"
              onClick={() => setDrawerOpen(false)}
              className="rounded-lg p-1 hover:bg-white/10"
              style={{ color: "var(--text-secondary)" }}
              title="Đóng"
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <div className="mt-4 flex flex-col items-center text-center">
            {activeThread.kind === "direct" ? (
              <button
                type="button"
                onClick={() => setProfileUid(activeThread.id)}
                title="Tra cứu thông tin tài khoản"
                className="rounded-full transition hover:opacity-85"
              >
                <ThreadAvatar
                  avatar={activeThread.avatar}
                  title={activeThread.title}
                  kind={activeThread.kind}
                  size="lg"
                />
              </button>
            ) : (
              <ThreadAvatar
                avatar={activeThread.avatar}
                title={activeThread.title}
                kind={activeThread.kind}
                size="lg"
              />
            )}
            <div className="mt-2 text-[15.5px] font-semibold">{activeThread.title}</div>
            <div className="text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {activeThread.kind === "group"
                ? threadDetails?.total_members
                  ? `Nhóm · ${threadDetails.total_members} thành viên`
                  : "Nhóm trò chuyện"
                : activeThread.kind === "cloud"
                  ? "Cloud của tôi"
                  : "Trò chuyện 1-1"}
            </div>
            {activeThread.kind === "direct" && (
              <button
                type="button"
                onClick={() => setProfileUid(activeThread.id)}
                className="mt-1 text-xs hover:underline"
                style={{ color: "var(--accent)" }}
              >
                Tra cứu thông tin tài khoản
              </button>
            )}
            <div className="mt-0.5 text-xs opacity-60">ID: {activeThread.id}</div>
          </div>

          <div className="mt-4 grid grid-cols-3 gap-1.5 text-xs">
            <button
              type="button"
              onClick={() => void handleTogglePin(activeThread)}
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: activeThread.is_pinned ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              {activeThread.is_pinned ? <PinOff className="h-4 w-4" /> : <Pin className="h-4 w-4" />}
              <span>{activeThread.is_pinned ? "Bỏ ghim" : "Ghim"}</span>
            </button>
            <button
              type="button"
              onClick={() => void handleToggleMute(activeThread)}
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: activeThread.is_muted ? "#fbbf24" : "var(--text-secondary)",
              }}
            >
              {activeThread.is_muted ? <Bell className="h-4 w-4" /> : <BellOff className="h-4 w-4" />}
              <span>{activeThread.is_muted ? "Bật TB" : "Tắt TB"}</span>
            </button>
            <button
              type="button"
              onClick={() => void handleMarkUnread(activeThread)}
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <MessageSquare className="h-4 w-4" />
              <span>Chưa đọc</span>
            </button>
          </div>

          <div className="mt-4 flex-1 overflow-y-auto pr-0.5">
            {activeThread.kind === "group" && (
              <div className="mb-4">
                <div
                  className="mb-2 text-[13px] font-semibold"
                  style={{ color: "var(--text-secondary)" }}
                >
                  Thành viên ({threadDetails?.total_members ?? threadDetails?.members.length ?? "..."})
                </div>
                {detailsLoading && !threadDetails ? (
                  <div className="text-[13px] opacity-60">Đang tải thành viên...</div>
                ) : threadDetails && threadDetails.members.length > 0 ? (
                  <div className="max-h-48 space-y-1.5 overflow-y-auto">
                    {threadDetails.members.map((mem) => (
                      <button
                        key={mem.id}
                        type="button"
                        onClick={() => setProfileUid(mem.id)}
                        title="Tra cứu thông tin tài khoản"
                        className="hover-menu flex w-full items-center gap-2 rounded-lg px-1.5 py-1 text-left transition"
                      >
                        <ThreadAvatar avatar={mem.avatar} title={mem.name} kind="direct" size="sm" />
                        <div className="min-w-0 flex-1">
                          <div className="truncate text-[13.5px] font-medium">{mem.name}</div>
                          {mem.role !== "Thành viên" && (
                            <div className="text-xs" style={{ color: "var(--accent)" }}>
                              {mem.role}
                            </div>
                          )}
                        </div>
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="text-[13px] opacity-60">Chưa có danh sách thành viên</div>
                )}
              </div>
            )}

            <div>
              <div className="mb-2 flex gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                <button
                  type="button"
                  onClick={() => setDetailsTab("media")}
                  className="flex flex-1 items-center justify-center gap-1 rounded-lg py-1 text-[13px] font-medium transition"
                  style={{
                    background: detailsTab === "media" ? "var(--bg-hover)" : "transparent",
                    color: detailsTab === "media" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <ImageIcon className="h-3.5 w-3.5" />
                  Ảnh/Video ({threadDetails?.media_items.length ?? 0})
                </button>
                <button
                  type="button"
                  onClick={() => setDetailsTab("files")}
                  className="flex flex-1 items-center justify-center gap-1 rounded-lg py-1 text-[13px] font-medium transition"
                  style={{
                    background: detailsTab === "files" ? "var(--bg-hover)" : "transparent",
                    color: detailsTab === "files" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <FileText className="h-3.5 w-3.5" />
                  Tệp ({threadDetails?.file_items.length ?? 0})
                </button>
              </div>

              {detailsTab === "media" ? (
                threadDetails && threadDetails.media_items.length > 0 ? (
                  <div className="grid grid-cols-3 gap-1.5">
                    {threadDetails.media_items.slice(0, 18).map((item) => (
                      <button
                        key={item.msg_id}
                        type="button"
                        onClick={() =>
                          item.attachment_url &&
                          setLightbox({
                            url: item.attachment_url,
                            title: item.attachment_name || "Hình ảnh",
                          })
                        }
                        className="aspect-square overflow-hidden rounded-lg bg-black/25"
                      >
                        {item.attachment_url && (
                          <img
                            src={item.attachment_url}
                            alt={item.attachment_name || "Media"}
                            referrerPolicy="no-referrer"
                            className="h-full w-full object-cover"
                            loading="lazy"
                          />
                        )}
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="py-4 text-center text-xs opacity-60">Chưa có ảnh/video đã lưu</div>
                )
              ) : threadDetails && threadDetails.file_items.length > 0 ? (
                <div className="space-y-1.5">
                  {threadDetails.file_items.slice(0, 20).map((item) => (
                    <a
                      key={item.msg_id}
                      href={
                        item.attachment_url && /^https:\/\//i.test(item.attachment_url)
                          ? item.attachment_url
                          : undefined
                      }
                      target="_blank"
                      rel="noreferrer"
                      className="hover-menu flex items-center gap-2 rounded-lg px-2 py-1.5 text-xs transition"
                      style={{ background: "var(--bg-surface)" }}
                    >
                      <FileText className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                      <span className="truncate">{item.attachment_name || item.text}</span>
                    </a>
                  ))}
                </div>
              ) : (
                <div className="py-4 text-center text-xs opacity-60">Chưa có tệp đính kèm</div>
              )}
            </div>
          </div>
        </aside>
      )}

      {threadMenu && (
        <div
          className="fixed z-50 w-48 rounded-xl py-1 shadow-2xl"
          style={{
            left: Math.min(threadMenu.x, window.innerWidth - 200),
            top: Math.min(threadMenu.y, window.innerHeight - 140),
            background: "var(--bg-sidebar)",
            border: "1px solid var(--border-subtle)",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          <button
            type="button"
            onClick={() => {
              void handleTogglePin(threadMenu.thread);
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            {threadMenu.thread.is_pinned ? (
              <>
                <PinOff className="h-3.5 w-3.5" /> Bỏ ghim hội thoại
              </>
            ) : (
              <>
                <Pin className="h-3.5 w-3.5" /> Ghim lên đầu
              </>
            )}
          </button>
          <button
            type="button"
            onClick={() => {
              void handleMarkUnread(threadMenu.thread);
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            <MessageSquare className="h-3.5 w-3.5" /> Đánh dấu chưa đọc
          </button>
          <button
            type="button"
            onClick={() => {
              void handleToggleMute(threadMenu.thread);
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            {threadMenu.thread.is_muted ? (
              <>
                <Bell className="h-3.5 w-3.5" /> Bật thông báo
              </>
            ) : (
              <>
                <BellOff className="h-3.5 w-3.5" /> Tắt thông báo
              </>
            )}
          </button>
        </div>
      )}

      <UpdateModal
        open={updateModalOpen}
        currentVersion={appVersion}
        availableUpdate={availableUpdate}
        checking={checkingUpdate}
        errorText={updateError}
        lastCheckedAt={lastUpdateCheckAt}
        onCheckNow={() => checkForUpdates(false)}
        onClose={() => setUpdateModalOpen(false)}
      />

      <TranslateSettingsModal
        open={translateModalOpen}
        config={translateCfg}
        onClose={() => setTranslateModalOpen(false)}
        onSaved={(cfg) => {
          setTranslateCfg(cfg);
          setTranslationsByMsg({});
          if (selectedId && threadAutoTranslate[selectedId]) {
            setThreadAutoTranslate((prev) => ({
              ...prev,
              [selectedId]: cfg.target_lang || "VI",
            }));
          }
        }}
      />

      <QuickSearchModal
        open={quickSearchOpen}
        threads={threads}
        onClose={() => setQuickSearchOpen(false)}
        onSelectMessage={(hit) => void handleSelectSearchResult(hit)}
      />

      <ForwardModal
        message={forwardMsg}
        threads={threads}
        onClose={() => setForwardMsg(null)}
        onForwardTo={async (targetThreadId, msg) => {
          const content =
            msg.attachment_url && /^https:\/\//i.test(msg.attachment_url)
              ? `${msg.text ? `${msg.text}\n` : ""}${msg.attachment_url}`
              : msg.text;
          if (!content.trim()) return;
          try {
            const sent = await bridge.sendText(targetThreadId, content);
            setThreads((prev) => bumpThreadList(prev, sent, false));
            if (targetThreadId === selectedIdRef.current) {
              setMessages((prev) => upsertMsg(prev, sent));
              setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
            }
          } catch (err) {
            showError(err);
          }
        }}
      />

      <MediaLightbox
        url={lightbox?.url ?? null}
        title={lightbox?.title ?? ""}
        onClose={() => setLightbox(null)}
      />

      <UserProfileModal
        userId={profileUid}
        onClose={() => setProfileUid(null)}
        onOpenImage={(url, title) => setLightbox({ url, title })}
        onStartChat={(profile) => {
          setThreads((prev) => {
            if (prev.some((t) => t.id === profile.uid)) return prev;
            const created: ThreadDto = {
              id: profile.uid,
              title: profile.display_name || profile.uid,
              avatar: profile.avatar,
              kind: "direct",
              last_preview: "",
              last_ts: 0,
              unread_count: 0,
              is_pinned: false,
              is_muted: false,
            };
            return [created, ...prev].sort(compareThreads);
          });
          setSelectedId(profile.uid);
        }}
      />
    </div>
  );
}
