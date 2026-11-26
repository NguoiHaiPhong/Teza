import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Archive,
  ArchiveRestore,
  ArrowLeft,
  Bell,
  BellOff,
  Bookmark,
  Briefcase,
  Check,
  CheckCheck,
  ChevronDown,
  ChevronUp,
  Download,
  ExternalLink,
  FileText,
  Flame,
  Folder,
  FolderPlus,
  Forward,
  Heart,
  Image as ImageIcon,
  Info,
  Languages,
  LayoutGrid,
  Link2,
  Loader2,
  Lock,
  LogOut,
  Menu,
  MessageSquare,
  MessagesSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Phone,
  Pin,
  PinOff,
  Play,
  RefreshCw,
  Search,
  Settings,
  Shield,
  Sparkles,
  Star,
  Tag,
  Trash2,
  UploadCloud,
  User,
  UserCheck,
  UserMinus,
  UserPlus,
  Users,
  Video,
  X,
  Zap,
} from "lucide-react";
import { bridge, isTauri } from "./bridge";
import {
  BUZZ_COOLDOWN_MS,
  BUZZ_TEXT,
  type BuzzStreakMap,
  isBuzzMessage,
  playBuzzSound,
  recordIncomingBuzz,
  resetBuzzStreak,
} from "./utils/buzz";
import {
  classifyDrawerFileAction,
  isDataImageUrl,
  normalizeMediaRequestUrl,
  setRelayMediaRouting,
} from "./utils/mediaCache";
import { playMessageSound, showDesktopNotification } from "./utils/notify";
import {
  computeTotalUnreadBadge,
  formatNotificationContent,
  isThreadCurrentlyMuted,
  loadNotifyPrefs,
  saveNotifyPrefs,
  shouldNotifyMessage,
} from "./utils/notifyPrefs";
import {
  applyOpenThreadSearchState,
  applyOptimisticReaction,
  applyPollUpdateToMessages,
  applyThreadReadSync,
  bumpThreadList,
  compareThreads,
  computeLayoutMode,
  computeThreadRenderLimitForTarget,
  countFolderUnread,
  dayKey,
  dedupeOptimisticPollMessages,
  extractFirstUrl,
  filterThreadsByFolder,
  findFirstUnreadMsgId,
  formatDayLabel,
  formatThreadTime,
  getDisplayBodyText,
  looksLikePhoneQuery,
  normalizeReactionEmoji,
  normalizeReactions,
  reconcileUpdatedMessage,
  shouldAutoMarkReadOnFocus,
  upsertMsg,
} from "./utils/threadState";
import { isImeComposingEvent, matchesSearchQuery } from "./utils/searchNormalize";
import {
  playCallCueTone,
  startIncomingRingtone,
} from "./utils/callRingtone";
import {
  canStartVoiceCall,
  classifyCallEndReason,
  formatCallDuration,
  isCameraFailureStatus,
  shouldCaptureLocalVideo,
  shouldShowVideoOverlay,
} from "./utils/callState";
import {
  LocalCameraEncoder,
  RemoteH264Renderer,
  listBrowserCameras,
  type CameraDeviceInfo,
  type LocalCameraStatus,
} from "./utils/videoCallMedia";
import {
  classifySkipTranslation,
  isEffectivelySameText,
  isSameLangCode,
  normalizeLangBase,
} from "./utils/translateFilter";
import { ActiveCallBar } from "./components/ActiveCallBar";
import { Composer } from "./components/Composer";
import { DownloadOverlay } from "./components/DownloadOverlay";
import { IncomingCallModal } from "./components/IncomingCallModal";
import type { LightboxItem } from "./components/MediaLightbox";
import { MessageBubble, type MessageTranslationState } from "./components/MessageBubble";
import { NavDrawer } from "./components/NavDrawer";
import { PasscodeLockScreen } from "./components/PasscodeLockScreen";
import { QrLoginModal } from "./components/QrLoginModal";
import { RelayImage } from "./components/RelayImage";
import { ReplyThreadPanel } from "./components/ReplyThreadPanel";
import type { DensityMode, FontSizeScale, SettingsTab } from "./components/SettingsModal";
import { ThreadAvatar } from "./components/ThreadAvatar";
import { TriforceEffects } from "./components/TriforceEffects";
import { useI18n, type TranslationKey } from "./i18n";
import {
  DEFAULT_PASSCODE_CONFIG,
  loadPasscodeConfig,
  savePasscodeConfig,
  verifyPasscode,
} from "./utils/passcode";
import {
  applyDownloadProgress,
  type DownloadProgressItem,
} from "./utils/downloadState";
import { buildComposerMentionMembers, isPlaceholderMemberName } from "./utils/mentions";
import { parsePollPayload } from "./utils/richMessage";
import {
  loadWebAppsConfig,
  saveWebAppsConfig,
  type WebAppItem,
  type WebAppsConfig,
} from "./utils/webApps";
import type {
  AccountProfile,
  AiConfigDto,
  AiStoredMessageDto,
  AiSummaryDto,
  AppUpdateInfo,
  BuiltInRailFolderId,
  CallDevicesDto,
  CallEventEnvelopeDto,
  CallSnapshotDto,
  CallStateDto,
  CallTelemetryDto,
  CallVideoFrameDto,
  CallVideoKeyframeRequestDto,
  ChatFolderDto,
  ConnectionStatus,
  DownloadProgressDto,
  FolderFilter,
  MessageDto,
  MonitorInfoDto,
  NotificationPrefs,
  NotifyToastPayload,
  PasscodeConfig,
  PendingAttachment,
  RailDisplayMode,
  RelayConfigSummary,
  SavedFileResultDto,
  ScheduledMessageDto,
  StickerItem,
  ThemeMode,
  ThreadDetailsDto,
  ThreadDto,
  ThreadMemberDto,
  TranslateConfigSummary,
  UserProfileDto,
} from "./types";

const AddGroupMembersModal = lazy(() =>
  import("./components/AddGroupMembersModal").then((m) => ({ default: m.AddGroupMembersModal })),
);
const AiChatPanel = lazy(() => import("./components/AiChatPanel"));
const CreatePollModal = lazy(() =>
  import("./components/CreatePollModal").then((m) => ({ default: m.CreatePollModal })),
);
const CreateReminderModal = lazy(() =>
  import("./components/CreateReminderModal").then((m) => ({ default: m.CreateReminderModal })),
);
const FindUserModal = lazy(() =>
  import("./components/FindUserModal").then((m) => ({ default: m.FindUserModal })),
);
const FolderEditorModal = lazy(() =>
  import("./components/FolderEditorModal").then((m) => ({ default: m.FolderEditorModal })),
);
const ForwardModal = lazy(() =>
  import("./components/ForwardModal").then((m) => ({ default: m.ForwardModal })),
);
const FriendRequestsModal = lazy(() => import("./components/FriendRequestsModal"));
const MediaLightbox = lazy(() =>
  import("./components/MediaLightbox").then((m) => ({ default: m.MediaLightbox })),
);
const QuickSearchModal = lazy(() =>
  import("./components/QuickSearchModal").then((m) => ({ default: m.QuickSearchModal })),
);
const ScheduledMessagesModal = lazy(() => import("./components/ScheduledMessagesModal"));
const ScreenshotEditorModal = lazy(() =>
  import("./components/ScreenshotEditorModal").then((m) => ({ default: m.ScreenshotEditorModal })),
);
const SettingsModal = lazy(() =>
  import("./components/SettingsModal").then((m) => ({ default: m.SettingsModal })),
);
const TranslateSettingsModal = lazy(() =>
  import("./components/TranslateSettingsModal").then((m) => ({
    default: m.TranslateSettingsModal,
  })),
);
const UpdateModal = lazy(() =>
  import("./components/UpdateModal").then((m) => ({ default: m.UpdateModal })),
);
const UserProfileModal = lazy(() =>
  import("./components/UserProfileModal").then((m) => ({ default: m.UserProfileModal })),
);
const VideoCallOverlay = lazy(() =>
  import("./components/VideoCallOverlay").then((m) => ({ default: m.VideoCallOverlay })),
);
const WebAppHub = lazy(() =>
  import("./components/WebAppHub").then((m) => ({ default: m.WebAppHub })),
);
const WebAppWorkspace = lazy(() =>
  import("./components/WebAppWorkspace").then((m) => ({ default: m.WebAppWorkspace })),
);

const FOLDER_ICON_MAP: Record<string, typeof Folder> = {
  folder: Folder,
  briefcase: Briefcase,
  star: Star,
  heart: Heart,
  users: Users,
  user: User,
  tag: Tag,
  bookmark: Bookmark,
  flame: Flame,
  shield: Shield,
};

function renderFolderIcon(iconId: string, className = "h-5 w-5") {
  const Icon = FOLDER_ICON_MAP[iconId] ?? Folder;
  return <Icon className={className} />;
}

const TRANSLATE_LANG_OPTIONS = [
  { code: "VI", label: "Tiếng Việt (VI)" },
  { code: "EN", label: "English (EN)" },
  { code: "JA", label: "日本語 (JA)" },
  { code: "KO", label: "한국어 (KO)" },
  { code: "ZH", label: "中文 (ZH)" },
  { code: "FR", label: "Français (FR)" },
  { code: "DE", label: "Deutsch (DE)" },
  { code: "RU", label: "Русский (RU)" },
  { code: "TH", label: "ภาษาไทย (TH)" },
  { code: "ID", label: "Bahasa Indonesia (ID)" },
];

const FOLDERS: { id: FolderFilter; labelKey: TranslationKey; icon: typeof MessageSquare }[] = [
  { id: "all", labelKey: "folderAll", icon: MessagesSquare },
  { id: "unread", labelKey: "folderUnread", icon: MessageSquare },
  { id: "direct", labelKey: "folderDirect", icon: User },
  { id: "group", labelKey: "folderGroup", icon: Users },
  { id: "cloud", labelKey: "folderCloud", icon: Bookmark },
];

const DEFAULT_CALL_STATE: CallStateDto = {
  available: false,
  unavailable_reason: null,
  phase: "idle",
  active: null,
};

export default function App() {
  const { lang, translateCode, t } = useI18n();
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

  const [folder, setFolder] = useState<string>("all");
  const [customFolders, setCustomFolders] = useState<ChatFolderDto[]>([]);
  const [folderModalOpen, setFolderModalOpen] = useState(false);
  const [editingFolder, setEditingFolder] = useState<ChatFolderDto | null>(null);
  const [folderMenu, setFolderMenu] = useState<{ folder: ChatFolderDto; x: number; y: number } | null>(
    null,
  );
  const [threads, setThreads] = useState<ThreadDto[]>([]);
  const [filterText, setFilterText] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [messages, setMessages] = useState<MessageDto[]>([]);
  const [hasMoreHistory, setHasMoreHistory] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [replyTo, setReplyTo] = useState<MessageDto | null>(null);
  const [replyFocusSeq, setReplyFocusSeq] = useState(0);
  const [activeDownloads, setActiveDownloads] = useState<DownloadProgressItem[]>([]);
  const [completedDownload, setCompletedDownload] = useState<SavedFileResultDto | null>(null);
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
  const [lightboxState, setLightboxState] = useState<{
    items: LightboxItem[];
    index: number;
  } | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(true);
  const [quickSearchOpen, setQuickSearchOpen] = useState(false);
  const [threadDetails, setThreadDetails] = useState<ThreadDetailsDto | null>(null);
  const [groupMembersCache, setGroupMembersCache] = useState<Record<string, ThreadMemberDto[]>>(
    {},
  );
  const detailsFetchKeyRef = useRef<string | null>(null);
  const detailsInFlightRef = useRef<string | null>(null);
  const [detailsLoading, setDetailsLoading] = useState(false);
  const [detailsTab, setDetailsTab] = useState<"media" | "files" | "links">("media");
  const [threadMenu, setThreadMenu] = useState<{ thread: ThreadDto; x: number; y: number } | null>(
    null,
  );
  const [myProfile, setMyProfile] = useState<AccountProfile | null>(null);
  const [profileUid, setProfileUid] = useState<string | null>(null);
  const [profileInitial, setProfileInitial] = useState<UserProfileDto | null>(null);
  const [findUserOpen, setFindUserOpen] = useState(false);
  const [findUserInitialPhone, setFindUserInitialPhone] = useState("");
  const [friendRequestsOpen, setFriendRequestsOpen] = useState(false);
  const [pendingFriendRequestsCount, setPendingFriendRequestsCount] = useState(0);
  const [socialNotice, setSocialNotice] = useState<{
    text: string;
    tone: "info" | "success" | "warn" | "error";
  } | null>(null);
  const [leaveGroupTarget, setLeaveGroupTarget] = useState<ThreadDto | null>(null);
  const [leaveGroupSilent, setLeaveGroupSilent] = useState(false);
  const [leaveGroupBusy, setLeaveGroupBusy] = useState(false);
  const [leaveGroupError, setLeaveGroupError] = useState<string | null>(null);
  const [kickTarget, setKickTarget] = useState<{
    groupId: string;
    member: ThreadMemberDto;
  } | null>(null);
  const [kickBusy, setKickBusy] = useState(false);
  const [kickError, setKickError] = useState<string | null>(null);
  const [addMembersOpen, setAddMembersOpen] = useState(false);
  const [createReminderOpen, setCreateReminderOpen] = useState(false);
  const [createPollOpen, setCreatePollOpen] = useState(false);
  const [capturingScreen, setCapturingScreen] = useState(false);
  const [hideWindowOnCapture, setHideWindowOnCapture] = useState(
    () => localStorage.getItem("teza.screenshotHideWindow") !== "0",
  );
  const [screenshotEditor, setScreenshotEditor] = useState<{
    sourceUrl: string;
    revokeUrl?: string;
    initialMode: "crop" | "annotate";
    monitors: MonitorInfoDto[];
    activeMonitorIndex: number | null;
    canRecapture: boolean;
  } | null>(null);
  const [forwardMessages, setForwardMessages] = useState<MessageDto[]>([]);
  const [selectMode, setSelectMode] = useState(false);
  const [selectedMsgIds, setSelectedMsgIds] = useState<Set<string>>(() => new Set());
  const [pinnedMessages, setPinnedMessages] = useState<MessageDto[]>([]);
  const [pinnedIdx, setPinnedIdx] = useState(0);
  const [pinnedListOpen, setPinnedListOpen] = useState(false);
  const [detailsRefreshTick, setDetailsRefreshTick] = useState(0);
  const [windowWidth, setWindowWidth] = useState<number>(() =>
    typeof window !== "undefined" ? window.innerWidth : 1280,
  );
  const [manualSidebarCompact, setManualSidebarCompact] = useState<boolean>(
    () => localStorage.getItem("litechat.sidebarCompact") === "1",
  );
  const [mobilePane, setMobilePane] = useState<"list" | "chat">("list");
  const [threadRenderLimit, setThreadRenderLimit] = useState(100);
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
  const [fontSize, setFontSize] = useState<FontSizeScale>(() => {
    const raw = localStorage.getItem("litechat.fontSize");
    return raw === "sm" || raw === "lg" ? raw : "md";
  });
  const [density, setDensity] = useState<DensityMode>(() =>
    localStorage.getItem("litechat.density") === "compact" ? "compact" : "comfortable",
  );
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<SettingsTab>("notifications");
  const [navDrawerOpen, setNavDrawerOpen] = useState(false);
  const [aiConfig, setAiConfig] = useState<AiConfigDto | null>(null);
  const [aiChatOpen, setAiChatOpen] = useState(false);
  const [aiMessages, setAiMessages] = useState<AiStoredMessageDto[]>([]);
  const [aiStreamingText, setAiStreamingText] = useState("");
  const [aiIsStreaming, setAiIsStreaming] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const [aiSummaryByThread, setAiSummaryByThread] = useState<
    Record<
      string,
      {
        loading: boolean;
        streamingText: string;
        result: AiSummaryDto | null;
        error: string | null;
      }
    >
  >({});
  const [schedules, setSchedules] = useState<ScheduledMessageDto[]>([]);
  const [schedulesLoading, setSchedulesLoading] = useState(false);
  const [scheduledModalOpen, setScheduledModalOpen] = useState(false);
  const [scheduledInitialThreadId, setScheduledInitialThreadId] = useState<string | null>(null);
  const aiIsStreamingRef = useRef(false);
  aiIsStreamingRef.current = aiIsStreaming;
  const [notifyPrefs, setNotifyPrefs] = useState<NotificationPrefs>(() => loadNotifyPrefs());
  const [sendOnEnter, setSendOnEnter] = useState<boolean>(
    () => localStorage.getItem("teza.sendOnEnter") !== "0",
  );
  const [webNotifyToast, setWebNotifyToast] = useState<NotifyToastPayload | null>(null);
  const [webToastHovered, setWebToastHovered] = useState(false);
  const [drawerMuteOpen, setDrawerMuteOpen] = useState(false);
  const [hiddenBuiltInFolders, setHiddenBuiltInFolders] = useState<BuiltInRailFolderId[]>(() => {
    try {
      const raw = localStorage.getItem("litechat.hiddenBuiltInFolders");
      if (!raw) return [];
      const parsed = JSON.parse(raw) as unknown;
      if (!Array.isArray(parsed)) return [];
      return parsed.filter(
        (x): x is BuiltInRailFolderId =>
          x === "unread" || x === "direct" || x === "group" || x === "cloud",
      );
    } catch {
      return [];
    }
  });
  const [passcodeCfg, setPasscodeCfg] = useState<PasscodeConfig>(() => loadPasscodeConfig());
  const [isLocked, setIsLocked] = useState<boolean>(() => loadPasscodeConfig().enabled);
  const [threadMenuFoldersExpanded, setThreadMenuFoldersExpanded] = useState(false);
  const [railDisplayMode, setRailDisplayMode] = useState<RailDisplayMode>(() => {
    const raw = localStorage.getItem("teza.railDisplayMode");
    return raw === "icon-only" || raw === "text-only" ? raw : "icon-text";
  });
  const [webAppsCfg, setWebAppsCfg] = useState<WebAppsConfig>(() => loadWebAppsConfig());
  const [activeWebAppId, setActiveWebAppId] = useState<string | null>(null);
  const [lastOpenedWebAppId, setLastOpenedWebAppId] = useState<string | null>(null);
  const [webAppHubOpen, setWebAppHubOpen] = useState<boolean>(false);
  const [closeToTray, setCloseToTray] = useState<boolean>(
    () => localStorage.getItem("litechat.closeToTray") !== "0",
  );
  const [inChatSearchOpen, setInChatSearchOpen] = useState(false);
  const [inChatSearchQuery, setInChatSearchQuery] = useState("");
  const [inChatMatchIdx, setInChatMatchIdx] = useState(0);
  const [unreadDividerCount, setUnreadDividerCount] = useState(0);
  const [unreadWatermarkTs, setUnreadWatermarkTs] = useState(0);
  const [draftsByThread, setDraftsByThread] = useState<Record<string, string>>(() => {
    try {
      const raw = localStorage.getItem("litechat.drafts");
      return raw ? (JSON.parse(raw) as Record<string, string>) : {};
    } catch {
      return {};
    }
  });
  const [buzzShaking, setBuzzShaking] = useState(false);
  const [buzzToast, setBuzzToast] = useState<{
    threadId: string;
    threadTitle: string;
    senderName: string;
    streak: number;
    nonce: number;
  } | null>(null);
  const [buzzToastHovered, setBuzzToastHovered] = useState(false);
  const [buzzCooldownUntil, setBuzzCooldownUntil] = useState<Record<string, number>>({});
  const [buzzNow, setBuzzNow] = useState(() => Date.now());
  const [appVersion, setAppVersion] = useState("0.6.1");
  const [updateModalOpen, setUpdateModalOpen] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<AppUpdateInfo | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [lastUpdateCheckAt, setLastUpdateCheckAt] = useState<number | null>(null);
  const [relayCfg, setRelayCfg] = useState<RelayConfigSummary | null>(null);
  const [focusRelaySettings, setFocusRelaySettings] = useState(false);
  const [callState, setCallState] = useState<CallStateDto>(DEFAULT_CALL_STATE);
  const [callTelemetry, setCallTelemetry] = useState<CallTelemetryDto | null>(null);
  const [callDevices, setCallDevices] = useState<CallDevicesDto | null>(null);
  const [callDevicesLoading, setCallDevicesLoading] = useState(false);
  const [callActionBusy, setCallActionBusy] = useState(false);
  const [callHangupBusy, setCallHangupBusy] = useState(false);
  const [activeCallDevicesOpen, setActiveCallDevicesOpen] = useState(false);
  const [callNotice, setCallNotice] = useState<{
    text: string;
    tone: "info" | "warn" | "error";
  } | null>(null);
  const [videoOverlayMode, setVideoOverlayMode] = useState<"expanded" | "floating" | "minimized">(
    "expanded",
  );
  const [localVideoStream, setLocalVideoStream] = useState<MediaStream | null>(null);
  const [localCameraStatus, setLocalCameraStatus] = useState<LocalCameraStatus>("idle");
  const [localCameraError, setLocalCameraError] = useState<string | null>(null);
  const [cameraDevices, setCameraDevices] = useState<CameraDeviceInfo[]>([]);
  const [selectedCameraId, setSelectedCameraId] = useState<string | null>(
    () => localStorage.getItem("teza.callCameraId") || null,
  );
  const [hasRemoteVideoFrame, setHasRemoteVideoFrame] = useState(false);
  const [remoteVideoDims, setRemoteVideoDims] = useState<{
    width: number;
    height: number;
  } | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState<number>(() => {
    const raw = Number(localStorage.getItem("litechat.sidebarWidth"));
    return Number.isFinite(raw) && raw >= 240 && raw <= 440 ? raw : 308;
  });

  const selectedIdRef = useRef<string | null>(null);
  const messagesRef = useRef<MessageDto[]>(messages);
  const pollRefreshTimersRef = useRef<Map<number, number>>(new Map());
  const folderRef = useRef<string>(folder);
  const filterTextRef = useRef<string>(filterText);
  const threadsRef = useRef<ThreadDto[]>(threads);
  const visibleThreadsRef = useRef<ThreadDto[]>([]);
  const sidebarListRef = useRef<HTMLDivElement | null>(null);
  const pendingSidebarScrollIdRef = useRef<string | null>(null);
  const reactionSeqCounterRef = useRef(0);
  const reactionSeqByMsgRef = useRef<Map<string, number>>(new Map());
  const notifyPrefsRef = useRef<NotificationPrefs>(notifyPrefs);
  const themeRef = useRef<ThemeMode>(theme);
  const tRef = useRef(t);
  const myProfileRef = useRef<AccountProfile | null>(myProfile);
  const webToastTimerRef = useRef<number | null>(null);
  const downloadSeqRef = useRef(0);
  const handledDoneDownloadIdsRef = useRef<Set<string>>(new Set());
  const toastErrorTimerRef = useRef<number | null>(null);
  const callNoticeTimerRef = useRef<number | null>(null);
  const socialNoticeTimerRef = useRef<number | null>(null);
  const pendingFriendRequestsCountRef = useRef<number>(pendingFriendRequestsCount);
  const silentSyncInFlightRef = useRef<boolean>(false);
  const recentlyLeftGroupIdsRef = useRef<Set<string>>(new Set());
  const recentlyLeftTimersRef = useRef<Map<string, number>>(new Map());
  const activeCallRef = useRef<CallSnapshotDto | null>(null);
  const remoteVideoRendererRef = useRef<RemoteH264Renderer | null>(null);
  const remoteCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const localCameraEncoderRef = useRef<LocalCameraEncoder | null>(null);
  const selectedCameraIdRef = useRef<string | null>(selectedCameraId);
  const cameraStartSeqRef = useRef(0);
  const encoderRunningCallIdRef = useRef<number | null>(null);
  const encoderRunningDeviceIdRef = useRef<string | null>(null);
  const lastVideoCallIdRef = useRef<number | null>(null);
  const lastPliRequestAtRef = useRef<number>(0);
  const inFlightVideoPushesRef = useRef(0);
  const groupMembersCacheRef = useRef<Record<string, ThreadMemberDto[]>>(groupMembersCache);
  const pendingAttachmentRef = useRef<PendingAttachment | null>(pendingAttachment);
  const hasActiveWebAppRef = useRef<boolean>(false);
  const webAppHubOpenRef = useRef<boolean>(false);
  const chatPaneVisibleRef = useRef<boolean>(true);
  const hasBlockingOverlayRef = useRef<boolean>(false);
  const buzzStreaksRef = useRef<BuzzStreakMap>(new Map());
  const buzzShakeTimerRef = useRef<number | null>(null);
  const buzzToastTimerRef = useRef<number | null>(null);
  const jumpHighlightTimerRef = useRef<number | null>(null);
  const jumpRetryTimerRef = useRef<number | null>(null);
  const sidebarDragCleanupRef = useRef<(() => void) | null>(null);
  const activeThreadRootIdRef = useRef<string | null>(activeThreadRootId);
  const inChatSearchInputRef = useRef<HTMLInputElement | null>(null);
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const messagesContentRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef<boolean>(false);
  const initialScrollReadyRef = useRef<boolean>(true);
  const lastProgrammaticScrollAtRef = useRef<number>(0);
  const pendingJumpRef = useRef<{ threadId: string; msgId: string; ts: number } | null>(null);
  const bootedRef = useRef(false);
  const translationsRef = useRef(translationsByMsg);
  const translateCfgRef = useRef<TranslateConfigSummary | null>(translateCfg);
  const translationCacheRef = useRef<Map<string, MessageTranslationState>>(new Map());
  const inFlightTranslateRef = useRef<Set<string>>(new Set());
  const prevConnStatusRef = useRef<ConnectionStatus>(connStatus);
  const lastOlderCursorRef = useRef<string | null>(null);
  const pinnedMsgIdsRef = useRef<Set<string>>(new Set());
  const pendingMarkReadRef = useRef<string | null>(null);
  const isLockedRef = useRef<boolean>(isLocked);
  const passcodeEnabledRef = useRef<boolean>(passcodeCfg.enabled);
  const customFoldersRef = useRef<ChatFolderDto[]>(customFolders);
  const hiddenBuiltInFoldersRef = useRef<BuiltInRailFolderId[]>(hiddenBuiltInFolders);
  const lastActivityAtRef = useRef<number>(Date.now());
  const authedRef = useRef<boolean>(authed);
  const capturingScreenRef = useRef<boolean>(capturingScreen);
  const screenshotEditorRef = useRef(screenshotEditor);
  const handleCaptureScreenRef = useRef<() => void>(() => {});
  const progressiveEscStateRef = useRef({
    hasOverlay: false,
    hasMenu: false,
    inChatSearch: false,
    hasThreadPanel: false,
    hasReply: false,
  });

  selectedIdRef.current = selectedId;
  messagesRef.current = messages;
  folderRef.current = folder;
  filterTextRef.current = filterText;
  threadsRef.current = threads;
  notifyPrefsRef.current = notifyPrefs;
  themeRef.current = theme;
  tRef.current = t;
  myProfileRef.current = myProfile;
  pendingFriendRequestsCountRef.current = pendingFriendRequestsCount;
  activeThreadRootIdRef.current = activeThreadRootId;
  translationsRef.current = translationsByMsg;
  translateCfgRef.current = translateCfg;
  pinnedMsgIdsRef.current = new Set(pinnedMessages.map((p) => p.msg_id));
  isLockedRef.current = isLocked;
  passcodeEnabledRef.current = passcodeCfg.enabled;
  customFoldersRef.current = customFolders;
  hiddenBuiltInFoldersRef.current = hiddenBuiltInFolders;
  authedRef.current = authed;
  capturingScreenRef.current = capturingScreen;
  screenshotEditorRef.current = screenshotEditor;
  activeCallRef.current = callState.active;
  selectedCameraIdRef.current = selectedCameraId;
  groupMembersCacheRef.current = groupMembersCache;
  pendingAttachmentRef.current = pendingAttachment;
  webAppHubOpenRef.current = Boolean(webAppsCfg.enabled && webAppHubOpen);
  const showVideoOverlay = Boolean(
    authed &&
      !isLocked &&
      shouldShowVideoOverlay(callState.active, videoOverlayMode === "minimized"),
  );
  const hasBlockingOverlay = Boolean(
    isLocked ||
      settingsOpen ||
      navDrawerOpen ||
      folderModalOpen ||
      quickSearchOpen ||
      findUserOpen ||
      friendRequestsOpen ||
      Boolean(leaveGroupTarget) ||
      addMembersOpen ||
      createReminderOpen ||
      createPollOpen ||
      scheduledModalOpen ||
      screenshotEditor ||
      profileUid ||
      lightboxState ||
      forwardMessages.length > 0 ||
      updateModalOpen ||
      translateModalOpen ||
      (showVideoOverlay && videoOverlayMode === "expanded") ||
      activeCallDevicesOpen ||
      callState.phase === "incoming_ringing",
  );
  hasBlockingOverlayRef.current = hasBlockingOverlay;
  progressiveEscStateRef.current = {
    hasOverlay: Boolean(hasBlockingOverlay || selectMode),
    hasMenu: Boolean(threadMenu || folderMenu || drawerMuteOpen),
    inChatSearch: inChatSearchOpen,
    hasThreadPanel: Boolean(activeThreadRootId),
    hasReply: Boolean(replyTo),
  };

  const updatePendingAttachment = useCallback((next: PendingAttachment | null) => {
    const prev = pendingAttachmentRef.current;
    const prevUrl = prev?.kind === "raw" ? prev.previewUrl : null;
    const nextUrl = next?.kind === "raw" ? next.previewUrl : null;
    if (prevUrl && prevUrl !== nextUrl) {
      URL.revokeObjectURL(prevUrl);
    }
    pendingAttachmentRef.current = next;
    setPendingAttachment(next);
  }, []);

  const leaveWebApps = useCallback(() => {
    setActiveWebAppId(null);
    setWebAppHubOpen(false);
    void bridge.webappHideAll();
  }, []);

  const layout = useMemo(
    () => computeLayoutMode(windowWidth, manualSidebarCompact),
    [windowWidth, manualSidebarCompact],
  );
  const chatPaneVisible = !(layout.mode === "single" && mobilePane === "list");
  chatPaneVisibleRef.current = chatPaneVisible;
  const prevAutoCompactDrawerRef = useRef(layout.autoCompactDrawer);

  useEffect(() => {
    const onResize = () => setWindowWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    if (!prevAutoCompactDrawerRef.current && layout.autoCompactDrawer) {
      setDrawerOpen(false);
    }
    prevAutoCompactDrawerRef.current = layout.autoCompactDrawer;
  }, [layout.autoCompactDrawer]);

  const refreshPinnedMessages = useCallback(async (tid: string) => {
    try {
      const list = await bridge.listPinnedMessages(tid);
      if (selectedIdRef.current === tid) {
        setPinnedMessages(list);
      }
    } catch {
      // Ignore transient pin sync failures
    }
  }, []);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("litechat.theme", theme);
  }, [theme]);

  useEffect(() => {
    document.documentElement.setAttribute("data-font-size", fontSize);
    localStorage.setItem("litechat.fontSize", fontSize);
  }, [fontSize]);

  useEffect(() => {
    document.documentElement.setAttribute("data-density", density);
    localStorage.setItem("litechat.density", density);
  }, [density]);

  useEffect(() => {
    localStorage.setItem("litechat.fx", fxEnabled ? "1" : "0");
  }, [fxEnabled]);

  useEffect(() => {
    saveNotifyPrefs(notifyPrefs);
  }, [notifyPrefs]);

  useEffect(() => {
    localStorage.setItem("teza.sendOnEnter", sendOnEnter ? "1" : "0");
  }, [sendOnEnter]);

  useEffect(() => {
    localStorage.setItem("litechat.closeToTray", closeToTray ? "1" : "0");
    void bridge.setCloseToTray(closeToTray);
  }, [closeToTray]);

  useEffect(() => {
    const totalUnread = computeTotalUnreadBadge(threads, notifyPrefs);
    const title = totalUnread > 0 ? `(${totalUnread}) Teza` : "Teza";
    document.title = title;
    if (isTauri()) {
      void getCurrentWindow()
        .setTitle(title)
        .catch(() => undefined);
    }
  }, [threads, notifyPrefs]);

  const setDraftForThread = useCallback((tid: string, val: string) => {
    setDraftsByThread((prev) => {
      const next = { ...prev };
      if (!val) {
        delete next[tid];
      } else {
        next[tid] = val;
      }
      try {
        const entries = Object.entries(next).slice(-80);
        localStorage.setItem("litechat.drafts", JSON.stringify(Object.fromEntries(entries)));
      } catch {
        // Ignore storage quota errors
      }
      return next;
    });
  }, []);

  const applyRelaySummary = useCallback((cfg: RelayConfigSummary) => {
    setRelayCfg(cfg);
    setRelayMediaRouting(Boolean(cfg.enabled && cfg.route_webview_media));
  }, []);

  useEffect(() => {
    void bridge
      .getRelayConfig()
      .then(applyRelaySummary)
      .catch(() => undefined);
  }, [applyRelaySummary]);

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
      const update = await bridge.checkAppUpdate();
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
      if (isLockedRef.current || isImeComposingEvent(e)) return;
      const keyLower = e.key.toLowerCase();
      const isCodeOrKey = (code: string, key: string) => e.code === code || keyLower === key;
      if ((e.ctrlKey || e.metaKey) && isCodeOrKey("KeyL", "l")) {
        if (passcodeEnabledRef.current) {
          e.preventDefault();
          setIsLocked(true);
        }
        return;
      }
      if (
        (e.ctrlKey || e.metaKey) &&
        e.shiftKey &&
        !e.altKey &&
        isCodeOrKey("KeyS", "s")
      ) {
        if (
          authedRef.current &&
          selectedIdRef.current &&
          !hasActiveWebAppRef.current &&
          !webAppHubOpenRef.current &&
          !capturingScreenRef.current &&
          !screenshotEditorRef.current
        ) {
          e.preventDefault();
          handleCaptureScreenRef.current();
        }
        return;
      }
      if ((e.ctrlKey || e.metaKey) && isCodeOrKey("KeyK", "k")) {
        e.preventDefault();
        setQuickSearchOpen((v) => !v);
      } else if ((e.ctrlKey || e.metaKey) && isCodeOrKey("KeyF", "f")) {
        if (
          selectedIdRef.current &&
          !activeThreadRootIdRef.current &&
          !hasActiveWebAppRef.current &&
          !webAppHubOpenRef.current
        ) {
          e.preventDefault();
          setInChatSearchOpen(true);
          setTimeout(() => inChatSearchInputRef.current?.focus(), 20);
        }
      } else if (
        (e.ctrlKey || e.metaKey) &&
        !e.shiftKey &&
        !e.altKey &&
        e.key >= "1" &&
        e.key <= "9"
      ) {
        const railIds: string[] = [
          ...FOLDERS.filter(
            (item) => item.id === "all" || !hiddenBuiltInFoldersRef.current.includes(item.id),
          ).map((item) => item.id),
          ...customFoldersRef.current.map((cf) => `custom:${cf.id}`),
        ];
        const targetFolder = railIds[Number(e.key) - 1];
        if (targetFolder) {
          e.preventDefault();
          leaveWebApps();
          setFolder(targetFolder);
        }
      } else if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
        const list = visibleThreadsRef.current;
        if (list.length === 0) return;
        e.preventDefault();
        const curIdx = list.findIndex((t) => t.id === selectedIdRef.current);
        const delta = e.key === "ArrowDown" ? 1 : -1;
        const nextIdx =
          curIdx < 0 ? 0 : (curIdx + delta + list.length) % list.length;
        setSelectedId(list[nextIdx].id);
      } else if (e.key === "Escape") {
        if (e.defaultPrevented) return;
        const esc = progressiveEscStateRef.current;
        if (esc.hasOverlay) {
          return;
        }
        if (document.querySelector("[data-teza-floating-popup]")) {
          return;
        }
        if (esc.hasMenu) {
          e.preventDefault();
          setThreadMenu(null);
          setFolderMenu(null);
          setDrawerMuteOpen(false);
        } else if (esc.inChatSearch) {
          e.preventDefault();
          setInChatSearchOpen(false);
          setInChatSearchQuery("");
        } else if (esc.hasThreadPanel) {
          e.preventDefault();
          setActiveThreadRootId(null);
        } else if (esc.hasReply) {
          e.preventDefault();
          setReplyTo(null);
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [leaveWebApps]);

  const isInActiveCall = callState.phase !== "idle";
  useEffect(() => {
    if (
      !authed ||
      !passcodeCfg.enabled ||
      passcodeCfg.autoLockMinutes <= 0 ||
      isLocked ||
      isInActiveCall
    ) {
      return;
    }
    lastActivityAtRef.current = Date.now();
    const markActivity = () => {
      lastActivityAtRef.current = Date.now();
    };
    const events: (keyof WindowEventMap)[] = [
      "mousemove",
      "mousedown",
      "keydown",
      "wheel",
      "touchstart",
    ];
    for (const evName of events) {
      window.addEventListener(evName, markActivity, { passive: true });
    }
    const thresholdMs = passcodeCfg.autoLockMinutes * 60_000;
    const timer = window.setInterval(() => {
      if (Date.now() - lastActivityAtRef.current >= thresholdMs) {
        setIsLocked(true);
      }
    }, 15_000);
    return () => {
      for (const evName of events) {
        window.removeEventListener(evName, markActivity);
      }
      window.clearInterval(timer);
    };
  }, [authed, passcodeCfg.enabled, passcodeCfg.autoLockMinutes, isLocked, isInActiveCall]);

  useEffect(() => {
    if (!selectMode || forwardMessages.length > 0) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented && !hasBlockingOverlayRef.current) {
        e.preventDefault();
        setSelectMode(false);
        setSelectedMsgIds(new Set());
      }
    };
    window.addEventListener("keydown", onEsc);
    return () => window.removeEventListener("keydown", onEsc);
  }, [selectMode, forwardMessages.length]);

  useEffect(() => {
    setThreadRenderLimit(
      computeThreadRenderLimitForTarget(
        visibleThreadsRef.current,
        pendingSidebarScrollIdRef.current ?? selectedIdRef.current,
        100,
      ),
    );
  }, [folder, filterText]);

  const dismissToastError = useCallback(() => {
    if (toastErrorTimerRef.current !== null) {
      window.clearTimeout(toastErrorTimerRef.current);
      toastErrorTimerRef.current = null;
    }
    setToastError(null);
  }, []);

  const showError = useCallback((err: unknown) => {
    setToastError(String(err));
    if (toastErrorTimerRef.current !== null) {
      window.clearTimeout(toastErrorTimerRef.current);
    }
    toastErrorTimerRef.current = window.setTimeout(() => {
      toastErrorTimerRef.current = null;
      setToastError(null);
    }, 5000);
  }, []);

  const dismissCallNotice = useCallback(() => {
    if (callNoticeTimerRef.current !== null) {
      window.clearTimeout(callNoticeTimerRef.current);
      callNoticeTimerRef.current = null;
    }
    setCallNotice(null);
  }, []);

  const showCallNotice = useCallback((text: string, tone: "info" | "warn" | "error" = "info") => {
    setCallNotice({ text, tone });
    if (callNoticeTimerRef.current !== null) {
      window.clearTimeout(callNoticeTimerRef.current);
    }
    callNoticeTimerRef.current = window.setTimeout(() => {
      callNoticeTimerRef.current = null;
      setCallNotice(null);
    }, 4500);
  }, []);

  const dismissSocialNotice = useCallback(() => {
    if (socialNoticeTimerRef.current !== null) {
      window.clearTimeout(socialNoticeTimerRef.current);
      socialNoticeTimerRef.current = null;
    }
    setSocialNotice(null);
  }, []);

  const showSocialNotice = useCallback(
    (text: string, tone: "info" | "success" | "warn" | "error" = "info") => {
      setSocialNotice({ text, tone });
      if (socialNoticeTimerRef.current !== null) {
        window.clearTimeout(socialNoticeTimerRef.current);
      }
      socialNoticeTimerRef.current = window.setTimeout(() => {
        socialNoticeTimerRef.current = null;
        setSocialNotice(null);
      }, 4500);
    },
    [],
  );

  const dismissBuzzToast = useCallback(() => {
    if (buzzToastTimerRef.current !== null) {
      window.clearTimeout(buzzToastTimerRef.current);
      buzzToastTimerRef.current = null;
    }
    setBuzzToastHovered(false);
    setBuzzToast(null);
  }, []);

  const dismissWebNotifyToast = useCallback(() => {
    if (webToastTimerRef.current !== null) {
      window.clearTimeout(webToastTimerRef.current);
      webToastTimerRef.current = null;
    }
    setWebToastHovered(false);
    setWebNotifyToast(null);
  }, []);

  const refreshCallDevices = useCallback(() => {
    setCallDevicesLoading(true);
    void bridge
      .callListDevices()
      .then(setCallDevices)
      .catch(() => undefined)
      .finally(() => setCallDevicesLoading(false));
  }, []);

  const refreshCameraDevices = useCallback(() => {
    void listBrowserCameras()
      .then(setCameraDevices)
      .catch(() => undefined);
  }, []);

  const handleSelectCameraDevice = useCallback((deviceId: string | null) => {
    const clean = deviceId?.trim() || null;
    setSelectedCameraId(clean);
    if (clean) {
      localStorage.setItem("teza.callCameraId", clean);
    } else {
      localStorage.removeItem("teza.callCameraId");
    }
  }, []);

  const ensureRemoteVideoRenderer = useCallback(() => {
    if (!remoteVideoRendererRef.current) {
      remoteVideoRendererRef.current = new RemoteH264Renderer({
        onFirstFrame: () => {
          setHasRemoteVideoFrame(true);
        },
        onDimensions: (width, height) => {
          setRemoteVideoDims((prev) =>
            prev && prev.width === width && prev.height === height ? prev : { width, height },
          );
        },
        onKeyframeNeeded: () => {
          const cur = activeCallRef.current;
          if (
            !cur ||
            cur.call_type !== "DirectVideo" ||
            cur.phase !== "connected" ||
            !cur.peer_cam_enabled
          ) {
            return;
          }
          const now = Date.now();
          if (now - lastPliRequestAtRef.current < 800) {
            return;
          }
          lastPliRequestAtRef.current = now;
          void bridge.callRequestVideoKeyframe(cur.call_id).catch(() => undefined);
        },
        onError: (err) => {
          console.debug("[video-ui] remote renderer:", err);
        },
      });
      if (remoteCanvasRef.current) {
        remoteVideoRendererRef.current.attachCanvas(remoteCanvasRef.current);
      }
    }
    return remoteVideoRendererRef.current;
  }, []);

  const handleRemoteCanvasChange = useCallback(
    (canvas: HTMLCanvasElement | null) => {
      remoteCanvasRef.current = canvas;
      if (canvas) {
        ensureRemoteVideoRenderer().attachCanvas(canvas);
      } else {
        remoteVideoRendererRef.current?.attachCanvas(null);
      }
    },
    [ensureRemoteVideoRenderer],
  );

  const stopLocalCameraEncoder = useCallback((destroy = false) => {
    cameraStartSeqRef.current += 1;
    encoderRunningCallIdRef.current = null;
    encoderRunningDeviceIdRef.current = null;
    const enc = localCameraEncoderRef.current;
    if (enc) {
      if (destroy) {
        enc.destroy();
        localCameraEncoderRef.current = null;
      } else {
        enc.stop();
      }
    }
    setLocalVideoStream(null);
  }, []);

  const startLocalCameraForCall = useCallback(
    async (callId: number, deviceId: string | null) => {
      const seq = ++cameraStartSeqRef.current;
      if (localCameraEncoderRef.current) {
        localCameraEncoderRef.current.destroy();
        localCameraEncoderRef.current = null;
      }
      encoderRunningCallIdRef.current = callId;
      encoderRunningDeviceIdRef.current = deviceId;
      setLocalCameraError(null);
      setLocalCameraStatus("starting");

      const encoder = new LocalCameraEncoder({
        onEncodedAu: (dataB64, isKeyFrame) => {
          const cur = activeCallRef.current;
          if (
            !cur ||
            cur.call_type !== "DirectVideo" ||
            cur.phase !== "connected" ||
            cur.call_id !== callId ||
            !cur.cam_enabled
          ) {
            return;
          }
          if (
            (!isKeyFrame && inFlightVideoPushesRef.current >= 2) ||
            inFlightVideoPushesRef.current >= 4
          ) {
            localCameraEncoderRef.current?.requestKeyframe();
            return;
          }
          inFlightVideoPushesRef.current += 1;
          void bridge
            .callPushVideoFrame(callId, dataB64)
            .catch(() => undefined)
            .finally(() => {
              inFlightVideoPushesRef.current = Math.max(0, inFlightVideoPushesRef.current - 1);
            });
        },
        onStatusChange: (status, detail) => {
          if (cameraStartSeqRef.current !== seq) return;
          setLocalCameraStatus(status);
          setLocalCameraError(detail ?? null);
          if (startResolved && (status === "error" || status === "unavailable")) {
            encoder.destroy();
            if (localCameraEncoderRef.current === encoder) {
              localCameraEncoderRef.current = null;
            }
            setLocalVideoStream(null);
            showCallNotice(
              tRef.current(
                status === "error"
                  ? "callVideoCameraErrorNotice"
                  : "callVideoCameraUnavailableNotice",
              ),
              "warn",
            );
            const cur = activeCallRef.current;
            if (
              cur &&
              cur.call_type === "DirectVideo" &&
              cur.call_id === callId &&
              cur.cam_enabled
            ) {
              void bridge
                .callSetCamera(false)
                .then(setCallState)
                .catch(() => undefined)
                .finally(() => {
                  if (
                    cameraStartSeqRef.current === seq &&
                    encoderRunningCallIdRef.current === callId
                  ) {
                    encoderRunningCallIdRef.current = null;
                    encoderRunningDeviceIdRef.current = null;
                  }
                });
            } else if (cameraStartSeqRef.current === seq) {
              encoderRunningCallIdRef.current = null;
              encoderRunningDeviceIdRef.current = null;
            }
          }
        },
        onStreamChange: (stream) => {
          if (cameraStartSeqRef.current !== seq) {
            if (stream) {
              stream.getTracks().forEach((tr) => tr.stop());
            }
            return;
          }
          setLocalVideoStream(stream);
        },
      });
      localCameraEncoderRef.current = encoder;

      let startResolved = false;
      let status = await encoder.start(deviceId);
      if (cameraStartSeqRef.current !== seq) {
        encoder.destroy();
        if (localCameraEncoderRef.current === encoder) {
          localCameraEncoderRef.current = null;
        }
        return status;
      }

      if (deviceId && (status === "error" || status === "unavailable")) {
        // Saved camera device may have been unplugged; fall back to default camera once
        localStorage.removeItem("teza.callCameraId");
        setSelectedCameraId(null);
        encoderRunningDeviceIdRef.current = null;
        status = await encoder.start(null);
        if (cameraStartSeqRef.current !== seq) {
          encoder.destroy();
          if (localCameraEncoderRef.current === encoder) {
            localCameraEncoderRef.current = null;
          }
          return status;
        }
      }
      startResolved = true;

      void listBrowserCameras()
        .then(setCameraDevices)
        .catch(() => undefined);

      if (isCameraFailureStatus(status)) {
        encoder.destroy();
        if (localCameraEncoderRef.current === encoder) {
          localCameraEncoderRef.current = null;
        }
        setLocalVideoStream(null);
        setLocalCameraStatus(status);

        const noticeKey: TranslationKey =
          status === "denied"
            ? "callVideoCameraDeniedNotice"
            : status === "unsupported"
              ? "callVideoCameraUnsupportedNotice"
              : status === "error"
                ? "callVideoCameraErrorNotice"
                : "callVideoCameraUnavailableNotice";
        showCallNotice(tRef.current(noticeKey), "warn");

        const cur = activeCallRef.current;
        if (
          cur &&
          cur.call_type === "DirectVideo" &&
          cur.call_id === callId &&
          cur.cam_enabled
        ) {
          void bridge
            .callSetCamera(false)
            .then(setCallState)
            .catch(() => undefined)
            .finally(() => {
              if (
                cameraStartSeqRef.current === seq &&
                encoderRunningCallIdRef.current === callId
              ) {
                encoderRunningCallIdRef.current = null;
                encoderRunningDeviceIdRef.current = null;
              }
            });
        } else if (cameraStartSeqRef.current === seq) {
          encoderRunningCallIdRef.current = null;
          encoderRunningDeviceIdRef.current = null;
        }
      }
      return status;
    },
    [showCallNotice],
  );

  const handleSelectCallDevices = useCallback(
    async (input: string | null, output: string | null): Promise<boolean> => {
      setCallDevicesLoading(true);
      try {
        const next = await bridge.callSetDevices(input, output);
        setCallDevices(next);
        return true;
      } catch (err) {
        showError(err);
        return false;
      } finally {
        setCallDevicesLoading(false);
      }
    },
    [showError],
  );

  const handleStartVoiceCall = useCallback(
    async (partnerId: string, partnerName?: string) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      setCallNotice(null);
      try {
        const next = await bridge.callStartVoice(partnerId, partnerName);
        setCallState(next);
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError],
  );

  const handleStartVideoCall = useCallback(
    async (partnerId: string, partnerName?: string) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      setCallNotice(null);
      setVideoOverlayMode("expanded");
      try {
        const next = await bridge.callStartVideo(partnerId, partnerName);
        setCallState(next);
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError],
  );

  const handleAnswerCall = useCallback(
    async (callId: number) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      setVideoOverlayMode("expanded");
      try {
        const next = await bridge.callAnswer(callId);
        setCallState(next);
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError],
  );

  const handleDeclineCall = useCallback(
    async (callId: number) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      try {
        const next = await bridge.callDecline(callId);
        setCallState(next);
        if (next.phase === "idle") {
          setCallTelemetry(null);
          stopLocalCameraEncoder(true);
          remoteVideoRendererRef.current?.reset();
        }
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError, stopLocalCameraEncoder],
  );

  const handleHangupCall = useCallback(async () => {
    if (callHangupBusy) return;
    setCallHangupBusy(true);
    stopLocalCameraEncoder(true);
    try {
      const next = await bridge.callHangup();
      setCallState(next);
      if (next.phase === "idle") {
        setCallTelemetry(null);
        remoteVideoRendererRef.current?.reset();
      }
    } catch (err) {
      showError(err);
    } finally {
      setCallHangupBusy(false);
    }
  }, [callHangupBusy, showError, stopLocalCameraEncoder]);

  const handleToggleCallMute = useCallback(
    async (nextMuted: boolean) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      try {
        const next = await bridge.callSetMute(nextMuted);
        setCallState(next);
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError],
  );

  const handleToggleCallCamera = useCallback(
    async (nextEnabled: boolean) => {
      if (callActionBusy) return;
      setCallActionBusy(true);
      try {
        const next = await bridge.callSetCamera(nextEnabled);
        if (!nextEnabled && (!next.active || !next.active.cam_enabled)) {
          stopLocalCameraEncoder(true);
          setLocalCameraStatus("off");
          setLocalCameraError(null);
        }
        setCallState(next);
        if (
          nextEnabled &&
          next.active &&
          shouldCaptureLocalVideo(next.active) &&
          encoderRunningCallIdRef.current !== next.active.call_id
        ) {
          void startLocalCameraForCall(next.active.call_id, selectedCameraIdRef.current);
        }
      } catch (err) {
        showError(err);
      } finally {
        setCallActionBusy(false);
      }
    },
    [callActionBusy, showError, startLocalCameraForCall, stopLocalCameraEncoder],
  );

  const snapScrollToBottom = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    lastProgrammaticScrollAtRef.current = performance.now();
    el.scrollTop = el.scrollHeight;
  }, []);

  useEffect(() => {
    return () => {
      if (toastErrorTimerRef.current !== null) {
        window.clearTimeout(toastErrorTimerRef.current);
        toastErrorTimerRef.current = null;
      }
      if (callNoticeTimerRef.current !== null) {
        window.clearTimeout(callNoticeTimerRef.current);
        callNoticeTimerRef.current = null;
      }
      if (socialNoticeTimerRef.current !== null) {
        window.clearTimeout(socialNoticeTimerRef.current);
        socialNoticeTimerRef.current = null;
      }
      for (const timerId of recentlyLeftTimersRef.current.values()) {
        window.clearTimeout(timerId);
      }
      recentlyLeftTimersRef.current.clear();
      recentlyLeftGroupIdsRef.current.clear();
      for (const timerId of pollRefreshTimersRef.current.values()) {
        window.clearTimeout(timerId);
      }
      pollRefreshTimersRef.current.clear();
      if (webToastTimerRef.current !== null) {
        window.clearTimeout(webToastTimerRef.current);
        webToastTimerRef.current = null;
      }
      if (buzzToastTimerRef.current !== null) {
        window.clearTimeout(buzzToastTimerRef.current);
        buzzToastTimerRef.current = null;
      }
      if (buzzShakeTimerRef.current !== null) {
        window.clearTimeout(buzzShakeTimerRef.current);
        buzzShakeTimerRef.current = null;
      }
      if (jumpHighlightTimerRef.current !== null) {
        window.clearTimeout(jumpHighlightTimerRef.current);
        jumpHighlightTimerRef.current = null;
      }
      if (jumpRetryTimerRef.current !== null) {
        window.clearTimeout(jumpRetryTimerRef.current);
        jumpRetryTimerRef.current = null;
      }
      sidebarDragCleanupRef.current?.();
      sidebarDragCleanupRef.current = null;
      localCameraEncoderRef.current?.destroy();
      localCameraEncoderRef.current = null;
      remoteVideoRendererRef.current?.destroy();
      remoteVideoRendererRef.current = null;
    };
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
    setFriendRequestsOpen(false);
    setPendingFriendRequestsCount(0);
    setSocialNotice(null);
    setLeaveGroupTarget(null);
    setLeaveGroupSilent(false);
    setLeaveGroupBusy(false);
    setLeaveGroupError(null);
    setKickTarget(null);
    setKickBusy(false);
    setKickError(null);
    for (const timerId of recentlyLeftTimersRef.current.values()) {
      window.clearTimeout(timerId);
    }
    recentlyLeftTimersRef.current.clear();
    recentlyLeftGroupIdsRef.current.clear();
    setForwardMessages([]);
    setSelectMode(false);
    setSelectedMsgIds(new Set());
    setPinnedIdx(0);
    setThreadRenderLimit(100);
    setTranslationsByMsg({});
    translationCacheRef.current.clear();
    stickToBottomRef.current = false;
    setThreadAutoTranslate({});
    setThreads([]);
    setCustomFolders([]);
    setFolder("all");
    setMessages([]);
    setSelectedId(null);
    setReplyTo(null);
    setActiveThreadRootId(null);
    updatePendingAttachment(null);
    setThreadDetails(null);
    setGroupMembersCache({});
    detailsFetchKeyRef.current = null;
    detailsInFlightRef.current = null;
    setThreadMenu(null);
    setThreadMenuFoldersExpanded(false);
    setShowScrollDown(false);
    setUnreadBelow(0);
    setActiveDownloads([]);
    setCompletedDownload(null);
    setCallState(DEFAULT_CALL_STATE);
    setCallTelemetry(null);
    setCallActionBusy(false);
    setCallHangupBusy(false);
    setActiveCallDevicesOpen(false);
    setCallNotice(null);
    stopLocalCameraEncoder(true);
    remoteVideoRendererRef.current?.reset();
    lastVideoCallIdRef.current = null;
    lastPliRequestAtRef.current = 0;
    setVideoOverlayMode("expanded");
    for (const timerId of pollRefreshTimersRef.current.values()) {
      window.clearTimeout(timerId);
    }
    pollRefreshTimersRef.current.clear();
    setLocalCameraStatus("idle");
    setLocalCameraError(null);
    setHasRemoteVideoFrame(false);
    setRemoteVideoDims(null);
    setIsLocked(false);
    setLastOpenedWebAppId(null);
    setAiChatOpen(false);
    setAiMessages([]);
    setAiStreamingText("");
    setAiIsStreaming(false);
    setAiError(null);
    setAiSummaryByThread({});
    setSchedules([]);
    setScheduledModalOpen(false);
    setScheduledInitialThreadId(null);
    leaveWebApps();
  }, [stopLocalCameraEncoder, updatePendingAttachment, leaveWebApps]);

  const pendingSchedulesCount = useMemo(
    () => schedules.filter((s) => s.status === "pending" || s.status === "sending").length,
    [schedules],
  );

  const refreshSchedules = useCallback(async () => {
    setSchedulesLoading(true);
    try {
      const list = await bridge.scheduleList("all", 200);
      setSchedules(list);
    } catch {
      // Ignore transient refresh failures
    } finally {
      setSchedulesLoading(false);
    }
  }, []);

  const handleOpenAiChat = useCallback(() => {
    leaveWebApps();
    setActiveThreadRootId(null);
    setDrawerOpen(false);
    setAiChatOpen(true);
    setMobilePane("chat");
  }, [leaveWebApps]);

  const handleAiSend = useCallback(async (rawPrompt: string) => {
    const trimmed = rawPrompt.trim();
    if (!trimmed || aiIsStreamingRef.current) return;
    const tempUserId = `tmp-ai-user-${Date.now()}`;
    const optimisticUser: AiStoredMessageDto = {
      id: tempUserId,
      session_id: "default",
      role: "user",
      content: trimmed,
      model: aiConfig?.gemini_model || "",
      backend: aiConfig?.backend || "gemini_api",
      ts: Date.now(),
    };
    setAiError(null);
    setAiStreamingText("");
    setAiIsStreaming(true);
    setAiMessages((prev) => [...prev, optimisticUser]);
    try {
      const reply = await bridge.aiChatSend(trimmed, "default");
      try {
        const synced = await bridge.aiListMessages("default", 100);
        setAiMessages(synced);
      } catch {
        setAiMessages((prev) => [...prev, reply]);
      }
      setAiStreamingText("");
    } catch (err) {
      setAiError(err instanceof Error ? err.message : String(err));
      setAiStreamingText("");
      void bridge
        .aiListMessages("default", 100)
        .then(setAiMessages)
        .catch(() => {
          setAiMessages((prev) => prev.filter((m) => m.id !== tempUserId));
        });
    } finally {
      setAiIsStreaming(false);
    }
  }, [aiConfig?.backend, aiConfig?.gemini_model]);

  const handleAiClearHistory = useCallback(async () => {
    try {
      await bridge.aiClearMessages("default");
      setAiMessages([]);
      setAiStreamingText("");
      setAiError(null);
    } catch (err) {
      setAiError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const handleSummarizeUnread = useCallback(async (threadId: string) => {
    if (!threadId) return;
    setAiSummaryByThread((prev) => ({
      ...prev,
      [threadId]: {
        loading: true,
        streamingText: "",
        result: null,
        error: null,
      },
    }));
    try {
      const summary = await bridge.aiSummarizeUnread(threadId);
      setAiSummaryByThread((prev) => ({
        ...prev,
        [threadId]: {
          loading: false,
          streamingText: "",
          result: summary,
          error: null,
        },
      }));
    } catch (err) {
      setAiSummaryByThread((prev) => ({
        ...prev,
        [threadId]: {
          loading: false,
          streamingText: "",
          result: null,
          error: err instanceof Error ? err.message : String(err),
        },
      }));
    }
  }, []);

  const handleDismissSummary = useCallback((threadId: string) => {
    setAiSummaryByThread((prev) => {
      if (!prev[threadId]) return prev;
      const next = { ...prev };
      delete next[threadId];
      return next;
    });
  }, []);

  useEffect(() => {
    if (!authed) return;
    void bridge
      .getTranslateConfig()
      .then(setTranslateCfg)
      .catch(() => undefined);
    void bridge
      .aiGetConfig()
      .then(setAiConfig)
      .catch(() => undefined);
    void refreshSchedules();
    void bridge
      .callGetState()
      .then(setCallState)
      .catch(() => undefined);
    refreshCallDevices();
  }, [authed, refreshCallDevices, refreshSchedules]);

  useEffect(() => {
    if (!authed || !aiChatOpen) return;
    let cancelled = false;
    void bridge
      .aiGetConfig()
      .then((cfg) => {
        if (!cancelled) setAiConfig(cfg);
      })
      .catch(() => undefined);
    void bridge
      .aiListMessages("default", 100)
      .then((list) => {
        if (!cancelled) setAiMessages(list);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [authed, aiChatOpen]);

  useEffect(() => {
    if (!authed) return;
    let disposed = false;
    const unsubs: (() => void)[] = [];

    const register = (promise: Promise<() => void>) => {
      void promise
        .then((fn) => {
          if (disposed) {
            fn();
          } else {
            unsubs.push(fn);
          }
        })
        .catch(() => undefined);
    };

    register(
      bridge.onAiStreamDelta((ev) => {
        if (disposed) return;
        const sid = ev.session_id || "default";
        if (sid !== "default" || !aiIsStreamingRef.current) return;
        if (ev.delta) {
          setAiStreamingText((prev) => prev + ev.delta);
        }
      }),
    );

    register(
      bridge.onAiSummaryDelta((ev) => {
        if (disposed || !ev.thread_id || !ev.delta) return;
        setAiSummaryByThread((prev) => {
          const cur = prev[ev.thread_id];
          if (!cur || !cur.loading) return prev;
          return {
            ...prev,
            [ev.thread_id]: {
              ...cur,
              streamingText: cur.streamingText + ev.delta,
            },
          };
        });
      }),
    );

    register(
      bridge.onScheduleUpdated(() => {
        if (!disposed) {
          void refreshSchedules();
        }
      }),
    );

    return () => {
      disposed = true;
      for (const fn of unsubs) {
        try {
          fn();
        } catch {
          // Ignore cleanup errors
        }
      }
    };
  }, [authed, refreshSchedules]);

  useEffect(() => {
    if (!authed || isLocked || callState.phase !== "incoming_ringing") return;
    return startIncomingRingtone(notifyPrefs.sound);
  }, [authed, isLocked, callState.phase, notifyPrefs.sound]);

  const activeVideoCallId =
    callState.active?.call_type === "DirectVideo" && callState.phase !== "idle"
      ? callState.active.call_id
      : null;

  useEffect(() => {
    if (activeVideoCallId === lastVideoCallIdRef.current) return;
    lastVideoCallIdRef.current = activeVideoCallId;
    lastPliRequestAtRef.current = 0;
    remoteVideoRendererRef.current?.reset();
    setHasRemoteVideoFrame(false);
    setRemoteVideoDims(null);
    setVideoOverlayMode("expanded");
    if (activeVideoCallId === null) {
      stopLocalCameraEncoder(true);
      setLocalCameraStatus("idle");
      setLocalCameraError(null);
    } else {
      refreshCameraDevices();
    }
  }, [activeVideoCallId, refreshCameraDevices, stopLocalCameraEncoder]);

  useEffect(() => {
    const active = callState.active;
    if (active?.call_type === "DirectVideo" && !active.peer_cam_enabled) {
      setHasRemoteVideoFrame(false);
      remoteVideoRendererRef.current?.reset();
    }
  }, [callState.active?.call_id, callState.active?.call_type, callState.active?.peer_cam_enabled]);

  useEffect(() => {
    const active = callState.active;
    if (shouldCaptureLocalVideo(active) && active) {
      if (
        encoderRunningCallIdRef.current !== active.call_id ||
        encoderRunningDeviceIdRef.current !== selectedCameraId
      ) {
        void startLocalCameraForCall(active.call_id, selectedCameraId);
      }
      return;
    }

    if (localCameraEncoderRef.current || encoderRunningCallIdRef.current !== null) {
      stopLocalCameraEncoder(true);
      if (active?.call_type === "DirectVideo" && !active.cam_enabled) {
        setLocalCameraStatus((prev) => (isCameraFailureStatus(prev) ? prev : "off"));
      }
    }
  }, [
    callState.active?.call_id,
    callState.active?.call_type,
    callState.active?.phase,
    callState.active?.cam_enabled,
    selectedCameraId,
    startLocalCameraForCall,
    stopLocalCameraEncoder,
  ]);

  const isTranslatableText = useCallback((m: MessageDto) => {
    if (m.is_recalled) return false;
    return getDisplayBodyText(m).trim().length > 0;
  }, []);

  const translateBatchMessages = useCallback(
    async (
      targetMsgs: MessageDto[],
      targetLang: string,
      singleMode = false,
      forceRefresh = false,
    ) => {
      const candidates = targetMsgs.filter((m) => {
        if (!isTranslatableText(m)) return false;
        if (!forceRefresh) {
          const cur = translationsRef.current[m.msg_id];
          if (
            !singleMode &&
            cur &&
            cur.targetLang === targetLang &&
            (cur.text || cur.loading || cur.skipped)
          ) {
            return false;
          }
        }
        return !inFlightTranslateRef.current.has(`${m.msg_id}:${targetLang}`);
      });
      if (candidates.length === 0) return;

      const detectMode = translateCfgRef.current?.detect_mode ?? "api_cache";
      const immediateUpdates: Record<string, MessageTranslationState> = {};
      const toFetch: { msg: MessageDto; text: string }[] = [];

      for (const m of candidates) {
        const text = getDisplayBodyText(m);
        const cacheKey = `${m.msg_id}:${targetLang}`;
        if (!forceRefresh) {
          const cached = translationCacheRef.current.get(cacheKey);
          if (cached) {
            immediateUpdates[m.msg_id] =
              singleMode && cached.skipped && cached.src
                ? { ...cached, note: t("alreadyInTargetLang", { lang: targetLang }) }
                : cached;
            continue;
          }
        }

        const skipReason = classifySkipTranslation(text, targetLang);
        if (!forceRefresh && skipReason === "non_linguistic") {
          const baseEntry: MessageTranslationState = {
            targetLang,
            loading: false,
            skipped: true,
          };
          translationCacheRef.current.set(cacheKey, baseEntry);
          immediateUpdates[m.msg_id] = baseEntry;
          continue;
        }
        if (
          !forceRefresh &&
          skipReason === "same_language" &&
          detectMode === "local_smart_skip"
        ) {
          const baseEntry: MessageTranslationState = {
            targetLang,
            loading: false,
            skipped: true,
            src: normalizeLangBase(targetLang),
          };
          translationCacheRef.current.set(cacheKey, baseEntry);
          immediateUpdates[m.msg_id] = singleMode
            ? { ...baseEntry, note: t("alreadyInTargetLang", { lang: targetLang }) }
            : baseEntry;
          continue;
        }

        toFetch.push({ msg: m, text });
      }

      if (Object.keys(immediateUpdates).length > 0) {
        setTranslationsByMsg((prev) => ({ ...prev, ...immediateUpdates }));
      }
      if (toFetch.length === 0) return;

      for (const { msg: m } of toFetch) {
        inFlightTranslateRef.current.add(`${m.msg_id}:${targetLang}`);
      }
      setTranslationsByMsg((prev) => {
        const next = { ...prev };
        for (const { msg: m } of toFetch) {
          next[m.msg_id] = { targetLang, loading: true };
        }
        return next;
      });
      if (!singleMode) setThreadTranslating(true);

      try {
        const results = await bridge.translateMessages(
          toFetch.map(({ msg: m, text }) => ({ msg_id: m.msg_id, text })),
          targetLang,
          undefined,
          forceRefresh,
        );
        const byId = new Map(results.map((r) => [r.msg_id, r]));
        const fetchedEntries: Record<string, MessageTranslationState> = {};
        const missingIds: string[] = [];
        for (const { msg: m, text } of toFetch) {
          const hit = byId.get(m.msg_id);
          const cacheKey = `${m.msg_id}:${targetLang}`;
          if (!hit) {
            missingIds.push(m.msg_id);
            continue;
          }
          const isSkipped =
            Boolean(hit.skipped) ||
            isSameLangCode(hit.detected_source_lang, targetLang) ||
            isEffectivelySameText(text, hit.translated_text);
          if (isSkipped) {
            const baseEntry: MessageTranslationState = {
              targetLang,
              loading: false,
              skipped: true,
              src: hit.detected_source_lang,
            };
            translationCacheRef.current.set(cacheKey, baseEntry);
            fetchedEntries[m.msg_id] = singleMode
              ? { ...baseEntry, note: t("alreadyInTargetLang", { lang: targetLang }) }
              : baseEntry;
          } else {
            const entry: MessageTranslationState = {
              text: hit.translated_text,
              src: hit.detected_source_lang,
              targetLang,
              loading: false,
            };
            translationCacheRef.current.set(cacheKey, entry);
            fetchedEntries[m.msg_id] = entry;
          }
        }
        setTranslationsByMsg((prev) => {
          const next = { ...prev, ...fetchedEntries };
          for (const mid of missingIds) {
            delete next[mid];
          }
          return next;
        });
      } catch (err) {
        const msg = String(err);
        setTranslationsByMsg((prev) => {
          const next = { ...prev };
          for (const { msg: m } of toFetch) {
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
        for (const { msg: m } of toFetch) {
          inFlightTranslateRef.current.delete(`${m.msg_id}:${targetLang}`);
        }
        if (!singleMode) setThreadTranslating(false);
      }
    },
    [isTranslatableText, showError, t],
  );

  const handleTranslateSingle = useCallback(
    (msg: MessageDto) => {
      const targetLang =
        (selectedIdRef.current && threadAutoTranslate[selectedIdRef.current]) ||
        translateCode;
      void translateBatchMessages([msg], targetLang, true);
    },
    [threadAutoTranslate, translateCode, translateBatchMessages],
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
      setThreadAutoTranslate((prev) => ({ ...prev, [selectedId]: translateCode }));
    }
  }, [selectedId, threadAutoTranslate, translateCode, messages]);

  const syncRemoteThreads = useCallback(async () => {
    setSyncing(true);
    try {
      const list = await bridge.syncThreads("all");
      setThreads(list);
      const curTid = selectedIdRef.current;
      if (curTid && pendingMarkReadRef.current === curTid) {
        const synced = list.find((t) => t.id === curTid);
        if (synced && synced.unread_count === 0) {
          pendingMarkReadRef.current = null;
          setUnreadDividerCount(0);
          setUnreadBelow(0);
        }
      }
      if (!selectedIdRef.current && list.length > 0) {
        setSelectedId(list[0].id);
      }
    } catch (err) {
      showError(err);
    } finally {
      setSyncing(false);
    }
  }, [showError]);

  const refreshThreadsSilently = useCallback(async () => {
    if (silentSyncInFlightRef.current) return;
    silentSyncInFlightRef.current = true;
    try {
      const list = await bridge.syncThreads("all");
      setThreads(list);
      const curTid = selectedIdRef.current;
      if (curTid && pendingMarkReadRef.current === curTid) {
        const synced = list.find((item) => item.id === curTid);
        if (synced && synced.unread_count === 0) {
          pendingMarkReadRef.current = null;
          setUnreadDividerCount(0);
          setUnreadBelow(0);
        }
      }
    } catch {
      // Best-effort silent refresh
    } finally {
      silentSyncInFlightRef.current = false;
    }
  }, []);

  const refreshFriendRequestsCount = useCallback(async () => {
    try {
      const res = await bridge.friendListRequests();
      setPendingFriendRequestsCount(Array.isArray(res.received) ? res.received.length : 0);
    } catch {
      // Best-effort badge refresh
    }
  }, []);

  const handleRelationshipChanged = useCallback(() => {
    void refreshFriendRequestsCount();
    void refreshThreadsSilently();
  }, [refreshFriendRequestsCount, refreshThreadsSilently]);

  const removeGroupThreadLocally = useCallback((groupId: string) => {
    threadsRef.current = threadsRef.current.filter((item) => item.id !== groupId);
    setThreads((prev) => prev.filter((item) => item.id !== groupId));
    setGroupMembersCache((prev) => {
      if (!(groupId in prev)) return prev;
      const next = { ...prev };
      delete next[groupId];
      return next;
    });
    setCustomFolders((prev) =>
      prev.map((cf) =>
        cf.thread_ids.includes(groupId)
          ? { ...cf, thread_ids: cf.thread_ids.filter((id) => id !== groupId) }
          : cf,
      ),
    );
    setKickTarget((prev) => (prev?.groupId === groupId ? null : prev));
    setLeaveGroupTarget((prev) => (prev?.id === groupId ? null : prev));
    if (selectedIdRef.current === groupId) {
      selectedIdRef.current = null;
      if (pendingMarkReadRef.current === groupId) {
        pendingMarkReadRef.current = null;
      }
      detailsFetchKeyRef.current = null;
      detailsInFlightRef.current = null;
      setSelectedId(null);
      setMessages([]);
      setThreadDetails(null);
      setDrawerOpen(false);
      setAddMembersOpen(false);
      setCreatePollOpen(false);
      setCreateReminderOpen(false);
      setActiveThreadRootId(null);
      setReplyTo(null);
      setPinnedMessages([]);
      setHasMoreHistory(false);
      void bridge.setActiveThread(null);
    }
  }, []);

  const markRecentlyLeftGroup = useCallback((groupId: string) => {
    recentlyLeftGroupIdsRef.current.add(groupId);
    const prevTimer = recentlyLeftTimersRef.current.get(groupId);
    if (prevTimer !== undefined) {
      window.clearTimeout(prevTimer);
    }
    const timerId = window.setTimeout(() => {
      recentlyLeftTimersRef.current.delete(groupId);
      recentlyLeftGroupIdsRef.current.delete(groupId);
    }, 10_000);
    recentlyLeftTimersRef.current.set(groupId, timerId);
  }, []);

  const unmarkRecentlyLeftGroup = useCallback((groupId: string) => {
    recentlyLeftGroupIdsRef.current.delete(groupId);
    const prevTimer = recentlyLeftTimersRef.current.get(groupId);
    if (prevTimer !== undefined) {
      window.clearTimeout(prevTimer);
      recentlyLeftTimersRef.current.delete(groupId);
    }
  }, []);

  const handleOpenLeaveGroupDialog = useCallback((target: ThreadDto) => {
    setLeaveGroupSilent(false);
    setLeaveGroupError(null);
    setLeaveGroupBusy(false);
    setLeaveGroupTarget(target);
  }, []);

  const handleConfirmLeaveGroup = useCallback(async () => {
    if (!leaveGroupTarget || leaveGroupBusy) return;
    const target = leaveGroupTarget;
    const silent = leaveGroupSilent;
    setLeaveGroupBusy(true);
    setLeaveGroupError(null);
    markRecentlyLeftGroup(target.id);
    try {
      const res = await bridge.groupLeave(target.id, silent);
      if (res.left) {
        removeGroupThreadLocally(target.id);
        setLeaveGroupTarget(null);
        setLeaveGroupSilent(false);
        setLeaveGroupError(null);
        showSocialNotice(tRef.current("groupLeftSuccess", { name: target.title }), "success");
      } else {
        unmarkRecentlyLeftGroup(target.id);
        setLeaveGroupError(res.message?.trim() || tRef.current("groupLeaveBtn"));
      }
    } catch (err) {
      unmarkRecentlyLeftGroup(target.id);
      const clean = String(err).replace(/^Error:\s*/i, "").trim();
      setLeaveGroupError(clean || String(err));
    } finally {
      setLeaveGroupBusy(false);
    }
  }, [
    leaveGroupTarget,
    leaveGroupSilent,
    leaveGroupBusy,
    markRecentlyLeftGroup,
    unmarkRecentlyLeftGroup,
    removeGroupThreadLocally,
    showSocialNotice,
  ]);

  const handleConfirmKickMember = useCallback(async () => {
    if (!kickTarget || kickBusy) return;
    const { groupId, member } = kickTarget;
    setKickBusy(true);
    setKickError(null);
    try {
      const res = await bridge.groupKickMembers(groupId, [member.id]);
      const removedOk =
        res.removed.includes(member.id) ||
        (res.removed.length > 0 && !res.failed.includes(member.id));
      if (removedOk) {
        setGroupMembersCache((prev) => {
          if (!(groupId in prev)) return prev;
          const next = { ...prev };
          delete next[groupId];
          return next;
        });
        setThreadDetails((prev) => {
          if (!prev || prev.thread_id !== groupId) return prev;
          const nextMembers = prev.members.filter((m) => m.id !== member.id);
          return {
            ...prev,
            members: nextMembers,
            total_members:
              typeof prev.total_members === "number"
                ? Math.max(nextMembers.length, prev.total_members - 1)
                : prev.total_members,
            admin_ids: (prev.admin_ids ?? []).filter((id) => id !== member.id),
          };
        });
        setDetailsRefreshTick((n) => n + 1);
        setKickTarget(null);
        setKickError(null);
        showSocialNotice(tRef.current("groupKickSuccess", { name: member.name }), "success");
      } else {
        const msg = res.message?.trim() || tRef.current("groupKickFailed");
        setKickError(msg);
        showSocialNotice(msg, "error");
      }
    } catch (err) {
      const clean = String(err).replace(/^Error:\s*/i, "").trim() || tRef.current("groupKickFailed");
      setKickError(clean);
      showSocialNotice(clean, "error");
    } finally {
      setKickBusy(false);
    }
  }, [kickTarget, kickBusy, showSocialNotice]);

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

  const completeMarkReadForThread = useCallback((tid: string) => {
    if (pendingMarkReadRef.current === tid) {
      pendingMarkReadRef.current = null;
    }
    threadsRef.current = applyThreadReadSync(threadsRef.current, tid, false);
    void bridge.markRead(tid);
    if (
      tid === selectedIdRef.current &&
      authedRef.current &&
      !isLockedRef.current &&
      !hasActiveWebAppRef.current &&
      !webAppHubOpenRef.current &&
      chatPaneVisibleRef.current
    ) {
      void bridge.setActiveThread(tid);
    }
    setThreads((prev) => applyThreadReadSync(prev, tid, false));
  }, []);

  const tryAutoMarkReadActiveThread = useCallback(() => {
    const tid = selectedIdRef.current;
    if (!tid) return;
    const activeThreadUnread =
      threadsRef.current.find((t) => t.id === tid)?.unread_count ?? 0;
    const el = scrollContainerRef.current;
    const isMountedVisible = Boolean(el && el.clientHeight > 0);
    const isScrollable = isMountedVisible
      ? el!.scrollHeight > el!.clientHeight + 1
      : undefined;
    const distFromBottom = isMountedVisible
      ? Math.max(0, el!.scrollHeight - el!.scrollTop - el!.clientHeight)
      : null;
    if (
      shouldAutoMarkReadOnFocus({
        authed: authedRef.current,
        isLocked: isLockedRef.current,
        hasActiveWebApp: hasActiveWebAppRef.current,
        webAppHubOpen: webAppHubOpenRef.current,
        windowFocused: document.hasFocus(),
        visibilityState: document.visibilityState,
        selectedId: tid,
        pendingMarkReadId: pendingMarkReadRef.current,
        activeThreadUnread,
        distFromBottom,
        isScrollable,
        chatPaneVisible: chatPaneVisibleRef.current,
        initialScrollReady: initialScrollReadyRef.current,
      })
    ) {
      completeMarkReadForThread(tid);
    }
  }, [completeMarkReadForThread]);

  useEffect(() => {
    let rafId = 0;
    const onFocusOrVisibility = () => {
      if (rafId) {
        window.cancelAnimationFrame(rafId);
        rafId = 0;
      }
      const el = scrollContainerRef.current;
      if (
        document.visibilityState === "visible" &&
        document.hasFocus() &&
        (!el || el.clientHeight === 0)
      ) {
        rafId = window.requestAnimationFrame(() => {
          rafId = 0;
          tryAutoMarkReadActiveThread();
        });
        return;
      }
      tryAutoMarkReadActiveThread();
    };
    window.addEventListener("focus", onFocusOrVisibility);
    document.addEventListener("visibilitychange", onFocusOrVisibility);
    return () => {
      if (rafId) {
        window.cancelAnimationFrame(rafId);
      }
      window.removeEventListener("focus", onFocusOrVisibility);
      document.removeEventListener("visibilitychange", onFocusOrVisibility);
    };
  }, [tryAutoMarkReadActiveThread]);

  const selectAndRevealThread = useCallback(
    (tid: string, opts?: { forceScroll?: boolean; listOverride?: ThreadDto[] }) => {
      const sourceThreads = opts?.listOverride ?? threadsRef.current;
      const next = applyOpenThreadSearchState(
        {
          filterText: filterTextRef.current,
          selectedId: selectedIdRef.current,
          folder: folderRef.current,
        },
        tid,
        sourceThreads,
        customFoldersRef.current,
      );
      if (next.clearedSearch) {
        filterTextRef.current = "";
        setFilterText("");
      }
      if (next.folder !== folderRef.current) {
        folderRef.current = next.folder;
        setFolder(next.folder);
      }
      if (opts?.forceScroll || next.shouldScrollSidebar) {
        pendingSidebarScrollIdRef.current = tid;
      } else {
        pendingSidebarScrollIdRef.current = null;
      }
      setAiChatOpen(false);
      setSelectedId(tid);
    },
    [],
  );

  const openThreadById = useCallback(
    (tid: string) => {
      leaveWebApps();
      selectAndRevealThread(tid, { forceScroll: true });
      setActiveThreadRootId(null);
      setMobilePane("chat");
    },
    [leaveWebApps, selectAndRevealThread],
  );

  const triggerNotifyPopup = useCallback((payload: NotifyToastPayload) => {
    if (isTauri()) {
      if (notifyPrefsRef.current.useCustomPopup) {
        void bridge.showNotifyPopup(payload).catch(() => undefined);
      } else {
        void showDesktopNotification(payload.title, payload.body);
      }
      return;
    }
    setWebToastHovered(false);
    setWebNotifyToast(payload);
    if (webToastTimerRef.current !== null) {
      window.clearTimeout(webToastTimerRef.current);
    }
    webToastTimerRef.current = window.setTimeout(() => {
      webToastTimerRef.current = null;
      setWebToastHovered(false);
      setWebNotifyToast(null);
    }, Math.max(2500, Math.min(15000, payload.duration_ms)));
  }, []);

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
          const prev = prevConnStatusRef.current;
          prevConnStatusRef.current = ev.payload;
          setConnStatus(ev.payload);
          if (prev !== "connected" && ev.payload === "connected") {
            const curTid = selectedIdRef.current;
            if (curTid) {
              const curThread = threadsRef.current.find((t) => t.id === curTid);
              if (
                curThread?.kind === "group" &&
                (groupMembersCacheRef.current[curTid]?.length ?? 0) === 0
              ) {
                setDetailsRefreshTick((n) => n + 1);
              }
            }
          }
          if (prev === "reconnecting" && ev.payload === "connected") {
            void syncRemoteThreads();
            const curTid = selectedIdRef.current;
            if (curTid) {
              void bridge
                .loadMessages(curTid, undefined, true)
                .then((page) => {
                  if (selectedIdRef.current !== curTid) return;
                  setMessages((prevMsgs) => {
                    let next = prevMsgs;
                    for (const m of page.messages) {
                      next = upsertMsg(next, m);
                    }
                    return next;
                  });
                })
                .catch(() => undefined);
            }
          }
        }),
        listen<MessageDto>("core://message-new", (ev) => {
          const msg = ev.payload;
          const hasThread = threadsRef.current.some((t) => t.id === msg.thread_id);
          const isActiveThread = msg.thread_id === selectedIdRef.current;
          const chatVisible =
            !isLockedRef.current &&
            !hasActiveWebAppRef.current &&
            !webAppHubOpenRef.current &&
            chatPaneVisibleRef.current;
          const el = isActiveThread && chatVisible ? scrollContainerRef.current : null;
          const nearBottom =
            isActiveThread &&
            chatVisible &&
            Boolean(el) &&
            (stickToBottomRef.current ||
              el!.scrollHeight - el!.scrollTop - el!.clientHeight < 140);
          const wasPendingMarkRead = pendingMarkReadRef.current === msg.thread_id;
          const readingLive =
            !msg.is_outgoing &&
            nearBottom &&
            document.hasFocus() &&
            document.visibilityState === "visible" &&
            !wasPendingMarkRead;
          const incUnread = !msg.is_outgoing && !readingLive;
          setThreads((prev) => bumpThreadList(prev, msg, incUnread));
          if (!hasThread) {
            void bridge
              .listThreads("all")
              .then(setThreads)
              .catch(() => undefined);
          }
          if (isActiveThread) {
            setMessages((prev) => upsertMsg(prev, msg));
            if (msg.attachment_url) {
              setDetailsRefreshTick((n) => n + 1);
            }
            if (!msg.is_outgoing && readingLive) {
              completeMarkReadForThread(msg.thread_id);
            } else if (!msg.is_outgoing) {
              pendingMarkReadRef.current = msg.thread_id;
              void bridge.setActiveThread(null);
            } else if (wasPendingMarkRead) {
              pendingMarkReadRef.current = null;
              if (authedRef.current && chatVisible) {
                void bridge.setActiveThread(msg.thread_id);
              }
            }
            if (msg.is_outgoing) {
              stickToBottomRef.current = true;
              setUnreadDividerCount(0);
              setUnreadBelow(0);
            }
            if ((nearBottom && !wasPendingMarkRead) || msg.is_outgoing) {
              setTimeout(() => {
                lastProgrammaticScrollAtRef.current = performance.now();
                bottomRef.current?.scrollIntoView({
                  behavior: document.visibilityState === "visible" ? "smooth" : "auto",
                });
              }, 40);
            } else if (!msg.is_outgoing) {
              setUnreadBelow((n) => n + 1);
              setShowScrollDown(true);
            }
          }
          const matchedThread = threadsRef.current.find((t) => t.id === msg.thread_id);
          const isRecent = Math.abs(Date.now() - msg.ts) < 90_000;
          const prefs = notifyPrefsRef.current;
          const threadMuted = isThreadCurrentlyMuted(matchedThread);
          if (!isBuzzMessage(msg)) {
            resetBuzzStreak(buzzStreaksRef.current, msg.thread_id);
          }
          if (!msg.is_outgoing && isBuzzMessage(msg) && prefs.buzz) {
            if (!threadMuted && isRecent) {
              const streak = recordIncomingBuzz(
                buzzStreaksRef.current,
                msg.thread_id,
                msg.msg_id,
                Date.now(),
              );
              if (!streak.isDuplicate) {
                const fullScreenBuzz = streak.shouldShakeScreen;
                playBuzzSound(fullScreenBuzz);
                const senderName = msg.sender_name || matchedThread?.title || "Liên hệ";
                const locked = isLockedRef.current;
                if (prefs.enabled) {
                  triggerNotifyPopup({
                    thread_id: msg.thread_id,
                    title: locked ? "Teza" : matchedThread?.title || msg.sender_name || "Teza",
                    body: locked
                      ? streak.count >= 2
                        ? `⚡ BUZZ! (×${streak.count})`
                        : "⚡ BUZZ!"
                      : `${tRef.current("buzzSentByUser", { name: senderName })}${
                          streak.count >= 2 ? ` (×${streak.count})` : ""
                        }`,
                    avatar: locked ? "" : matchedThread?.avatar || "",
                    theme: themeRef.current,
                    position: prefs.position,
                    duration_ms: prefs.durationSec * 1000,
                    is_buzz: true,
                    buzz_streak: streak.count,
                  });
                }
                if (!locked) {
                  setBuzzToastHovered(false);
                  setBuzzToast({
                    threadId: msg.thread_id,
                    threadTitle: matchedThread?.title || msg.sender_name || msg.thread_id,
                    senderName,
                    streak: streak.count,
                    nonce: Date.now(),
                  });
                  if (buzzToastTimerRef.current !== null) {
                    window.clearTimeout(buzzToastTimerRef.current);
                  }
                  buzzToastTimerRef.current = window.setTimeout(() => {
                    buzzToastTimerRef.current = null;
                    setBuzzToastHovered(false);
                    setBuzzToast(null);
                  }, 5000);
                }
                if (buzzShakeTimerRef.current !== null) {
                  window.clearTimeout(buzzShakeTimerRef.current);
                  buzzShakeTimerRef.current = null;
                }
                if (fullScreenBuzz && isTauri()) {
                  setBuzzShaking(false);
                  buzzShakeTimerRef.current = window.setTimeout(() => {
                    buzzShakeTimerRef.current = null;
                    void bridge.buzzShakeScreen().catch(() => undefined);
                  }, 60);
                } else {
                  setBuzzShaking(false);
                  window.requestAnimationFrame(() => setBuzzShaking(true));
                  buzzShakeTimerRef.current = window.setTimeout(() => {
                    buzzShakeTimerRef.current = null;
                    setBuzzShaking(false);
                  }, 1200);
                }
                if (isTauri()) {
                  void getCurrentWindow()
                    .requestUserAttention(2)
                    .catch(() => undefined);
                }
              }
            }
          } else if (isRecent) {
            const { showPopup, playSound } = shouldNotifyMessage({
              msg,
              thread: matchedThread,
              isActiveThread: msg.thread_id === selectedIdRef.current,
              windowFocused: document.hasFocus(),
              prefs,
              myUid: myProfileRef.current?.uid ?? "",
              myName: myProfileRef.current?.display_name ?? "",
            });
            if (playSound) {
              playMessageSound();
            }
            if (showPopup) {
              const locked = isLockedRef.current;
              const { title, body } = formatNotificationContent({
                msg,
                thread: matchedThread,
                previewMode: locked ? "hidden" : prefs.previewMode,
              });
              triggerNotifyPopup({
                thread_id: msg.thread_id,
                title,
                body,
                avatar: locked ? "" : matchedThread?.avatar || "",
                theme: themeRef.current,
                position: prefs.position,
                duration_ms: prefs.durationSec * 1000,
              });
            }
          }
        }),
        listen<MessageDto>("core://message-updated", (ev) => {
          const msg = ev.payload;
          const updatedPoll = parsePollPayload(msg);
          if (updatedPoll && updatedPoll.poll_id > 0 && updatedPoll.options.length > 0) {
            const pendingTimer = pollRefreshTimersRef.current.get(updatedPoll.poll_id);
            if (pendingTimer !== undefined) {
              window.clearTimeout(pendingTimer);
              pollRefreshTimersRef.current.delete(updatedPoll.poll_id);
            }
          }
          if (msg.thread_id === selectedIdRef.current) {
            setMessages((prev) => reconcileUpdatedMessage(prev, msg));
            if (msg.is_pinned || pinnedMsgIdsRef.current.has(msg.msg_id)) {
              void refreshPinnedMessages(msg.thread_id);
            }
          }
        }),
        listen<{ thread_id?: string; poll_id?: number | null }>("core://poll-updated", (ev) => {
          const pid = ev.payload?.poll_id;
          const tid = ev.payload?.thread_id;
          if (!pid || pid <= 0) return;
          const curSelectedId = selectedIdRef.current;
          if (!curSelectedId) return;
          if (tid && tid !== curSelectedId) return;
          const matched = messagesRef.current.find((m) => parsePollPayload(m)?.poll_id === pid);
          const targetThreadId = matched?.thread_id || tid || curSelectedId;
          if (targetThreadId !== curSelectedId) return;
          if (!matched && !tid) return;
          const existingTimer = pollRefreshTimersRef.current.get(pid);
          if (existingTimer !== undefined) {
            window.clearTimeout(existingTimer);
          }
          const timerId = window.setTimeout(() => {
            pollRefreshTimersRef.current.delete(pid);
            if (selectedIdRef.current !== targetThreadId) return;
            const curMsg = messagesRef.current.find((m) => parsePollPayload(m)?.poll_id === pid);
            void bridge
              .getPollDetail(pid, targetThreadId, curMsg?.msg_id)
              .then((updated) => {
                if (selectedIdRef.current !== targetThreadId) return;
                setMessages((prev) =>
                  applyPollUpdateToMessages(prev, pid, updated, curMsg?.msg_id),
                );
              })
              .catch(() => undefined);
          }, 250);
          pollRefreshTimersRef.current.set(pid, timerId);
        }),
        listen<{ thread_id: string; msg_id: string }>("core://message-deleted", (ev) => {
          if (ev.payload.thread_id === selectedIdRef.current) {
            setMessages((prev) => prev.filter((m) => m.msg_id !== ev.payload.msg_id));
            if (pinnedMsgIdsRef.current.has(ev.payload.msg_id)) {
              void refreshPinnedMessages(ev.payload.thread_id);
            }
          }
        }),
        listen("core://threads-updated", () => {
          void bridge.listThreads("all").then(setThreads);
        }),
        listen<{ thread_id: string; is_unread: boolean }>("core://thread-read-synced", (ev) => {
          const { thread_id, is_unread } = ev.payload;
          threadsRef.current = applyThreadReadSync(threadsRef.current, thread_id, is_unread);
          setThreads((prev) => applyThreadReadSync(prev, thread_id, is_unread));
          if (thread_id === selectedIdRef.current) {
            if (!is_unread) {
              setUnreadDividerCount(0);
              setUnreadBelow(0);
              if (pendingMarkReadRef.current === thread_id) {
                pendingMarkReadRef.current = null;
              }
              const el = scrollContainerRef.current;
              const nearBottomOrFlat =
                Boolean(el && el.clientHeight > 0) &&
                (el!.scrollHeight <= el!.clientHeight + 1 ||
                  el!.scrollHeight - el!.scrollTop - el!.clientHeight < 90);
              if (
                authedRef.current &&
                !isLockedRef.current &&
                !hasActiveWebAppRef.current &&
                !webAppHubOpenRef.current &&
                chatPaneVisibleRef.current &&
                nearBottomOrFlat
              ) {
                void bridge.setActiveThread(thread_id);
              }
            } else {
              void bridge.setActiveThread(null);
            }
          }
        }),
        listen<{ thread_id: string; peer_read_ts: number }>("core://thread-receipt", (ev) => {
          const { thread_id, peer_read_ts } = ev.payload;
          setThreads((prev) =>
            prev.map((t) =>
              t.id === thread_id
                ? { ...t, peer_read_ts: Math.max(t.peer_read_ts ?? 0, peer_read_ts) }
                : t,
            ),
          );
        }),
        listen<{ thread_id: string; messages: MessageDto[]; has_more?: boolean }>(
          "core://thread-messages-synced",
          (ev) => {
            if (ev.payload.thread_id !== selectedIdRef.current) return;
            const el = scrollContainerRef.current;
            const shouldStick =
              stickToBottomRef.current ||
              !el ||
              el.scrollHeight - el.scrollTop - el.clientHeight < 180;
            if (shouldStick) {
              stickToBottomRef.current = true;
            }
            setMessages((prev) => {
              let next = prev;
              for (const m of ev.payload.messages) {
                next = upsertMsg(next, m);
              }
              return next;
            });
            if (typeof ev.payload.has_more === "boolean") {
              setHasMoreHistory((prev) => prev || Boolean(ev.payload.has_more));
            }
            const newest = ev.payload.messages.reduce<MessageDto | null>(
              (best, m) => (!best || m.ts > best.ts ? m : best),
              null,
            );
            if (newest) {
              setThreads((prev) => bumpThreadList(prev, newest, false));
            }
            if (shouldStick) {
              window.requestAnimationFrame(() => {
                if (stickToBottomRef.current && selectedIdRef.current === ev.payload.thread_id) {
                  snapScrollToBottom();
                }
              });
              if (
                pendingMarkReadRef.current !== ev.payload.thread_id &&
                document.hasFocus() &&
                document.visibilityState === "visible" &&
                !isLockedRef.current &&
                !hasActiveWebAppRef.current &&
                !webAppHubOpenRef.current &&
                chatPaneVisibleRef.current
              ) {
                completeMarkReadForThread(ev.payload.thread_id);
              }
            }
          },
        ),
        listen<RelayConfigSummary>("core://relay-updated", (ev) => {
          applyRelaySummary(ev.payload);
        }),
        listen<CallStateDto>("core://call-availability", (ev) => {
          setCallState(ev.payload);
          if (ev.payload.phase !== "connected") {
            setCallTelemetry(null);
          }
        }),
        listen<CallEventEnvelopeDto>("core://call-event", (ev) => {
          const { event, state } = ev.payload;
          setCallState(state);
          if (state.phase !== "connected") {
            setCallTelemetry(null);
          }
          if (event.type === "connected") {
            playCallCueTone("connect", notifyPrefsRef.current.sound);
          } else if (event.type === "media_warning") {
            showCallNotice(event.message, "warn");
          } else if (event.type === "signaling_warning") {
            showCallNotice(event.message, "error");
          } else if (event.type === "call_ended") {
            setCallTelemetry(null);
            stopLocalCameraEncoder(true);
            remoteVideoRendererRef.current?.reset();
            playCallCueTone("end", notifyPrefsRef.current.sound);
            const classified = classifyCallEndReason(event.reason);
            if (!classified.isSilent) {
              if (classified.kind === "answered_on_other_device") {
                showCallNotice(tRef.current("callAnsweredElsewhereNotice"), "info");
              } else if (classified.kind === "ended_on_other_device") {
                showCallNotice(tRef.current("callEndedElsewhereNotice"), "info");
              } else if (classified.kind === "remote_cancelled") {
                showCallNotice(tRef.current("callRemoteCancelledNotice"), "info");
              } else if (classified.kind === "ring_timeout") {
                showCallNotice(tRef.current("callRingTimeoutNotice"), "warn");
              } else if (classified.kind === "remote_rejected") {
                showCallNotice(
                  classified.detailMessage || tRef.current("callEndedNotice"),
                  "warn",
                );
              } else if (classified.kind === "error") {
                showCallNotice(
                  classified.detailMessage || tRef.current("callEndedNotice"),
                  "error",
                );
              } else if (event.duration_secs > 0) {
                showCallNotice(
                  tRef.current("callEndedDurationNotice", {
                    duration: formatCallDuration(event.duration_secs),
                  }),
                  "info",
                );
              } else if (classified.kind === "remote_ended") {
                showCallNotice(tRef.current("callEndedNotice"), "info");
              }
            }
          }
        }),
        listen<CallTelemetryDto>("core://call-telemetry", (ev) => {
          setCallTelemetry(ev.payload);
        }),
        listen<CallVideoFrameDto>("core://call-video-frame", (ev) => {
          const payload = ev.payload;
          const cur = activeCallRef.current;
          if (
            !cur ||
            cur.call_type !== "DirectVideo" ||
            cur.call_id !== payload.call_id ||
            !cur.peer_cam_enabled
          ) {
            return;
          }
          ensureRemoteVideoRenderer().pushFrame({
            timestamp: payload.timestamp,
            is_keyframe: payload.is_keyframe,
            just_synced: payload.just_synced,
            data_b64: payload.data_b64,
          });
        }),
        listen<CallVideoKeyframeRequestDto>("core://call-video-keyframe-request", (ev) => {
          const cur = activeCallRef.current;
          if (!cur || cur.call_type !== "DirectVideo" || cur.call_id !== ev.payload.call_id) {
            return;
          }
          localCameraEncoderRef.current?.requestKeyframe();
        }),
        listen<string>("core://open-thread", (ev) => {
          if (ev.payload) {
            openThreadById(ev.payload);
          }
        }),
        listen<DownloadProgressDto>("core://download-progress", (ev) => {
          const p = ev.payload;
          setActiveDownloads((prev) => applyDownloadProgress(prev, p));
          if (p.stage === "done" && p.saved_path && !handledDoneDownloadIdsRef.current.has(p.id)) {
            if (handledDoneDownloadIdsRef.current.size > 200) {
              handledDoneDownloadIdsRef.current.clear();
            }
            handledDoneDownloadIdsRef.current.add(p.id);
            setCompletedDownload({
              path: p.saved_path,
              file_name: p.file_name,
              can_open_directly: p.can_open_directly,
            });
          }
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
  }, [
    syncRemoteThreads,
    refreshQr,
    applyRelaySummary,
    refreshPinnedMessages,
    completeMarkReadForThread,
    openThreadById,
    triggerNotifyPopup,
    snapScrollToBottom,
    showCallNotice,
    ensureRemoteVideoRenderer,
    stopLocalCameraEncoder,
  ]);

  useEffect(() => {
    if (!authed) return;
    void bridge.listThreads("all").then((list) => {
      setThreads(list);
      if (!selectedIdRef.current && list.length > 0) {
        setSelectedId(list[0].id);
      }
    });
    void bridge
      .listChatFolders()
      .then(setCustomFolders)
      .catch(() => undefined);
    void refreshFriendRequestsCount();
  }, [authed, refreshFriendRequestsCount]);

  useEffect(() => {
    if (!authed) return;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    void bridge
      .onSocialEvent((ev) => {
        if (cancelled) return;
        if (
          (ev.kind === "group_left" || ev.kind === "group_member_removed") &&
          ev.self_removed &&
          ev.thread_id
        ) {
          const gid = ev.thread_id;
          const groupTitle =
            threadsRef.current.find((item) => item.id === gid)?.title || gid;
          removeGroupThreadLocally(gid);
          if (!(ev.kind === "group_left" && recentlyLeftGroupIdsRef.current.has(gid))) {
            if (ev.kind === "group_left") {
              showSocialNotice(
                tRef.current("groupLeftSuccess", { name: groupTitle }),
                "info",
              );
            } else {
              showSocialNotice(
                tRef.current("groupRemovedSelfNotice", { name: groupTitle }),
                "warn",
              );
            }
          }
          return;
        }

        if (
          ev.kind === "group_member_removed" ||
          ev.kind === "group_member_joined" ||
          ev.kind === "group_admin_changed"
        ) {
          if (ev.thread_id) {
            const gid = ev.thread_id;
            setGroupMembersCache((prev) => {
              if (!(gid in prev)) return prev;
              const next = { ...prev };
              delete next[gid];
              return next;
            });
            if (gid === selectedIdRef.current) {
              setDetailsRefreshTick((n) => n + 1);
            } else if (
              ev.kind === "group_member_joined" &&
              !threadsRef.current.some((item) => item.id === gid)
            ) {
              void refreshThreadsSilently();
            }
          }
          return;
        }

        if (ev.kind.startsWith("friend_") || ev.kind.startsWith("user_")) {
          void refreshFriendRequestsCount();
          if (ev.kind === "friend_added") {
            void refreshThreadsSilently();
          }
        }
      })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [
    authed,
    removeGroupThreadLocally,
    refreshFriendRequestsCount,
    refreshThreadsSilently,
    showSocialNotice,
  ]);

  useEffect(() => {
    if (!leaveGroupTarget) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !leaveGroupBusy) {
        e.preventDefault();
        setLeaveGroupTarget(null);
        setLeaveGroupError(null);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [leaveGroupTarget, leaveGroupBusy]);

  useEffect(() => {
    setKickTarget(null);
    setKickError(null);
  }, [selectedId, drawerOpen]);

  useEffect(() => {
    if (!folderMenu) return;
    const close = () => setFolderMenu(null);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [folderMenu]);

  const activeThreadKind = useMemo(
    () => threads.find((t) => t.id === selectedId)?.kind ?? null,
    [threads, selectedId],
  );

  useEffect(() => {
    if (
      !authed ||
      !selectedId ||
      activeThreadKind === null ||
      (!drawerOpen && activeThreadKind !== "group")
    ) {
      return;
    }
    const fetchKey = `${selectedId}:${detailsRefreshTick}`;
    if (detailsFetchKeyRef.current === fetchKey || detailsInFlightRef.current === fetchKey) {
      return;
    }
    let cancelled = false;
    detailsInFlightRef.current = fetchKey;
    setDetailsLoading(true);
    void bridge
      .getThreadDetails(selectedId)
      .then((res) => {
        if (detailsInFlightRef.current === fetchKey) {
          detailsInFlightRef.current = null;
        }
        if (cancelled) return;
        if (res.members.length > 0) {
          setGroupMembersCache((prev) => ({ ...prev, [res.thread_id]: res.members }));
        }
        if (activeThreadKind !== null && (activeThreadKind !== "group" || res.members.length > 0)) {
          detailsFetchKeyRef.current = fetchKey;
        }
        setThreadDetails((prev) => {
          if (
            res.members.length === 0 &&
            prev?.thread_id === res.thread_id &&
            prev.members.length > 0
          ) {
            return { ...res, members: prev.members };
          }
          return res;
        });
      })
      .catch(() => {
        if (detailsInFlightRef.current === fetchKey) {
          detailsInFlightRef.current = null;
        }
        if (!cancelled) {
          setThreadDetails((prev) => (prev?.thread_id === selectedId ? prev : null));
        }
      })
      .finally(() => {
        if (!cancelled) setDetailsLoading(false);
      });
    return () => {
      cancelled = true;
      if (detailsInFlightRef.current === fetchKey) {
        detailsInFlightRef.current = null;
      }
    };
  }, [authed, drawerOpen, selectedId, activeThreadKind, detailsRefreshTick]);

  useEffect(() => {
    setThreadMenuFoldersExpanded(false);
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
          if (jumpHighlightTimerRef.current !== null) {
            window.clearTimeout(jumpHighlightTimerRef.current);
          }
          jumpHighlightTimerRef.current = window.setTimeout(() => {
            jumpHighlightTimerRef.current = null;
            setHighlightedId(null);
          }, 1800);
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
            setMessages(dedupeOptimisticPollMessages(around.messages));
            setHasMoreHistory(around.has_more);
            if (jumpRetryTimerRef.current !== null) {
              window.clearTimeout(jumpRetryTimerRef.current);
            }
            jumpRetryTimerRef.current = window.setTimeout(() => {
              jumpRetryTimerRef.current = null;
              highlightEl(msgId);
            }, 60);
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
            updatePendingAttachment({
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
            setDetailsRefreshTick((n) => n + 1);
            setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
          }
        } catch (err) {
          showError(err);
        }
      })
      .then((fn) => {
        if (!active) {
          fn();
          return;
        }
        unlisten = fn;
      });
    return () => {
      active = false;
      setDragHover(false);
      unlisten?.();
    };
  }, [authed, showError, updatePendingAttachment]);

  useEffect(() => {
    if (!authed || !selectedId) return;
    let cancelled = false;
    initialScrollReadyRef.current = false;
    stickToBottomRef.current = false;
    let raf1 = 0;
    let raf2 = 0;
    const timers: number[] = [];
    let detachUserWatch: (() => void) | null = null;

    const current = threadsRef.current.find((t) => t.id === selectedId);
    const initUnread = current?.unread_count ?? 0;
    const initWatermark = current?.last_read_in_ts ?? 0;
    setUnreadDividerCount(initUnread);
    setUnreadWatermarkTs(initWatermark);
    const chatObscured = Boolean(
      isLockedRef.current ||
        hasActiveWebAppRef.current ||
        webAppHubOpenRef.current ||
        !chatPaneVisibleRef.current,
    );
    if (initUnread === 0 && !chatObscured) {
      pendingMarkReadRef.current = null;
      completeMarkReadForThread(selectedId);
    } else if (initUnread === 0) {
      pendingMarkReadRef.current = null;
      void bridge.setActiveThread(null);
    } else {
      pendingMarkReadRef.current = selectedId;
      void bridge.setActiveThread(null);
    }
    setInChatSearchOpen(false);
    setInChatSearchQuery("");
    setInChatMatchIdx(0);
    setSelectMode(false);
    setSelectedMsgIds(new Set());
    setPinnedMessages([]);
    setPinnedIdx(0);
    setPinnedListOpen(false);
    lastOlderCursorRef.current = null;
    setReplyTo(null);
    setActiveThreadRootId(null);
    updatePendingAttachment(null);
    setThreadDetails(null);
    setMessages([]);
    setHasMoreHistory(false);
    setLoadingOlder(false);
    setLoadingMessages(true);
    setShowScrollDown(false);
    setUnreadBelow(0);
    void refreshPinnedMessages(selectedId);
    void bridge
      .loadMessages(selectedId, undefined, true)
      .then(async (page) => {
        if (cancelled) return;
        const pending = pendingJumpRef.current;
        if (pending && pending.threadId === selectedId) {
          pendingJumpRef.current = null;
          stickToBottomRef.current = false;
          if (!page.messages.some((m) => m.msg_id === pending.msgId)) {
            const around = await bridge.loadMessagesAround(selectedId, pending.ts);
            if (cancelled) return;
            setMessages(dedupeOptimisticPollMessages(around.messages));
            setHasMoreHistory(around.has_more);
          } else {
            setMessages(dedupeOptimisticPollMessages(page.messages));
            setHasMoreHistory(page.has_more);
          }
          timers.push(
            window.setTimeout(() => {
              if (cancelled) return;
              void jumpToMsg(pending.msgId);
              initialScrollReadyRef.current = true;
            }, 60),
          );
        } else {
          const cleanMessages = dedupeOptimisticPollMessages(page.messages);
          setMessages(cleanMessages);
          setHasMoreHistory(page.has_more);
          const targetUnreadId = findFirstUnreadMsgId(cleanMessages, initWatermark, initUnread);
          if (targetUnreadId) {
            stickToBottomRef.current = false;
            raf1 = window.requestAnimationFrame(() => {
              if (cancelled) return;
              raf2 = window.requestAnimationFrame(() => {
                if (cancelled) return;
                lastProgrammaticScrollAtRef.current = performance.now();
                document
                  .getElementById("unread-divider")
                  ?.scrollIntoView({ block: "start" });
                const el = scrollContainerRef.current;
                initialScrollReadyRef.current = true;
                if (!el || el.scrollHeight - el.scrollTop - el.clientHeight < 90) {
                  stickToBottomRef.current = true;
                  tryAutoMarkReadActiveThread();
                } else {
                  setShowScrollDown(true);
                  setUnreadBelow(initUnread);
                }

                let userInteracted = false;
                const markUserInteracted = () => {
                  userInteracted = true;
                };
                if (el) {
                  el.addEventListener("wheel", markUserInteracted, { passive: true });
                  el.addEventListener("pointerdown", markUserInteracted, { passive: true });
                  detachUserWatch = () => {
                    el.removeEventListener("wheel", markUserInteracted);
                    el.removeEventListener("pointerdown", markUserInteracted);
                  };
                }
                timers.push(
                  window.setTimeout(() => {
                    detachUserWatch?.();
                    detachUserWatch = null;
                    if (cancelled || userInteracted || stickToBottomRef.current) return;
                    const divider = document.getElementById("unread-divider");
                    if (divider) {
                      lastProgrammaticScrollAtRef.current = performance.now();
                      divider.scrollIntoView({ block: "start" });
                    }
                  }, 150),
                );
              });
            });
          } else {
            stickToBottomRef.current = true;
            raf1 = window.requestAnimationFrame(() => {
              if (cancelled) return;
              raf2 = window.requestAnimationFrame(() => {
                if (cancelled) return;
                if (stickToBottomRef.current) {
                  snapScrollToBottom();
                }
                initialScrollReadyRef.current = true;
                if (pendingMarkReadRef.current === selectedId) {
                  tryAutoMarkReadActiveThread();
                }
              });
            });
            timers.push(
              window.setTimeout(() => {
                if (cancelled) return;
                initialScrollReadyRef.current = true;
                if (stickToBottomRef.current) {
                  snapScrollToBottom();
                }
              }, 120),
            );
          }
        }
      })
      .finally(() => {
        if (!cancelled) setLoadingMessages(false);
      });
    return () => {
      cancelled = true;
      if (raf1) window.cancelAnimationFrame(raf1);
      if (raf2) window.cancelAnimationFrame(raf2);
      for (const id of timers) window.clearTimeout(id);
      detachUserWatch?.();
      detachUserWatch = null;
    };
  }, [
    authed,
    selectedId,
    jumpToMsg,
    refreshPinnedMessages,
    completeMarkReadForThread,
    tryAutoMarkReadActiveThread,
    snapScrollToBottom,
    updatePendingAttachment,
  ]);

  const activeThreadLang = selectedId ? threadAutoTranslate[selectedId] : undefined;
  const prevTranslateCodeRef = useRef(translateCode);
  useEffect(() => {
    const prevCode = prevTranslateCodeRef.current;
    if (prevCode === translateCode) return;
    prevTranslateCodeRef.current = translateCode;
    setThreadAutoTranslate((prev) => {
      let changed = false;
      const next: Record<string, string> = {};
      for (const [tid, target] of Object.entries(prev)) {
        if (target === prevCode) {
          next[tid] = translateCode;
          changed = true;
        } else {
          next[tid] = target;
        }
      }
      return changed ? next : prev;
    });
    setTranslationsByMsg((prev) => {
      if (Object.keys(prev).length === 0) return prev;
      const next: typeof prev = {};
      for (const [mid, item] of Object.entries(prev)) {
        if (item.targetLang !== prevCode) {
          next[mid] = item;
        }
      }
      return next;
    });
  }, [translateCode]);
  useEffect(() => {
    if (!authed || !selectedId || !activeThreadLang || messages.length === 0) return;
    void translateBatchMessages(messages, activeThreadLang);
  }, [authed, selectedId, activeThreadLang, messages, translateBatchMessages]);

  const loadOlderMessages = useCallback(async () => {
    if (!selectedId || loadingOlder || !hasMoreHistory || messages.length === 0) return;
    stickToBottomRef.current = false;
    const oldest = messages[0];
    const container = scrollContainerRef.current;
    const prevScrollHeight = container?.scrollHeight ?? 0;
    const prevScrollTop = container?.scrollTop ?? 0;
    const existingIds = new Set(messages.map((m) => m.msg_id));
    const prevCursor = lastOlderCursorRef.current;
    lastOlderCursorRef.current = oldest.msg_id;

    setLoadingOlder(true);
    try {
      const page = await bridge.loadMessages(selectedId, oldest.ts, false, oldest.msg_id);
      if (selectedIdRef.current !== selectedId) return;
      const hasFresh = page.messages.some((m) => !existingIds.has(m.msg_id));
      if (hasFresh) {
        setMessages((prev) => {
          const seen = new Set(prev.map((m) => m.msg_id));
          const fresh = page.messages.filter((m) => !seen.has(m.msg_id));
          return fresh.length > 0 ? dedupeOptimisticPollMessages([...fresh, ...prev]) : prev;
        });
        requestAnimationFrame(() => {
          if (container) {
            lastProgrammaticScrollAtRef.current = performance.now();
            const delta = container.scrollHeight - prevScrollHeight;
            container.scrollTop = prevScrollTop + delta;
          }
        });
        setHasMoreHistory(page.has_more);
      } else {
        setHasMoreHistory(page.has_more && prevCursor !== oldest.msg_id);
      }
    } catch (err) {
      showError(err);
    } finally {
      setLoadingOlder(false);
    }
  }, [selectedId, loadingOlder, hasMoreHistory, messages, showError]);

  const handleSelectSearchResult = useCallback(
    async (hit: MessageDto) => {
      leaveWebApps();
      setMobilePane("chat");
      if (hit.thread_id === selectedId) {
        selectAndRevealThread(hit.thread_id, { forceScroll: true });
        if (messages.some((m) => m.msg_id === hit.msg_id)) {
          setTimeout(() => jumpToMsg(hit.msg_id), 40);
        } else {
          try {
            const around = await bridge.loadMessagesAround(hit.thread_id, hit.ts);
            setMessages(dedupeOptimisticPollMessages(around.messages));
            setHasMoreHistory(around.has_more);
            setTimeout(() => jumpToMsg(hit.msg_id), 60);
          } catch (err) {
            showError(err);
          }
        }
      } else {
        pendingJumpRef.current = { threadId: hit.thread_id, msgId: hit.msg_id, ts: hit.ts };
        selectAndRevealThread(hit.thread_id, { forceScroll: true });
      }
    },
    [selectedId, messages, jumpToMsg, showError, leaveWebApps, selectAndRevealThread],
  );

  const visibleThreads = useMemo(() => {
    const byFolder = filterThreadsByFolder(threads, folder, customFolders);
    const q = filterText.trim();
    const base = q
      ? byFolder.filter((t) =>
          matchesSearchQuery(`${t.title}\n${t.last_preview}\n${t.id}`, q),
        )
      : [...byFolder];
    return base.sort(compareThreads);
  }, [threads, folder, customFolders, filterText]);
  visibleThreadsRef.current = visibleThreads;

  useEffect(() => {
    const targetId = pendingSidebarScrollIdRef.current;
    if (!targetId || targetId !== selectedId) return;
    const idx = visibleThreads.findIndex((item) => item.id === targetId);
    if (idx < 0) return;
    if (idx >= threadRenderLimit) {
      setThreadRenderLimit((prev) =>
        computeThreadRenderLimitForTarget(visibleThreads, targetId, prev),
      );
      return;
    }
    const container = sidebarListRef.current;
    if (!container) return;
    const rows = container.querySelectorAll<HTMLElement>("[data-thread-row]");
    for (const row of rows) {
      if (row.dataset.threadRow === targetId) {
        pendingSidebarScrollIdRef.current = null;
        row.scrollIntoView({ block: "nearest" });
        break;
      }
    }
  }, [selectedId, visibleThreads, threadRenderLimit]);

  const activeThread = useMemo(
    () => threads.find((t) => t.id === selectedId) ?? null,
    [threads, selectedId],
  );
  const hasActiveThread = Boolean(activeThread);
  const activeCallCheck = useMemo(
    () =>
      canStartVoiceCall({
        threadKind: activeThread?.kind ?? null,
        threadId: activeThread?.id ?? "",
        myUid: myProfile?.uid ?? "",
        relayEnabled: Boolean(relayCfg?.enabled),
        callPhase: callState.phase,
        authed,
      }),
    [
      activeThread?.kind,
      activeThread?.id,
      myProfile?.uid,
      relayCfg?.enabled,
      callState.phase,
      authed,
    ],
  );

  useEffect(() => {
    if (!authed || !selectedId || !hasActiveThread) return;
    const contentEl = messagesContentRef.current;
    if (!contentEl || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (stickToBottomRef.current) {
        snapScrollToBottom();
      }
    });
    observer.observe(contentEl);
    return () => observer.disconnect();
  }, [authed, selectedId, hasActiveThread, snapScrollToBottom]);

  const inChatMatches = useMemo(() => {
    const q = inChatSearchQuery.trim();
    if (!inChatSearchOpen || !q) return [];
    return messages
      .filter((m) => {
        if (m.is_recalled) return false;
        let extra = "";
        if (m.attachment_name) {
          if (m.attachment_name.startsWith("teza-poll:")) {
            const poll = parsePollPayload(m);
            extra = poll
              ? `${poll.question} ${poll.options.map((o) => o.content).join(" ")}`
              : "";
          } else if (
            !m.attachment_name.startsWith("teza-reminder:") &&
            !m.attachment_name.startsWith("teza-call:")
          ) {
            extra = m.attachment_name;
          }
        }
        return matchesSearchQuery(`${m.text}\n${m.sender_name}\n${extra}`, q);
      })
      .map((m) => m.msg_id);
  }, [inChatSearchOpen, inChatSearchQuery, messages]);

  const inChatActiveMatchId =
    inChatMatches.length > 0
      ? inChatMatches[Math.min(inChatMatchIdx, inChatMatches.length - 1)]
      : null;

  useEffect(() => {
    if (inChatMatches.length === 0) {
      setInChatMatchIdx(0);
    } else if (inChatMatchIdx >= inChatMatches.length) {
      setInChatMatchIdx(inChatMatches.length - 1);
    }
  }, [inChatMatches, inChatMatchIdx]);

  useEffect(() => {
    if (!inChatActiveMatchId) return;
    const el = document.getElementById(`msg-${inChatActiveMatchId}`);
    el?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, [inChatActiveMatchId]);

  const firstUnreadMsgId = useMemo(
    () => findFirstUnreadMsgId(messages, unreadWatermarkTs, unreadDividerCount),
    [messages, unreadWatermarkTs, unreadDividerCount],
  );

  const archivedThreads = useMemo(
    () => threads.filter((t) => Boolean(t.is_archived)).sort(compareThreads),
    [threads],
  );

  const archivedUnread = useMemo(
    () => archivedThreads.reduce((acc, t) => acc + (t.is_muted ? 0 : t.unread_count), 0),
    [archivedThreads],
  );

  const visibleBuiltInFolders = useMemo(
    () => FOLDERS.filter((item) => item.id === "all" || !hiddenBuiltInFolders.includes(item.id)),
    [hiddenBuiltInFolders],
  );

  const handleToggleBuiltInFolder = useCallback(
    (id: BuiltInRailFolderId) => {
      setHiddenBuiltInFolders((prev) => {
        const exists = prev.includes(id);
        const next = exists ? prev.filter((x) => x !== id) : [...prev, id];
        try {
          localStorage.setItem("litechat.hiddenBuiltInFolders", JSON.stringify(next));
        } catch {
          // ignore storage quota errors
        }
        return next;
      });
      if (folderRef.current === id) {
        setFolder("all");
      }
    },
    [],
  );

  const handleUpdatePasscode = useCallback((next: PasscodeConfig) => {
    savePasscodeConfig(next);
    setPasscodeCfg(next);
    if (!next.enabled) {
      setIsLocked(false);
    }
  }, []);

  const embeddedWebApps = useMemo(
    () => webAppsCfg.items.filter((x) => x.openMode === "embedded"),
    [webAppsCfg.items],
  );

  const activeWebApp = useMemo(
    () =>
      webAppsCfg.enabled && activeWebAppId
        ? webAppsCfg.items.find(
            (x) => x.id === activeWebAppId && x.openMode === "embedded",
          ) ?? null
        : null,
    [webAppsCfg, activeWebAppId],
  );
  hasActiveWebAppRef.current = Boolean(activeWebApp);

  const webappSuspended = Boolean(
    isLocked ||
      navDrawerOpen ||
      settingsOpen ||
      folderModalOpen ||
      quickSearchOpen ||
      findUserOpen ||
      friendRequestsOpen ||
      Boolean(leaveGroupTarget) ||
      translateModalOpen ||
      updateModalOpen ||
      profileUid ||
      threadMenu ||
      folderMenu ||
      lightboxState ||
      forwardMessages.length > 0 ||
      addMembersOpen ||
      createReminderOpen ||
      createPollOpen ||
      scheduledModalOpen ||
      screenshotEditor ||
      showVideoOverlay ||
      activeCallDevicesOpen ||
      callState.phase === "incoming_ringing",
  );

  useEffect(() => {
    if (!activeWebApp) {
      void bridge.webappHideAll();
    }
  }, [activeWebApp]);

  const prevChatObscuredRef = useRef<boolean>(
    Boolean(isLocked || activeWebApp || (webAppsCfg.enabled && webAppHubOpen) || !chatPaneVisible),
  );
  useEffect(() => {
    const obscured = Boolean(
      isLocked || activeWebApp || (webAppsCfg.enabled && webAppHubOpen) || !chatPaneVisible,
    );
    const wasObscured = prevChatObscuredRef.current;
    prevChatObscuredRef.current = obscured;

    if (!authed || !selectedId || obscured) {
      void bridge.setActiveThread(null);
      return;
    }

    if (!wasObscured) return;

    const activeThreadUnread =
      threadsRef.current.find((t) => t.id === selectedId)?.unread_count ?? 0;
    if (pendingMarkReadRef.current !== selectedId && activeThreadUnread === 0) {
      void bridge.setActiveThread(selectedId);
      return;
    }

    let raf1 = 0;
    let raf2 = 0;
    raf1 = window.requestAnimationFrame(() => {
      raf2 = window.requestAnimationFrame(() => {
        tryAutoMarkReadActiveThread();
      });
    });
    return () => {
      if (raf1) window.cancelAnimationFrame(raf1);
      if (raf2) window.cancelAnimationFrame(raf2);
    };
  }, [
    authed,
    selectedId,
    isLocked,
    activeWebApp,
    webAppsCfg.enabled,
    webAppHubOpen,
    chatPaneVisible,
    tryAutoMarkReadActiveThread,
  ]);

  const handleWebAppsCfgChange = useCallback(
    (next: WebAppsConfig) => {
      const prevMap = new Map(webAppsCfg.items.map((x) => [x.id, x]));
      const nextMap = new Map(next.items.map((x) => [x.id, x]));
      const removedIds: string[] = [];
      for (const [id, prevItem] of prevMap) {
        const nextItem = nextMap.get(id);
        if (!nextItem) {
          removedIds.push(id);
          void bridge.webappClose(id);
        } else if (nextItem.url !== prevItem.url || nextItem.openMode !== prevItem.openMode) {
          void bridge.webappClose(id);
        }
      }
      if (removedIds.length > 0) {
        for (const removedId of removedIds) {
          void bridge
            .listWebAppAccounts(removedId)
            .then((accounts) =>
              Promise.allSettled(
                accounts.map((acc) => bridge.deleteWebAppAccount(acc.id)),
              ),
            )
            .catch(() => undefined);
        }
      }
      const nextActiveItem = activeWebAppId ? nextMap.get(activeWebAppId) : undefined;
      if (!next.enabled) {
        setActiveWebAppId(null);
        setLastOpenedWebAppId(null);
        setWebAppHubOpen(false);
        void bridge.webappHideAll();
      } else {
        if (
          activeWebAppId &&
          (!nextActiveItem || nextActiveItem.openMode !== "embedded")
        ) {
          setActiveWebAppId(null);
          void bridge.webappHideAll();
        }
        setLastOpenedWebAppId((prev) => {
          if (!prev) return null;
          const nextLastItem = nextMap.get(prev);
          const prevLastItem = prevMap.get(prev);
          if (
            !nextLastItem ||
            nextLastItem.openMode !== "embedded" ||
            (prevLastItem && nextLastItem.url !== prevLastItem.url)
          ) {
            return null;
          }
          return prev;
        });
      }
      saveWebAppsConfig(next);
      setWebAppsCfg(next);
    },
    [webAppsCfg, activeWebAppId],
  );

  const handleActivateWebApp = useCallback(
    (item: WebAppItem) => {
      if (item.openMode === "external") {
        void bridge.openExternalUrl(item.url).catch(showError);
        return;
      }
      setWebAppHubOpen(false);
      setActiveWebAppId(item.id);
      setLastOpenedWebAppId(item.id);
      if (layout.mode === "single") setMobilePane("chat");
    },
    [layout.mode, showError],
  );

  const handleToggleArchive = async (target: ThreadDto, archived: boolean) => {
    try {
      await bridge.toggleArchive(target.id, archived);
      setThreads((prev) =>
        prev.map((t) => (t.id === target.id ? { ...t, is_archived: archived } : t)),
      );
      if (!archived && folderRef.current === "archived") {
        const remaining = threadsRef.current.filter(
          (t) => t.id !== target.id && Boolean(t.is_archived),
        ).length;
        if (remaining === 0) {
          setFolder("all");
        }
      }
    } catch (err) {
      showError(err);
    }
  };

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

  const handleToggleMute = async (target: ThreadDto, durationSecs?: number) => {
    const nextMuted = durationSecs !== undefined ? true : !isThreadCurrentlyMuted(target);
    try {
      const untilTs = await bridge.toggleMute(target.id, nextMuted, durationSecs);
      setThreads((prev) =>
        prev.map((t) =>
          t.id === target.id
            ? { ...t, is_muted: nextMuted, muted_until_ts: nextMuted ? untilTs : 0 }
            : t,
        ),
      );
      setDrawerMuteOpen(false);
    } catch (err) {
      showError(err);
    }
  };

  const formatMuteUntil = (ts?: number): string => {
    if (!ts || ts <= Date.now()) return "";
    return new Date(ts).toLocaleTimeString("vi-VN", {
      hour: "2-digit",
      minute: "2-digit",
    });
  };

  const handleMarkRead = useCallback(
    async (target: ThreadDto) => {
      try {
        await bridge.markRead(target.id);
        if (pendingMarkReadRef.current === target.id) {
          pendingMarkReadRef.current = null;
        }
        if (selectedIdRef.current === target.id) {
          setUnreadDividerCount(0);
          setUnreadBelow(0);
          if (
            authedRef.current &&
            !isLockedRef.current &&
            !hasActiveWebAppRef.current &&
            !webAppHubOpenRef.current &&
            chatPaneVisibleRef.current
          ) {
            void bridge.setActiveThread(target.id);
          }
        }
        threadsRef.current = applyThreadReadSync(threadsRef.current, target.id, false);
        setThreads((prev) => applyThreadReadSync(prev, target.id, false));
      } catch (err) {
        showError(err);
      }
    },
    [showError],
  );

  const handleMarkUnread = async (target: ThreadDto) => {
    try {
      await bridge.markUnread(target.id);
      if (selectedIdRef.current === target.id) {
        pendingMarkReadRef.current = target.id;
      }
      setThreads((prev) =>
        prev
          .map((t) =>
            t.id === target.id
              ? {
                  ...t,
                  unread_count: Math.max(1, t.unread_count),
                  last_read_in_ts:
                    t.last_read_in_ts && t.last_ts
                      ? Math.min(t.last_read_in_ts, t.last_ts - 1)
                      : t.last_read_in_ts,
                }
              : t,
          )
          .sort(compareThreads),
      );
    } catch (err) {
      showError(err);
    }
  };

  const handleMarkFolderRead = useCallback(
    async (selection: string) => {
      const targets = filterThreadsByFolder(threadsRef.current, selection, customFolders).filter(
        (t) => t.unread_count > 0,
      );
      if (targets.length === 0) return;
      try {
        await Promise.all(targets.map((t) => bridge.markRead(t.id)));
        const ids = new Set(targets.map((t) => t.id));
        const curTid = selectedIdRef.current;
        if (curTid && ids.has(curTid)) {
          if (pendingMarkReadRef.current === curTid) {
            pendingMarkReadRef.current = null;
          }
          setUnreadDividerCount(0);
          setUnreadBelow(0);
          if (
            authedRef.current &&
            !isLockedRef.current &&
            !hasActiveWebAppRef.current &&
            !webAppHubOpenRef.current &&
            chatPaneVisibleRef.current
          ) {
            void bridge.setActiveThread(curTid);
          }
        }
        const applyAll = (list: ThreadDto[]) =>
          list.map((t) =>
            ids.has(t.id)
              ? {
                  ...t,
                  unread_count: 0,
                  last_read_in_ts: Math.max(t.last_read_in_ts ?? 0, t.last_ts),
                }
              : t,
          );
        threadsRef.current = applyAll(threadsRef.current);
        setThreads(applyAll);
      } catch (err) {
        showError(err);
      }
    },
    [customFolders, showError],
  );

  const handleToggleThreadInFolder = useCallback(
    async (targetFolder: ChatFolderDto, threadId: string) => {
      const exists = targetFolder.thread_ids.includes(threadId);
      const nextFolder: ChatFolderDto = {
        ...targetFolder,
        thread_ids: exists
          ? targetFolder.thread_ids.filter((id) => id !== threadId)
          : [...targetFolder.thread_ids, threadId],
      };
      try {
        const list = await bridge.saveChatFolder(nextFolder);
        setCustomFolders(list);
      } catch (err) {
        showError(err);
      }
    },
    [showError],
  );

  const handleReplyMessage = useCallback((target: MessageDto) => {
    setReplyTo(target);
    setReplyFocusSeq((n) => n + 1);
  }, []);

  const handleJumpToMsgVoid = useCallback(
    (msgId: string) => {
      void jumpToMsg(msgId);
    },
    [jumpToMsg],
  );

  const handleOpenProfileModal = useCallback((uid: string) => {
    setProfileUid(uid);
  }, []);

  const handleForwardSingle = useCallback((target: MessageDto) => {
    setForwardMessages([target]);
  }, []);

  const handleHideTranslation = useCallback((msgId: string) => {
    setTranslationsByMsg((prev) => {
      const next = { ...prev };
      delete next[msgId];
      return next;
    });
  }, []);

  const sendTextInternal = useCallback(
    async (targetThreadId: string, text: string, quoteTarget: MessageDto | null): Promise<boolean> => {
      const tempId = `opt-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
      const quoteSnap = quoteTarget
        ? {
            msg_id: quoteTarget.msg_id,
            sender_name: quoteTarget.sender_name,
            snippet: quoteTarget.text,
          }
        : null;
      const optimistic: MessageDto = {
        msg_id: tempId,
        thread_id: targetThreadId,
        sender_id: myProfile?.uid || "me",
        sender_name: "Bạn",
        text,
        msg_type: "chat.text",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: quoteSnap,
        reactions: [],
        attachment_url: null,
        attachment_name: null,
        send_status: "sending",
      };
      if (selectedIdRef.current === targetThreadId) {
        setMessages((prev) => upsertMsg(prev, optimistic));
        setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 30);
      }
      try {
        const msg = quoteTarget
          ? await bridge.sendQuote(targetThreadId, text, quoteTarget)
          : await bridge.sendText(targetThreadId, text);
        if (selectedIdRef.current === targetThreadId) {
          setMessages((prev) => upsertMsg(prev.filter((m) => m.msg_id !== tempId), msg));
          setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
        }
        setThreads((prev) => bumpThreadList(prev, msg, false));
        return true;
      } catch (err) {
        const errMsg = String(err);
        if (selectedIdRef.current === targetThreadId) {
          setMessages((prev) =>
            prev.map((m) =>
              m.msg_id === tempId ? { ...m, send_status: "failed", send_error: errMsg } : m,
            ),
          );
        }
        showError(err);
        return true;
      }
    },
    [myProfile?.uid, showError],
  );

  const handleSendText = async (text: string): Promise<boolean> => {
    if (!selectedId) return false;
    const q = replyTo;
    setReplyTo(null);
    setUnreadDividerCount(0);
    setUnreadWatermarkTs(0);
    completeMarkReadForThread(selectedId);
    return await sendTextInternal(selectedId, text, q);
  };

  const handleSendThreadReply = useCallback(
    async (text: string, quoteTarget: MessageDto): Promise<boolean> => {
      if (!selectedId) return false;
      return await sendTextInternal(selectedId, text, quoteTarget);
    },
    [selectedId, sendTextInternal],
  );

  const handleDiscardFailedMessage = useCallback((msgId: string) => {
    setMessages((prev) => prev.filter((m) => m.msg_id !== msgId));
  }, []);

  const handleRetryFailedMessage = useCallback(
    (failedMsg: MessageDto) => {
      setMessages((prev) => prev.filter((m) => m.msg_id !== failedMsg.msg_id));
      const quoteTarget = failedMsg.quote
        ? messages.find((m) => m.msg_id === failedMsg.quote?.msg_id) ?? {
            ...failedMsg,
            msg_id: failedMsg.quote.msg_id,
            sender_name: failedMsg.quote.sender_name,
            text: failedMsg.quote.snippet,
          }
        : null;
      void sendTextInternal(failedMsg.thread_id, failedMsg.text, quoteTarget);
    },
    [messages, sendTextInternal],
  );

  const avatarByUserId = useMemo(() => {
    const map = new Map<string, string>();
    for (const t of threads) {
      if (t.kind === "direct" && t.avatar) {
        map.set(t.id, t.avatar);
      }
    }
    const cachedMembers = selectedId ? groupMembersCache[selectedId] : undefined;
    if (cachedMembers) {
      for (const mem of cachedMembers) {
        if (mem.id && mem.avatar) {
          map.set(mem.id, mem.avatar);
        }
      }
    }
    if (selectedId && threadDetails?.thread_id === selectedId && threadDetails.members) {
      for (const mem of threadDetails.members) {
        if (mem.id && mem.avatar) {
          map.set(mem.id, mem.avatar);
        }
      }
    }
    return map;
  }, [threads, selectedId, groupMembersCache, threadDetails]);

  const handleSaveRemoteFile = useCallback(
    (url: string, suggestedName: string, opts?: { threadId?: string; msgId?: string }) => {
      const downloadId = `dl-${Date.now()}-${++downloadSeqRef.current}`;
      if (isTauri()) {
        setActiveDownloads((prev) =>
          applyDownloadProgress(prev, {
            id: downloadId,
            file_name: suggestedName.trim() || "download",
            stage: "downloading",
            downloaded_bytes: 0,
            total_bytes: null,
            saved_path: null,
            can_open_directly: false,
            error: null,
          }),
        );
      }
      void bridge
        .saveRemoteFile(url, suggestedName, downloadId, opts)
        .then((res) => {
          setActiveDownloads((prev) => prev.filter((item) => item.id !== downloadId));
          if (res && !handledDoneDownloadIdsRef.current.has(downloadId)) {
            if (handledDoneDownloadIdsRef.current.size > 200) {
              handledDoneDownloadIdsRef.current.clear();
            }
            handledDoneDownloadIdsRef.current.add(downloadId);
            setCompletedDownload(res);
          }
        })
        .catch((err) => {
          setActiveDownloads((prev) => prev.filter((item) => item.id !== downloadId));
          showError(err);
        });
    },
    [showError],
  );

  const handleOpenExternalUrl = useCallback(
    (url: string) => {
      void bridge.openExternalUrl(url).catch(showError);
    },
    [showError],
  );

  const openLightboxForUrl = useCallback(
    (url: string, title: string, opts?: { threadId?: string; msgId?: string }) => {
      const targetUrl = normalizeMediaRequestUrl(url) || url;
      const gallery: LightboxItem[] = messages
        .filter((m) => {
          const norm = normalizeMediaRequestUrl(m.attachment_url);
          return (
            !m.is_recalled &&
            !!norm &&
            !m.msg_type.includes("sticker") &&
            m.msg_type !== "5" &&
            !m.msg_type.includes("voice") &&
            m.msg_type !== "3" &&
            !m.msg_type.includes("video") &&
            m.msg_type !== "4" &&
            (m.msg_type.includes("photo") ||
              m.msg_type.includes("gif") ||
              m.msg_type === "2" ||
              isDataImageUrl(norm) ||
              /\.(png|jpe?g|webp|gif|bmp)(\?|$)/i.test(norm))
          );
        })
        .map((m) => ({
          url: normalizeMediaRequestUrl(m.attachment_url),
          title: m.attachment_name || "Hình ảnh",
          threadId: m.thread_id,
          msgId: m.msg_id,
        }));
      const foundIdx = gallery.findIndex((item) => item.url === targetUrl);
      if (foundIdx >= 0) {
        setLightboxState({ items: gallery, index: foundIdx });
      } else {
        setLightboxState({
          items: [
            {
              url: targetUrl,
              title,
              threadId: opts?.threadId,
              msgId: opts?.msgId,
            },
          ],
          index: 0,
        });
      }
    },
    [messages],
  );

  const handleReactMessage = useCallback(
    (target: MessageDto, emoji: string) => {
      const threadId = target.thread_id || selectedIdRef.current;
      const canonicalEmoji = normalizeReactionEmoji(emoji);
      if (!threadId || !canonicalEmoji || target.is_recalled || target.send_status) return;

      const msgKey = `${threadId}:${target.msg_id}`;
      const seq = ++reactionSeqCounterRef.current;
      reactionSeqByMsgRef.current.set(msgKey, seq);
      const prevReactions = normalizeReactions(target.reactions);

      setMessages((list) =>
        list.map((m) =>
          m.msg_id === target.msg_id
            ? { ...m, reactions: applyOptimisticReaction(m.reactions, canonicalEmoji) }
            : m,
        ),
      );

      void bridge
        .reactMessage(threadId, target.msg_id, canonicalEmoji)
        .then((updated) => {
          if (reactionSeqByMsgRef.current.get(msgKey) !== seq) return;
          reactionSeqByMsgRef.current.delete(msgKey);
          if (selectedIdRef.current === updated.thread_id) {
            setMessages((list) => reconcileUpdatedMessage(list, updated));
          }
        })
        .catch((err) => {
          if (reactionSeqByMsgRef.current.get(msgKey) === seq) {
            reactionSeqByMsgRef.current.delete(msgKey);
            if (selectedIdRef.current === threadId) {
              setMessages((list) =>
                list.map((m) =>
                  m.msg_id === target.msg_id ? { ...m, reactions: prevReactions } : m,
                ),
              );
            }
          }
          showError(err);
        });
    },
    [showError],
  );

  const handleRecallMessage = useCallback(
    (target: MessageDto) => {
      if (!selectedId) return;
      void bridge
        .recallMessage(selectedId, target.msg_id)
        .then((updated) => {
          setMessages((list) => upsertMsg(list, updated));
          if (target.attachment_url) {
            setDetailsRefreshTick((n) => n + 1);
          }
          if (pinnedMsgIdsRef.current.has(updated.msg_id)) {
            void refreshPinnedMessages(selectedId);
          }
        })
        .catch(showError);
    },
    [selectedId, showError, refreshPinnedMessages],
  );

  const handleDeleteLocalMessage = useCallback(
    (target: MessageDto) => {
      if (!selectedId) return;
      void bridge
        .deleteMessageLocal(selectedId, target.msg_id)
        .then(() => {
          setMessages((list) => list.filter((x) => x.msg_id !== target.msg_id));
          if (pinnedMsgIdsRef.current.has(target.msg_id)) {
            void refreshPinnedMessages(selectedId);
          }
        })
        .catch(showError);
    },
    [selectedId, showError, refreshPinnedMessages],
  );

  const handleTogglePinMessage = useCallback(
    (target: MessageDto) => {
      const tid = selectedIdRef.current;
      if (!tid) return;
      void bridge
        .togglePinMessage(tid, target.msg_id, !target.is_pinned)
        .then((updated) => {
          setMessages((list) => upsertMsg(list, updated));
          void refreshPinnedMessages(tid);
        })
        .catch(showError);
    },
    [showError, refreshPinnedMessages],
  );

  const openDirectChatThread = useCallback(
    (rawUid: string, rawTitle: string, rawAvatar: string) => {
      const uid = rawUid.trim();
      if (!uid) return;
      const title = rawTitle.trim() || uid;
      const avatar = rawAvatar.trim();
      const existing = threadsRef.current.find((t) => t.id === uid);
      const fallbackEntry: ThreadDto = existing
        ? {
            ...existing,
            title: existing.title && existing.title !== existing.id ? existing.title : title,
            avatar: existing.avatar || avatar,
          }
        : {
            id: uid,
            title,
            avatar,
            kind: "direct",
            last_preview: "",
            last_ts: 0,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
          };
      const nextList = existing
        ? threadsRef.current.map((t) => (t.id === uid ? fallbackEntry : t))
        : [fallbackEntry, ...threadsRef.current].sort(compareThreads);
      threadsRef.current = nextList;
      setThreads((prev) => {
        const idx = prev.findIndex((t) => t.id === uid);
        if (idx >= 0) {
          const cur = prev[idx];
          const nextTitle = cur.title && cur.title !== cur.id ? cur.title : title;
          const nextAvatar = cur.avatar || avatar;
          if (nextTitle === cur.title && nextAvatar === cur.avatar) return prev;
          return prev.map((t, i) =>
            i === idx ? { ...t, title: nextTitle, avatar: nextAvatar } : t,
          );
        }
        return [fallbackEntry, ...prev].sort(compareThreads);
      });
      selectAndRevealThread(uid, { forceScroll: true, listOverride: nextList });
      setMobilePane("chat");
      void bridge
        .ensureDirectThread(uid, title, avatar)
        .then((ensured) => {
          setThreads((prev) => {
            const idx = prev.findIndex((t) => t.id === ensured.id);
            if (idx >= 0) {
              const cur = prev[idx];
              const merged: ThreadDto =
                cur.last_ts > ensured.last_ts
                  ? {
                      ...ensured,
                      last_ts: cur.last_ts,
                      last_preview: cur.last_preview || ensured.last_preview,
                      unread_count: cur.unread_count,
                    }
                  : ensured;
              return prev.map((t, i) => (i === idx ? merged : t)).sort(compareThreads);
            }
            return [ensured, ...prev].sort(compareThreads);
          });
        })
        .catch(showError);
    },
    [selectAndRevealThread, showError],
  );

  const handleStartChatFromProfile = useCallback(
    (profile: UserProfileDto) => {
      leaveWebApps();
      setFindUserInitialPhone("");
      openDirectChatThread(profile.uid, profile.display_name || profile.uid, profile.avatar || "");
    },
    [leaveWebApps, openDirectChatThread],
  );

  const handleStartChatFromFriendRequest = useCallback(
    (uid: string, name: string, avatar: string) => {
      setFriendRequestsOpen(false);
      leaveWebApps();
      openDirectChatThread(uid, name || uid, avatar || "");
    },
    [leaveWebApps, openDirectChatThread],
  );

  const handleSendSticker = useCallback(
    async (sticker: StickerItem) => {
      const tid = selectedIdRef.current;
      if (!tid) return;
      setUnreadDividerCount(0);
      setUnreadWatermarkTs(0);
      completeMarkReadForThread(tid);
      try {
        const sent = await bridge.sendSticker(tid, sticker);
        setMessages((prev) => upsertMsg(prev, sent));
        setThreads((prev) => bumpThreadList(prev, sent, false));
        setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
      } catch (err) {
        showError(err);
      }
    },
    [showError, completeMarkReadForThread],
  );

  const handleToggleSelectMsg = useCallback((msgId: string) => {
    setSelectedMsgIds((prev) => {
      const next = new Set(prev);
      if (next.has(msgId)) next.delete(msgId);
      else next.add(msgId);
      return next;
    });
  }, []);

  const handleStartSelect = useCallback((msgId: string) => {
    setSelectMode(true);
    setSelectedMsgIds(new Set([msgId]));
  }, []);

  const handleBulkDeleteSelected = useCallback(async () => {
    const tid = selectedIdRef.current;
    const ids = Array.from(selectedMsgIds);
    if (!tid || ids.length === 0) return;
    try {
      for (const id of ids) {
        await bridge.deleteMessageLocal(tid, id);
      }
      const deletedSet = new Set(ids);
      setMessages((prev) => prev.filter((m) => !deletedSet.has(m.msg_id)));
      setSelectMode(false);
      setSelectedMsgIds(new Set());
    } catch (err) {
      showError(err);
    }
  }, [selectedMsgIds, showError]);

  const handleVotePoll = useCallback(
    async (msg: MessageDto, optionIds: number[]) => {
      const poll = parsePollPayload(msg);
      if (!poll || poll.poll_id <= 0) return;
      try {
        const updated = await bridge.votePoll(
          poll.poll_id,
          optionIds,
          msg.thread_id,
          msg.msg_id,
        );
        setMessages((prev) => applyPollUpdateToMessages(prev, poll.poll_id, updated, msg.msg_id));
      } catch (err) {
        showError(err);
      }
    },
    [showError],
  );

  const handleAddPollOption = useCallback(
    async (msg: MessageDto, optionText: string, votedOptionIds: number[]): Promise<boolean> => {
      const poll = parsePollPayload(msg);
      const trimmed = optionText.trim();
      if (!poll || poll.poll_id <= 0 || !trimmed) return false;
      try {
        const updated = await bridge.addPollOptions(
          msg.thread_id,
          poll.poll_id,
          [trimmed],
          votedOptionIds,
        );
        setMessages((prev) => applyPollUpdateToMessages(prev, poll.poll_id, updated, msg.msg_id));
        return true;
      } catch (err) {
        showError(err);
        return false;
      }
    },
    [showError],
  );

  const handleLockPoll = useCallback(
    async (msg: MessageDto): Promise<boolean> => {
      const poll = parsePollPayload(msg);
      if (!poll || poll.poll_id <= 0) return false;
      try {
        const updated = await bridge.lockPoll(msg.thread_id, poll.poll_id);
        setMessages((prev) =>
          applyPollUpdateToMessages(prev, poll.poll_id, { ...updated, closed: true }, msg.msg_id),
        );
        return true;
      } catch (err) {
        showError(err);
        return false;
      }
    },
    [showError],
  );

  const handleRefreshPoll = useCallback(
    async (msg: MessageDto) => {
      const poll = parsePollPayload(msg);
      if (!poll || poll.poll_id <= 0) return;
      try {
        const updated = await bridge.getPollDetail(poll.poll_id, msg.thread_id, msg.msg_id);
        setMessages((prev) => applyPollUpdateToMessages(prev, poll.poll_id, updated, msg.msg_id));
      } catch (err) {
        showError(err);
      }
    },
    [showError],
  );

  const directDirectory = useMemo(() => {
    const map = new Map<string, { name: string; avatar: string }>();
    for (const th of threads) {
      if (th.kind === "direct" && th.id) {
        map.set(th.id, { name: th.title, avatar: th.avatar });
      }
    }
    return map;
  }, [threads]);

  const activeThreadDetails = useMemo(
    () =>
      activeThread && threadDetails?.thread_id === activeThread.id ? threadDetails : null,
    [activeThread, threadDetails],
  );

  const activeMentionMembers = useMemo<ThreadMemberDto[]>(() => {
    if (!activeThread || activeThread.kind !== "group") return [];
    if (activeThreadDetails && activeThreadDetails.members.length > 0) {
      return activeThreadDetails.members;
    }
    return groupMembersCache[activeThread.id] ?? [];
  }, [activeThread, activeThreadDetails, groupMembersCache]);

  const composerMentionMembers = useMemo<ThreadMemberDto[]>(
    () =>
      buildComposerMentionMembers({
        thread: activeThread,
        groupMembers: activeMentionMembers,
        messages,
        selfUid: myProfile?.uid,
        directDirectory,
        mentionAllRole: lang === "vi" ? "Mọi người" : lang === "ko" ? "모두" : "Everyone",
      }),
    [activeThread, activeMentionMembers, messages, myProfile?.uid, directDirectory, lang],
  );

  const messageMentionCandidates = useMemo(() => {
    const byId = new Map<string, { id: string; name: string }>();
    const add = (id: string | undefined, name: string | undefined) => {
      const uid = (id || "").trim();
      if (!uid) return;
      let nm = (name || "").normalize("NFC").trim();
      const dirEntry = directDirectory.get(uid);
      if (
        isPlaceholderMemberName(nm) &&
        dirEntry?.name &&
        !isPlaceholderMemberName(dirEntry.name)
      ) {
        nm = dirEntry.name.normalize("NFC").trim();
      }
      if (!nm || isPlaceholderMemberName(nm)) return;
      const existing = byId.get(uid);
      if (!existing || nm.length > existing.name.length) {
        byId.set(uid, { id: uid, name: nm });
      }
    };
    for (const m of activeMentionMembers) {
      add(m.id, m.name);
    }
    for (const msg of messages) {
      add(msg.sender_id, msg.sender_name);
    }
    if (myProfile) {
      add(myProfile.uid, myProfile.display_name);
    }
    if (activeThread && activeThread.kind === "direct") {
      add(activeThread.id, activeThread.title);
    }
    return Array.from(byId.values());
  }, [activeMentionMembers, messages, myProfile, activeThread, directDirectory]);

  const currentPinned =
    pinnedMessages.length > 0 ? pinnedMessages[pinnedIdx % pinnedMessages.length] : null;

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
  ): Promise<MessageDto | false> => {
    if (!selectedId) return false;
    setUnreadDividerCount(0);
    setUnreadWatermarkTs(0);
    completeMarkReadForThread(selectedId);
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
      setDetailsRefreshTick((n) => n + 1);
      setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
      return msg;
    } catch (err) {
      showError(err);
      return false;
    }
  };

  const closeScreenshotEditor = useCallback(() => {
    setScreenshotEditor((prev) => {
      if (prev?.revokeUrl) {
        URL.revokeObjectURL(prev.revokeUrl);
      }
      return null;
    });
  }, []);

  const handleCaptureScreen = async (opts?: {
    hideWindow?: boolean;
    monitorIndex?: number | null;
    captureAll?: boolean;
  }) => {
    if (!authedRef.current || !selectedIdRef.current || capturingScreenRef.current) {
      return;
    }
    capturingScreenRef.current = true;
    setCapturingScreen(true);
    try {
      const cap = await bridge.captureScreen({
        hideWindow: opts?.hideWindow ?? hideWindowOnCapture,
        monitorIndex: opts?.monitorIndex ?? null,
        captureAll: opts?.captureAll ?? false,
      });
      setScreenshotEditor((prev) => {
        if (prev?.revokeUrl) {
          URL.revokeObjectURL(prev.revokeUrl);
        }
        return {
          sourceUrl: cap.data_url,
          initialMode: "crop",
          monitors: cap.monitors,
          activeMonitorIndex: opts?.captureAll ? null : cap.active_monitor_index,
          canRecapture: true,
        };
      });
    } catch (err) {
      showError(t("screenshotCaptureError", { err: String(err) }));
    } finally {
      capturingScreenRef.current = false;
      setCapturingScreen(false);
    }
  };

  const handleEditPendingImage = () => {
    if (!pendingAttachment || pendingAttachment.kind !== "raw" || !pendingAttachment.isImage) {
      return;
    }
    const existingPreview = pendingAttachment.previewUrl;
    const objectUrl = existingPreview ? undefined : URL.createObjectURL(pendingAttachment.file);
    const sourceUrl = existingPreview ?? objectUrl ?? "";
    if (!sourceUrl) return;
    setScreenshotEditor((prev) => {
      if (prev?.revokeUrl) {
        URL.revokeObjectURL(prev.revokeUrl);
      }
      return {
        sourceUrl,
        revokeUrl: objectUrl,
        initialMode: "annotate",
        monitors: [],
        activeMonitorIndex: null,
        canRecapture: false,
      };
    });
  };

  handleCaptureScreenRef.current = () => {
    void handleCaptureScreen();
  };

  useEffect(() => {
    return () => {
      const curAtt = pendingAttachmentRef.current;
      if (curAtt?.kind === "raw" && curAtt.previewUrl) {
        URL.revokeObjectURL(curAtt.previewUrl);
      }
      if (screenshotEditorRef.current?.revokeUrl) {
        URL.revokeObjectURL(screenshotEditorRef.current.revokeUrl);
      }
    };
  }, []);

  const handleSendThreadAttachment = async (
    att: PendingAttachment,
    caption: string,
    rootMsgId: string,
  ): Promise<boolean> => {
    const sent = await handleSendAttachment(att, caption);
    if (!sent) return false;
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
    return true;
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

  const webAppStageFallback = (
    <div
      className="relative z-10 flex min-w-0 flex-1 items-center justify-center"
      style={{ background: "var(--bg-stage)", color: "var(--text-secondary)" }}
    >
      <RefreshCw className="h-5 w-5 animate-spin opacity-60" />
    </div>
  );

  const utilityModals = (
    <>
      {findUserOpen && (
        <Suspense fallback={null}>
          <FindUserModal
            open={findUserOpen}
            initialPhone={findUserInitialPhone}
            onClose={() => {
              setFindUserOpen(false);
              setFindUserInitialPhone("");
            }}
            onOpenProfile={(profile) => {
              setProfileInitial(profile);
              setProfileUid(profile.uid);
            }}
            onStartChat={handleStartChatFromProfile}
            onRelationshipChanged={handleRelationshipChanged}
          />
        </Suspense>
      )}

      {authed && friendRequestsOpen && (
        <Suspense fallback={null}>
          <FriendRequestsModal
            open={friendRequestsOpen}
            onClose={() => setFriendRequestsOpen(false)}
            onOpenProfile={(uid) => {
              setProfileInitial(null);
              setProfileUid(uid);
            }}
            onStartChat={handleStartChatFromFriendRequest}
            onChanged={(receivedCount) => {
              const prevCount = pendingFriendRequestsCountRef.current;
              setPendingFriendRequestsCount(receivedCount);
              if (receivedCount < prevCount) {
                void refreshThreadsSilently();
              }
            }}
          />
        </Suspense>
      )}

      {folderModalOpen && (
        <Suspense fallback={null}>
          <FolderEditorModal
            open={folderModalOpen}
            folders={customFolders}
            threads={threads}
            initialEditingFolder={editingFolder}
            onClose={() => {
              setFolderModalOpen(false);
              setEditingFolder(null);
            }}
            onSaveFolder={async (targetFolder) => {
              const updated = await bridge.saveChatFolder(targetFolder);
              setCustomFolders(updated);
            }}
            onDeleteFolder={async (folderId) => {
              const updated = await bridge.deleteChatFolder(folderId);
              setCustomFolders(updated);
              if (folder === `custom:${folderId}`) {
                setFolder("all");
              }
            }}
            onReorderFolders={async (orderedIds) => {
              const updated = await bridge.reorderChatFolders(orderedIds);
              setCustomFolders(updated);
            }}
            onSyncRemoteLabels={async () => {
              const updated = await bridge.syncRemoteLabelFolders();
              setCustomFolders(updated);
              return updated.length;
            }}
          />
        </Suspense>
      )}

      {settingsOpen && (
        <Suspense fallback={null}>
          <SettingsModal
            open={settingsOpen}
            initialTab={settingsInitialTab}
            myProfile={myProfile}
            connStatus={connStatus}
            onOpenMyProfile={() => {
              if (myProfile?.uid) setProfileUid(myProfile.uid);
            }}
            onLogout={() => {
              void bridge.logout().then(() => {
                setAuthed(false);
                resetSessionUi();
                void refreshQr();
              });
            }}
            theme={theme}
            onThemeChange={setTheme}
            fontSize={fontSize}
            onFontSizeChange={setFontSize}
            density={density}
            onDensityChange={setDensity}
            fxEnabled={fxEnabled}
            onFxChange={setFxEnabled}
            sendOnEnter={sendOnEnter}
            onSendOnEnterChange={setSendOnEnter}
            notifyPrefs={notifyPrefs}
            onNotifyPrefsChange={setNotifyPrefs}
            onTestSound={() => playMessageSound(true)}
            onTestNotification={() => {
              const sampleThread =
                threads.find((t) => t.id === selectedId) ??
                threads.find((t) => t.kind !== "cloud") ??
                threads[0];
              const sampleMsg: MessageDto = {
                msg_id: `preview-${Date.now()}`,
                thread_id: sampleThread?.id || "cloud-me",
                sender_id: sampleThread?.id || "u-demo",
                sender_name: sampleThread?.title || "Teza",
                text: "Đây là thông báo thử — bấm vào đây để mở hội thoại này.",
                msg_type: "chat.text",
                ts: Date.now(),
                is_outgoing: false,
                is_recalled: false,
                is_pinned: false,
                quote: null,
                reactions: [],
                attachment_url: null,
                attachment_name: null,
              };
              const { title, body } = formatNotificationContent({
                msg: sampleMsg,
                thread: sampleThread,
                previewMode: notifyPrefs.previewMode,
              });
              triggerNotifyPopup({
                thread_id: sampleMsg.thread_id,
                title,
                body,
                avatar: sampleThread?.avatar || "",
                theme,
                position: notifyPrefs.position,
                duration_ms: notifyPrefs.durationSec * 1000,
              });
            }}
            closeToTray={closeToTray}
            onCloseToTrayChange={setCloseToTray}
            customFoldersCount={customFolders.length}
            hiddenBuiltInFolders={hiddenBuiltInFolders}
            onToggleBuiltInFolder={handleToggleBuiltInFolder}
            railDisplayMode={railDisplayMode}
            onRailDisplayModeChange={(mode) => {
              setRailDisplayMode(mode);
              localStorage.setItem("teza.railDisplayMode", mode);
            }}
            onOpenFoldersModal={() => {
              setEditingFolder(null);
              setFolderModalOpen(true);
            }}
            webAppsCfg={webAppsCfg}
            onWebAppsCfgChange={handleWebAppsCfgChange}
            onOpenWebApp={handleActivateWebApp}
            passcodeCfg={passcodeCfg}
            onUpdatePasscode={handleUpdatePasscode}
            onLockNow={() => {
              if (passcodeCfg.enabled) setIsLocked(true);
            }}
            translateCfg={translateCfg}
            onOpenTranslateModal={() => setTranslateModalOpen(true)}
            threads={threads}
            aiConfig={aiConfig}
            onAiConfigChange={setAiConfig}
            onOpenAiChat={handleOpenAiChat}
            relayCfg={relayCfg}
            onRelaySaved={(cfg) => {
              applyRelaySummary(cfg);
              if (!authed) {
                void refreshQr();
              }
            }}
            focusRelay={focusRelaySettings}
            callDevices={callDevices}
            callDevicesLoading={callDevicesLoading}
            onRefreshCallDevices={refreshCallDevices}
            onSelectCallDevices={handleSelectCallDevices}
            appVersion={appVersion}
            availableUpdate={availableUpdate}
            onOpenUpdateModal={() => setUpdateModalOpen(true)}
            onClose={() => {
              setSettingsOpen(false);
              setFocusRelaySettings(false);
            }}
          />
        </Suspense>
      )}

      {updateModalOpen && (
        <Suspense fallback={null}>
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
        </Suspense>
      )}

      {translateModalOpen && (
        <Suspense fallback={null}>
          <TranslateSettingsModal
            open={translateModalOpen}
            config={translateCfg}
            onClose={() => setTranslateModalOpen(false)}
            onSaved={(cfg) => {
              const prevCfg = translateCfgRef.current;
              setTranslateCfg(cfg);
              translateCfgRef.current = cfg;
              const providerChanged = Boolean(prevCfg && prevCfg.provider !== cfg.provider);
              const modeChanged = Boolean(prevCfg && prevCfg.detect_mode !== cfg.detect_mode);
              const langChanged = Boolean(prevCfg && prevCfg.target_lang !== cfg.target_lang);
              if (providerChanged || modeChanged || langChanged) {
                translationCacheRef.current.clear();
                translationsRef.current = {};
                setTranslationsByMsg({});
              }
              if (selectedId && threadAutoTranslate[selectedId]) {
                const nextLang = cfg.target_lang || translateCode;
                setThreadAutoTranslate((prev) => ({
                  ...prev,
                  [selectedId]: nextLang,
                }));
                if ((providerChanged || modeChanged) && !langChanged) {
                  void translateBatchMessages(messages, nextLang, false, providerChanged);
                }
              }
            }}
            onCacheCleared={() => {
              translationCacheRef.current.clear();
              translationsRef.current = {};
              setTranslationsByMsg({});
              if (selectedId && threadAutoTranslate[selectedId]) {
                void translateBatchMessages(messages, threadAutoTranslate[selectedId], false, false);
              }
            }}
          />
        </Suspense>
      )}
      {authed && scheduledModalOpen && (
        <Suspense fallback={null}>
          <ScheduledMessagesModal
            open={scheduledModalOpen}
            threads={threads}
            schedules={schedules}
            loading={schedulesLoading}
            initialThreadId={scheduledInitialThreadId}
            onClose={() => setScheduledModalOpen(false)}
            onRefresh={refreshSchedules}
            onCreate={async (input) => {
              const created = await bridge.scheduleCreate(input);
              await refreshSchedules();
              return created;
            }}
            onUpdate={async (id, input) => {
              const updated = await bridge.scheduleUpdate(id, input);
              await refreshSchedules();
              return updated ?? undefined;
            }}
            onCancel={async (id) => {
              await bridge.scheduleCancel(id);
              await refreshSchedules();
            }}
            onDelete={async (id) => {
              await bridge.scheduleDelete(id);
              await refreshSchedules();
            }}
            onSendNow={async (id) => {
              const updated = await bridge.scheduleSendNow(id);
              await refreshSchedules();
              return updated ?? undefined;
            }}
          />
        </Suspense>
      )}
    </>
  );

  if (authChecking) {
    return (
      <>
        <TriforceEffects enabled={fxEnabled} dustEnabled={fxEnabled && theme === "triforce"} />
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
        <TriforceEffects enabled={fxEnabled} dustEnabled={fxEnabled && theme === "triforce"} />
        <div className="relative z-10 h-full w-full">
          <QrLoginModal
            qrSvg={qrSvg}
            statusText={qrStatus}
            errorText={qrError}
            relayCfg={relayCfg}
            onOpenRelaySettings={() => {
              setFocusRelaySettings(true);
              setSettingsOpen(true);
            }}
            onRefresh={() => void refreshQr()}
            onEnterDemo={() => setAuthed(true)}
          />
          {utilityModals}
        </div>
      </>
    );
  }

  return (
    <div className={`relative flex h-full w-full overflow-hidden ${buzzShaking ? "buzz-shaking" : ""}`}>
      <TriforceEffects enabled={fxEnabled} dustEnabled={fxEnabled && theme === "triforce"} />
      <NavDrawer
        open={navDrawerOpen}
        onClose={() => setNavDrawerOpen(false)}
        myProfile={myProfile}
        connStatus={connStatus}
        theme={theme}
        appVersion={appVersion}
        availableUpdate={availableUpdate}
        archivedCount={archivedThreads.length}
        hasPasscode={passcodeCfg.enabled}
        aiEnabled={Boolean(aiConfig?.enabled)}
        pendingSchedulesCount={pendingSchedulesCount}
        onOpenMyProfile={() => {
          if (myProfile?.uid) setProfileUid(myProfile.uid);
        }}
        onOpenSavedMessages={() => {
          const cloudId = threads.find((t) => t.kind === "cloud")?.id || "cloud-me";
          openThreadById(cloudId);
        }}
        onOpenAiChat={handleOpenAiChat}
        onOpenScheduledMessages={() => {
          setScheduledInitialThreadId(selectedId);
          setScheduledModalOpen(true);
        }}
        onOpenArchived={() => {
          leaveWebApps();
          setFolder("archived");
        }}
        onOpenFindUser={() => {
          setFindUserInitialPhone("");
          setFindUserOpen(true);
        }}
        onOpenFriendRequests={() => setFriendRequestsOpen(true)}
        pendingFriendRequestsCount={pendingFriendRequestsCount}
        onOpenFolders={() => {
          setEditingFolder(null);
          setFolderModalOpen(true);
        }}
        onLockApp={() => {
          if (passcodeCfg.enabled) setIsLocked(true);
        }}
        onOpenSettings={(tab) => {
          setSettingsInitialTab(tab || "notifications");
          setSettingsOpen(true);
        }}
        onCycleTheme={cycleTheme}
        onOpenUpdateModal={() => setUpdateModalOpen(true)}
        onLogout={() => {
          void bridge.logout().then(() => {
            setAuthed(false);
            resetSessionUi();
            void refreshQr();
          });
        }}
      />
      {/* Column 1: Folder Rail */}
      <aside
        className={`tf-rail relative z-10 shrink-0 flex-col overflow-hidden ${
          layout.mode === "single"
            ? mobilePane === "chat"
              ? "hidden"
              : railDisplayMode === "icon-only"
                ? "flex w-[52px]"
                : railDisplayMode === "text-only"
                  ? "flex w-[96px]"
                  : "flex w-[64px]"
            : railDisplayMode === "icon-only"
              ? "flex w-[56px]"
              : railDisplayMode === "text-only"
                ? "flex w-[104px]"
                : "flex w-[78px]"
        }`}
        style={{ borderRight: "1px solid var(--border-subtle)" }}
      >
        <div className="flex w-full shrink-0 flex-col items-center px-1.5 pb-1 pt-2.5">
          <button
            type="button"
            onClick={() => setNavDrawerOpen(true)}
            title={t("menuMainSettings")}
            className="tf-rail-btn relative flex w-full items-center justify-center rounded-xl py-2"
            style={{ color: "var(--rail-text)" }}
          >
            <Menu className="h-5 w-5" />
            {availableUpdate && (
              <span className="absolute right-3 top-1.5 h-2 w-2 rounded-full bg-emerald-400" />
            )}
          </button>
          <div className="mt-1.5 h-px w-8" style={{ background: "var(--border-subtle)" }} />
        </div>

        <div className="flex min-h-0 w-full flex-1 flex-col items-center gap-1 overflow-y-auto px-1.5 py-1.5">
          {visibleBuiltInFolders.map((item) => {
            const Icon = item.icon;
            const active = !activeWebApp && !webAppHubOpen && folder === item.id;
            const badge = item.id === "all" ? 0 : countFolderUnread(threads, item.id, customFolders);
            const itemLabel = t(item.labelKey);
            const isTextOnly = railDisplayMode === "text-only";
            return (
              <button
                key={item.id}
                onClick={() => {
                  leaveWebApps();
                  setFolder(item.id);
                }}
                title={itemLabel}
                className={`tf-rail-btn ${active ? "tf-rail-btn-active" : ""} relative flex w-full shrink-0 rounded-xl ${
                  railDisplayMode === "icon-only"
                    ? "flex-col items-center justify-center py-2.5"
                    : isTextOnly
                      ? "flex-row items-center justify-between gap-1 py-2 pl-3 pr-2 text-left text-[12px] font-medium"
                      : "flex-col items-center justify-center gap-1 py-2 text-xs font-medium"
                }`}
                style={{
                  background: active ? "var(--rail-active-bg)" : undefined,
                  color: active ? "var(--rail-text-active)" : "var(--rail-text)",
                }}
              >
                {!isTextOnly && <Icon className="h-5 w-5 shrink-0" />}
                {railDisplayMode !== "icon-only" && (
                  <span
                    className={
                      isTextOnly
                        ? "min-w-0 flex-1 truncate text-left leading-tight"
                        : "max-w-full truncate px-0.5"
                    }
                  >
                    {itemLabel}
                  </span>
                )}
                {badge > 0 && (
                  <span
                    className={
                      isTextOnly
                        ? "shrink-0 min-w-[18px] rounded-full px-1.5 py-0.5 text-center text-[10px] font-bold leading-none"
                        : "absolute right-1 top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                    }
                    style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                  >
                    {badge > 99 ? "99+" : badge}
                  </span>
                )}
              </button>
            );
          })}

          {customFolders.length > 0 && (
            <div className="my-1 h-px w-8 shrink-0" style={{ background: "var(--border-subtle)" }} />
          )}

          {customFolders.map((cf) => {
            const sel = `custom:${cf.id}`;
            const active = !activeWebApp && !webAppHubOpen && folder === sel;
            const badge = countFolderUnread(threads, sel, customFolders);
            const isTextOnly = railDisplayMode === "text-only";
            return (
              <button
                key={cf.id}
                type="button"
                title={`${cf.name} (Chuột phải để tùy chỉnh)`}
                onClick={() => {
                  leaveWebApps();
                  setFolder(sel);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setFolderMenu({ folder: cf, x: e.clientX, y: e.clientY });
                }}
                className={`tf-rail-btn ${active ? "tf-rail-btn-active" : ""} relative flex w-full shrink-0 rounded-xl ${
                  railDisplayMode === "icon-only"
                    ? "flex-col items-center justify-center py-2.5"
                    : isTextOnly
                      ? "flex-row items-center justify-between gap-1 py-2 pl-3 pr-2 text-left text-[12px] font-medium"
                      : "flex-col items-center justify-center gap-1 py-2 text-xs font-medium"
                }`}
                style={{
                  background: active ? "var(--rail-active-bg)" : undefined,
                  color: active ? "var(--rail-text-active)" : "var(--rail-text)",
                }}
              >
                {!isTextOnly && renderFolderIcon(cf.icon, "h-5 w-5 shrink-0")}
                {railDisplayMode !== "icon-only" && (
                  <span
                    className={
                      isTextOnly
                        ? "min-w-0 flex-1 truncate text-left leading-tight"
                        : "max-w-full truncate px-0.5"
                    }
                  >
                    {cf.name}
                  </span>
                )}
                {badge > 0 && (
                  <span
                    className={
                      isTextOnly
                        ? "shrink-0 min-w-[18px] rounded-full px-1.5 py-0.5 text-center text-[10px] font-bold leading-none"
                        : "absolute right-1 top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                    }
                    style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                  >
                    {badge > 99 ? "99+" : badge}
                  </span>
                )}
              </button>
            );
          })}

          <button
            type="button"
            onClick={() => {
              setEditingFolder(null);
              setFolderModalOpen(true);
            }}
            title={t("manageFoldersTitle")}
            className={`tf-rail-btn mt-0.5 flex w-full shrink-0 rounded-xl font-medium ${
              railDisplayMode === "icon-only"
                ? "flex-col items-center justify-center py-2.5"
                : railDisplayMode === "text-only"
                  ? "flex-row items-center justify-start py-2 pl-3 pr-2 text-left text-[11.5px]"
                  : "flex-col items-center justify-center gap-1 py-2 text-[11px]"
            }`}
            style={{ color: "var(--rail-text)" }}
          >
            {railDisplayMode !== "text-only" && <FolderPlus className="h-4 w-4 shrink-0" />}
            {railDisplayMode !== "icon-only" && (
              <span className={railDisplayMode === "text-only" ? "truncate text-left" : "truncate"}>
                {railDisplayMode === "text-only" ? `+ ${t("manageFolders")}` : t("manageFolders")}
              </span>
            )}
          </button>

          {webAppsCfg.enabled && (
            <>
              <div className="my-1 h-px w-8 shrink-0" style={{ background: "var(--border-subtle)" }} />
              {(() => {
                const active = webAppHubOpen || Boolean(activeWebApp);
                const totalApps = webAppsCfg.items.length;
                const isTextOnly = railDisplayMode === "text-only";
                return (
                  <button
                    type="button"
                    title={t("webappHubTitle")}
                    onClick={() => {
                      if (activeWebApp) {
                        void bridge.webappHideAll();
                      }
                      setActiveWebAppId(null);
                      setWebAppHubOpen(true);
                      if (layout.mode === "single") setMobilePane("chat");
                    }}
                    className={`tf-rail-btn ${active ? "tf-rail-btn-active" : ""} relative flex w-full shrink-0 rounded-xl ${
                      railDisplayMode === "icon-only"
                        ? "flex-col items-center justify-center py-2.5"
                        : isTextOnly
                          ? "flex-row items-center justify-between gap-1 py-2 pl-3 pr-2 text-left text-[12px] font-medium"
                          : "flex-col items-center justify-center gap-1 py-2 text-xs font-medium"
                    }`}
                    style={{
                      background: active ? "var(--rail-active-bg)" : undefined,
                      color: active ? "var(--rail-text-active)" : "var(--rail-text)",
                    }}
                  >
                    {!isTextOnly && <LayoutGrid className="h-5 w-5 shrink-0" />}
                    {railDisplayMode !== "icon-only" && (
                      <span
                        className={
                          isTextOnly
                            ? "min-w-0 flex-1 truncate text-left leading-tight"
                            : "max-w-full truncate px-0.5"
                        }
                      >
                        {t("webappMenuLabel")}
                      </span>
                    )}
                    {totalApps > 0 && (
                      <span
                        className={
                          isTextOnly
                            ? "shrink-0 min-w-[18px] rounded-full px-1.5 py-0.5 text-center text-[10px] font-bold leading-none"
                            : "absolute right-1 top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                        }
                        style={{
                          background: active ? "var(--accent)" : "var(--bg-surface)",
                          color: active ? "var(--text-on-accent)" : "var(--text-secondary)",
                        }}
                      >
                        {totalApps}
                      </span>
                    )}
                  </button>
                );
              })()}
            </>
          )}
        </div>

        <div
          className="flex w-full shrink-0 flex-col items-center gap-1.5 px-1.5 py-2.5"
          style={{ borderTop: "1px solid var(--border-subtle)" }}
        >
          {passcodeCfg.enabled && (
            <button
              type="button"
              onClick={() => setIsLocked(true)}
              title={t("lockAppShortcut")}
              className="tf-rail-btn flex w-full items-center justify-center rounded-lg py-2"
              style={{ color: "var(--rail-text)" }}
            >
              <Lock className="h-5 w-5" />
            </button>
          )}
          <button
            type="button"
            onClick={() => {
              setSettingsInitialTab("notifications");
              setSettingsOpen(true);
            }}
            title={
              availableUpdate
                ? t("settingsUpdateHint", { version: availableUpdate.version })
                : t("settingsTeza")
            }
            className="tf-rail-btn relative flex w-full items-center justify-center rounded-lg py-2"
            style={{ color: availableUpdate ? "var(--accent)" : "var(--rail-text)" }}
          >
            <Settings className="h-5 w-5" />
            {availableUpdate && (
              <span className="absolute right-2.5 top-1.5 h-2 w-2 rounded-full bg-emerald-400" />
            )}
          </button>
        </div>
      </aside>

      {activeWebApp ? (
        <Suspense fallback={webAppStageFallback}>
          <WebAppWorkspace
            app={activeWebApp}
            suspended={webappSuspended}
            allEmbeddedApps={embeddedWebApps}
            onSelectApp={handleActivateWebApp}
            onBackToHub={() => {
              void bridge.webappHideAll();
              setActiveWebAppId(null);
              setWebAppHubOpen(true);
            }}
            onCloseToChat={leaveWebApps}
            onOpenExternal={(url) => void bridge.openExternalUrl(url).catch(showError)}
            onError={showError}
          />
        </Suspense>
      ) : webAppsCfg.enabled && webAppHubOpen ? (
        <Suspense fallback={webAppStageFallback}>
          <WebAppHub
            config={webAppsCfg}
            lastActiveId={lastOpenedWebAppId}
            onChangeConfig={handleWebAppsCfgChange}
            onLaunch={handleActivateWebApp}
            onOpenExternal={(url) => void bridge.openExternalUrl(url).catch(showError)}
            onCloseToChat={() => setWebAppHubOpen(false)}
          />
        </Suspense>
      ) : (
        <>
      {/* Column 2: Resizable / Auto-Collapsing Thread List */}
      <section
        className={`relative z-10 shrink-0 flex-col ${
          layout.mode === "single"
            ? mobilePane === "chat"
              ? "hidden"
              : "flex min-w-0 flex-1"
            : "flex"
        }`}
        style={{
          width:
            layout.mode === "single"
              ? undefined
              : layout.mode === "compact"
                ? "72px"
                : `${sidebarWidth}px`,
          background: "var(--bg-sidebar)",
          borderRight: layout.mode === "single" ? undefined : "1px solid var(--border-subtle)",
        }}
      >
        {layout.mode === "compact" ? (
          <div className="flex flex-col items-center gap-1.5 px-2 py-2.5">
            {windowWidth >= 900 && (
              <button
                type="button"
                onClick={() => {
                  setManualSidebarCompact(false);
                  localStorage.setItem("litechat.sidebarCompact", "0");
                }}
                title={t("expandListTitle")}
                className="hover-menu flex h-9 w-9 items-center justify-center rounded-xl transition"
                style={{ background: "var(--bg-surface)", color: "var(--accent)" }}
              >
                <PanelLeftOpen className="h-4 w-4" />
              </button>
            )}
            <button
              type="button"
              onClick={() => setQuickSearchOpen(true)}
              title={t("globalSearchTitle")}
              className="hover-menu flex h-9 w-9 items-center justify-center rounded-xl transition"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <Search className="h-4 w-4" />
            </button>
            <button
              type="button"
              onClick={() => {
                setFindUserInitialPhone("");
                setFindUserOpen(true);
              }}
              title={t("findFriendByPhoneTitle")}
              className="hover-menu flex h-9 w-9 items-center justify-center rounded-xl transition"
              style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
            >
              <UserPlus className="h-4 w-4" />
            </button>
            <button
              type="button"
              onClick={() => setFriendRequestsOpen(true)}
              title={t("friendRequestsMenu")}
              className="hover-menu relative flex h-9 w-9 items-center justify-center rounded-xl transition"
              style={{
                background: "var(--bg-surface)",
                color: pendingFriendRequestsCount > 0 ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              <UserCheck className="h-4 w-4" />
              {pendingFriendRequestsCount > 0 && (
                <span
                  className="absolute -right-1 -top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                  style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                >
                  {pendingFriendRequestsCount > 99 ? "99+" : pendingFriendRequestsCount}
                </span>
              )}
            </button>
          </div>
        ) : (
          <div className="p-3 pb-2">
            <div className="flex items-center gap-1.5">
              <div
                className="flex min-w-0 flex-1 items-center gap-2 rounded-full px-3 py-1.5"
                style={{ background: "var(--bg-surface)" }}
              >
                <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
                <input
                  value={filterText}
                  onChange={(e) => setFilterText(e.target.value)}
                  onKeyDown={(e) => {
                    if (isImeComposingEvent(e)) return;
                    if (e.key === "Escape" && filterText.length > 0) {
                      e.preventDefault();
                      e.stopPropagation();
                      setFilterText("");
                    } else if (
                      e.key === "Enter" &&
                      visibleThreads.length === 0 &&
                      looksLikePhoneQuery(filterText)
                    ) {
                      e.preventDefault();
                      setFindUserInitialPhone(filterText.trim());
                      setFindUserOpen(true);
                    }
                  }}
                  placeholder={t("searchThreadPlaceholder")}
                  className="w-full min-w-0 bg-transparent text-[15px] outline-none"
                />
                {filterText.length > 0 && (
                  <button
                    type="button"
                    onClick={() => setFilterText("")}
                    title={t("close")}
                    aria-label={t("close")}
                    className="hover-menu shrink-0 rounded-full p-0.5 transition"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                )}
              </div>
              <button
                type="button"
                onClick={() => {
                  setFindUserInitialPhone(looksLikePhoneQuery(filterText) ? filterText.trim() : "");
                  setFindUserOpen(true);
                }}
                title={t("findFriendByPhoneTitle")}
                className="hover-menu shrink-0 rounded-lg p-2 transition"
                style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
              >
                <UserPlus className="h-4 w-4" />
              </button>
              <button
                type="button"
                onClick={() => setFriendRequestsOpen(true)}
                title={t("friendRequestsMenu")}
                className="hover-menu relative shrink-0 rounded-lg p-2 transition"
                style={{
                  background: "var(--bg-surface)",
                  color: pendingFriendRequestsCount > 0 ? "var(--accent)" : "var(--text-secondary)",
                }}
              >
                <UserCheck className="h-4 w-4" />
                {pendingFriendRequestsCount > 0 && (
                  <span
                    className="absolute -right-1 -top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                    style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                  >
                    {pendingFriendRequestsCount > 99 ? "99+" : pendingFriendRequestsCount}
                  </span>
                )}
              </button>
              <button
                type="button"
                onClick={() => void syncRemoteThreads()}
                title={t("syncThreadsTitle")}
                className="hover-menu shrink-0 rounded-lg p-2 transition"
                style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
              >
                <RefreshCw className={`h-4 w-4 ${syncing ? "animate-spin" : ""}`} />
              </button>
              <button
                type="button"
                onClick={() => setQuickSearchOpen(true)}
                title={t("globalSearchTitle")}
                className="hover-menu shrink-0 rounded-lg px-2 py-1.5 text-xs font-medium transition"
                style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
              >
                Ctrl+K
              </button>
              {windowWidth >= 900 && (
                <button
                  type="button"
                  onClick={() => {
                    setManualSidebarCompact(true);
                    localStorage.setItem("litechat.sidebarCompact", "1");
                  }}
                  title={t("collapseListTitle")}
                  className="hover-menu shrink-0 rounded-lg p-2 transition"
                  style={{ background: "var(--bg-surface)", color: "var(--text-secondary)" }}
                >
                  <PanelLeftClose className="h-4 w-4" />
                </button>
              )}
            </div>
            {looksLikePhoneQuery(filterText) && (
              <button
                type="button"
                onClick={() => {
                  setFindUserInitialPhone(filterText.trim());
                  setFindUserOpen(true);
                }}
                className="hover-menu mt-2 flex w-full items-center gap-2 rounded-xl px-3 py-1.5 text-left text-xs font-medium transition"
                style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
              >
                <UserPlus className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate">
                  {t("findFriendByPhonePrompt", { query: filterText.trim() })}
                </span>
              </button>
            )}
          </div>
        )}

        <div
          ref={sidebarListRef}
          className={`flex-1 overflow-y-auto pb-2 ${layout.mode === "compact" ? "px-1.5" : "px-2"}`}
          onScroll={(e) => {
            const el = e.currentTarget;
            if (
              visibleThreads.length > threadRenderLimit &&
              el.scrollHeight - el.scrollTop - el.clientHeight < 240
            ) {
              setThreadRenderLimit((prev) => prev + 100);
            }
          }}
        >
          {folder === "archived" && (
            <div
              className="mb-2 flex items-center justify-between gap-2 rounded-xl px-3 py-2 text-xs"
              style={{
                background: "var(--bg-surface)",
                border: "1px solid var(--border-subtle)",
              }}
            >
              {layout.mode === "compact" ? (
                <button
                  type="button"
                  onClick={() => setFolder("all")}
                  title={t("backToAllThreads")}
                  className="flex w-full items-center justify-center py-0.5"
                  style={{ color: "var(--accent)" }}
                >
                  <ArrowLeft className="h-4 w-4" />
                </button>
              ) : (
                <>
                  <div className="flex min-w-0 items-center gap-2 font-medium">
                    <Archive className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                    <span className="truncate">
                      {t("archiveBannerTitle", { count: archivedThreads.length })}
                    </span>
                  </div>
                  <button
                    type="button"
                    onClick={() => setFolder("all")}
                    className="shrink-0 rounded-lg px-2 py-1 font-medium transition hover:opacity-85"
                    style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                  >
                    {t("back")}
                  </button>
                </>
              )}
            </div>
          )}

          {folder === "all" && !filterText.trim() && archivedThreads.length > 0 && (
            layout.mode === "compact" ? (
              <button
                type="button"
                onClick={() => setFolder("archived")}
                title={`${t("archiveRowTitle")} (${archivedThreads.length})`}
                className="hover-surface relative mb-1.5 flex w-full items-center justify-center rounded-xl py-2.5 transition"
                style={{ color: "var(--text-secondary)" }}
              >
                <Archive className="h-5 w-5" />
                {archivedUnread > 0 && (
                  <span
                    className="absolute right-2 top-1 min-w-[16px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none"
                    style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                  >
                    {archivedUnread > 99 ? "99+" : archivedUnread}
                  </span>
                )}
              </button>
            ) : (
              <button
                type="button"
                onClick={() => setFolder("archived")}
                className="hover-surface mb-1 flex w-full items-center gap-3 rounded-xl px-2.5 py-2.5 text-left transition"
              >
                <div
                  className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full"
                  style={{
                    background: "var(--bg-surface)",
                    color: "var(--accent)",
                    border: "1px solid var(--border-subtle)",
                  }}
                >
                  <Archive className="h-5 w-5" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-[14.5px] font-semibold">
                      {t("archiveRowTitle")}
                    </span>
                    <span className="shrink-0 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                      {archivedThreads.length}
                    </span>
                  </div>
                  <div className="mt-0.5 flex items-center justify-between gap-2">
                    <span
                      className="truncate text-[12.5px]"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {archivedThreads
                        .slice(0, 3)
                        .map((t) => t.title)
                        .join(", ")}
                    </span>
                    {archivedUnread > 0 && (
                      <span
                        className="shrink-0 rounded-full px-1.5 py-0.5 text-[10.5px] font-bold leading-none"
                        style={{ background: "var(--accent)", color: "var(--text-on-accent)" }}
                      >
                        {archivedUnread > 99 ? "99+" : archivedUnread}
                      </span>
                    )}
                  </div>
                </div>
              </button>
            )
          )}

          {visibleThreads.length === 0 ? (
            <div className="px-4 py-12 text-center text-[13px]" style={{ color: "var(--text-secondary)" }}>
              {layout.mode === "compact"
                ? t("emptyCompact")
                : filterText.trim()
                  ? t("emptySearch")
                  : folder === "archived"
                    ? t("emptyArchived")
                    : folder === "unread"
                      ? t("emptyUnread")
                      : t("emptyThreads")}
            </div>
          ) : (
            visibleThreads.slice(0, threadRenderLimit).map((thread) => {
              const active = thread.id === selectedId;
              const unread = thread.unread_count > 0;
              const draftText = draftsByThread[thread.id]?.trim() || "";
              const fallbackPreview =
                thread.kind === "group"
                  ? t("fallbackGroupPreview")
                  : thread.kind === "cloud"
                    ? t("fallbackCloudPreview")
                    : t("fallbackDirectPreview");
              if (layout.mode === "compact") {
                return (
                  <button
                    key={thread.id}
                    data-thread-row={thread.id}
                    type="button"
                    title={`${thread.title}${thread.last_preview ? ` — ${thread.last_preview}` : ""}`}
                    onClick={() => {
                      selectAndRevealThread(thread.id);
                      if (layout.mode === "single") setMobilePane("chat");
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setThreadMenu({ thread, x: e.clientX, y: e.clientY });
                    }}
                    className={`mb-1 flex w-full items-center justify-center rounded-xl py-2 transition ${
                      active ? "" : "hover-surface"
                    }`}
                    style={{
                      background: active ? "var(--bg-active)" : undefined,
                      color: active ? "var(--text-on-active)" : "var(--text-primary)",
                    }}
                  >
                    <div className="relative shrink-0">
                      <ThreadAvatar avatar={thread.avatar} title={thread.title} kind={thread.kind} size="md" />
                      {unread && (
                        <span
                          className="absolute -right-1 -top-1 min-w-[18px] rounded-full px-1 py-0.5 text-center text-[10px] font-bold leading-none shadow"
                          style={{
                            background: thread.is_muted ? "var(--chip-bg)" : "var(--accent)",
                            color: thread.is_muted ? "var(--text-secondary)" : "var(--text-on-accent)",
                          }}
                        >
                          {thread.unread_count > 99 ? "99+" : thread.unread_count}
                        </span>
                      )}
                      {thread.is_pinned && (
                        <span
                          className="absolute -bottom-0.5 -right-0.5 flex h-3.5 w-3.5 items-center justify-center rounded-full shadow"
                          style={{
                            background: "var(--bg-sidebar)",
                            color: "var(--accent)",
                          }}
                        >
                          <Pin className="h-2.5 w-2.5" />
                        </span>
                      )}
                    </div>
                  </button>
                );
              }
              return (
                <div
                  key={thread.id}
                  data-thread-row={thread.id}
                  role="button"
                  tabIndex={0}
                  onClick={() => {
                    selectAndRevealThread(thread.id);
                    if (layout.mode === "single") setMobilePane("chat");
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      selectAndRevealThread(thread.id);
                      if (layout.mode === "single") setMobilePane("chat");
                    }
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setThreadMenu({ thread, x: e.clientX, y: e.clientY });
                  }}
                  className={`tf-row thread-row-perf mb-0.5 flex w-full cursor-pointer items-center gap-3 rounded-xl px-2.5 text-left transition ${
                    active ? "" : "hover-surface"
                  }`}
                  style={{
                    paddingBlock: "var(--row-py)",
                    background: active ? "var(--bg-active)" : undefined,
                    color: active ? "var(--text-on-active)" : "var(--text-primary)",
                  }}
                >
                  {thread.kind === "direct" ? (
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        setProfileUid(thread.id);
                      }}
                      onKeyDown={(e) => e.stopPropagation()}
                      title={t("lookupProfile")}
                      className="shrink-0 rounded-full transition hover:opacity-85"
                    >
                      <ThreadAvatar avatar={thread.avatar} title={thread.title} kind={thread.kind} size="md" />
                    </button>
                  ) : (
                    <ThreadAvatar avatar={thread.avatar} title={thread.title} kind={thread.kind} size="md" />
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2">
                      <span
                        className={`list-title-text truncate ${unread ? "font-bold" : "font-semibold"}`}
                        style={{
                          color: active
                            ? "var(--text-on-active)"
                            : unread
                              ? "var(--text-primary)"
                              : "var(--list-title)",
                        }}
                      >
                        {thread.title}
                      </span>
                      <div
                        className="flex shrink-0 items-center gap-1 text-xs"
                        style={{
                          color: active ? "var(--text-secondary-on-active)" : "var(--text-secondary)",
                        }}
                      >
                        {thread.is_muted && <BellOff className="h-3 w-3 opacity-75" />}
                        {thread.is_pinned && <Pin className="h-3 w-3 opacity-80" />}
                        {thread.last_ts > 0 && thread.last_preview.startsWith("Bạn:") && !unread && (
                          thread.kind === "cloud" || (thread.peer_read_ts ?? 0) >= thread.last_ts ? (
                            <span title={t("youSeenStatus")} className="inline-flex items-center">
                              <CheckCheck
                                className="h-3.5 w-3.5"
                                style={{ color: active ? "var(--text-on-active)" : "var(--accent)" }}
                              />
                            </span>
                          ) : (
                            <span title={t("youSentStatus")} className="inline-flex items-center">
                              <Check className="h-3 w-3 opacity-80" />
                            </span>
                          )
                        )}
                        {thread.last_ts > 0 && <span>{formatThreadTime(thread.last_ts, lang)}</span>}
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
                        {draftText && !active ? (
                          <>
                            <span style={{ color: "var(--danger-text)", fontWeight: 600 }}>
                              {t("draftPrefix")}{" "}
                            </span>
                            {draftText}
                          </>
                        ) : (
                          thread.last_preview || fallbackPreview
                        )}
                      </span>
                      {unread && (
                        <span
                          className="shrink-0 rounded-full px-1.5 py-0.5 text-xs font-bold"
                          style={{
                            background: active
                              ? "rgba(255,255,255,0.2)"
                              : thread.is_muted
                                ? "var(--chip-bg)"
                                : "var(--accent)",
                            color: active
                              ? "var(--text-on-active)"
                              : thread.is_muted
                                ? "var(--text-secondary)"
                                : "var(--text-on-accent)",
                          }}
                        >
                          {thread.unread_count > 99 ? "99+" : thread.unread_count}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
              );
            })
          )}
          {visibleThreads.length > threadRenderLimit && layout.mode !== "compact" && (
            <button
              type="button"
              onClick={() => setThreadRenderLimit((prev) => prev + 100)}
              className="hover-surface mt-1 w-full rounded-xl py-2 text-center text-xs font-medium transition"
              style={{ color: "var(--accent)" }}
            >
              {t("loadMoreThreads", { count: visibleThreads.length - threadRenderLimit })}
            </button>
          )}
        </div>
      </section>

      {layout.mode === "full" && (
        <div
          role="separator"
          aria-orientation="vertical"
          title={t("resizeThreadsTitle")}
          onPointerDown={(e) => {
            e.preventDefault();
            sidebarDragCleanupRef.current?.();
            const startX = e.clientX;
            const startW = sidebarWidth;
            const onMove = (ev: PointerEvent) => {
              const rawW = startW + (ev.clientX - startX);
              if (rawW < 160) return;
              const next = Math.min(440, Math.max(240, rawW));
              setSidebarWidth(next);
            };
            const cleanup = () => {
              window.removeEventListener("pointermove", onMove);
              window.removeEventListener("pointerup", onUp);
              window.removeEventListener("pointercancel", onCancel);
              if (sidebarDragCleanupRef.current === cleanup) {
                sidebarDragCleanupRef.current = null;
              }
            };
            const onUp = (ev: PointerEvent) => {
              const rawW = startW + (ev.clientX - startX);
              if (rawW < 160 && windowWidth >= 900) {
                setManualSidebarCompact(true);
                localStorage.setItem("litechat.sidebarCompact", "1");
              } else {
                const finalW = Math.min(440, Math.max(240, rawW));
                setSidebarWidth(finalW);
                localStorage.setItem("litechat.sidebarCompact", "0");
                localStorage.setItem("litechat.sidebarWidth", String(finalW));
              }
              cleanup();
            };
            const onCancel = () => {
              cleanup();
            };
            sidebarDragCleanupRef.current = cleanup;
            window.addEventListener("pointermove", onMove);
            window.addEventListener("pointerup", onUp);
            window.addEventListener("pointercancel", onCancel);
          }}
          className="relative z-20 w-1.5 shrink-0 cursor-col-resize transition-colors hover:bg-sky-500/35"
          style={{ background: "var(--bg-sidebar)", borderRight: "1px solid var(--border-subtle)" }}
        />
      )}

      {/* Column 3: Chat Stage */}
      <main
        className={`relative z-10 min-w-0 flex-1 flex-col ${
          layout.mode === "single" && mobilePane === "list" ? "hidden" : "flex"
        }`}
      >
        {buzzToast && (
          <div
            className="absolute left-1/2 top-16 z-40 -translate-x-1/2"
            onMouseEnter={() => setBuzzToastHovered(true)}
            onMouseLeave={() => setBuzzToastHovered(false)}
          >
            <div
              key={buzzToast.nonce}
              className={`${
                buzzToastHovered ? "" : "buzz-shaking"
              } flex items-center gap-1 rounded-full bg-amber-500 pl-4 pr-2 py-1.5 text-[13px] font-bold text-slate-950 shadow-2xl transition`}
            >
              <button
                type="button"
                onClick={() => {
                  const targetThreadId = buzzToast.threadId;
                  dismissBuzzToast();
                  selectAndRevealThread(targetThreadId, { forceScroll: true });
                  setMobilePane("chat");
                  setActiveThreadRootId(null);
                }}
                className="flex items-center gap-2 py-0.5 text-left transition hover:opacity-85"
              >
                <Zap className="h-4 w-4 shrink-0 fill-slate-950" />
                <span>
                  {t("buzzSentByUser", { name: buzzToast.senderName })}
                  {buzzToast.streak >= 2 ? ` (×${buzzToast.streak})` : ""} ({buzzToast.threadTitle})
                </span>
              </button>
              <button
                type="button"
                onMouseDown={(e) => {
                  if (e.button !== 0) return;
                  e.preventDefault();
                  e.stopPropagation();
                  dismissBuzzToast();
                }}
                onClick={(e) => {
                  e.stopPropagation();
                  dismissBuzzToast();
                }}
                title={t("notifyCloseTitle")}
                aria-label={t("notifyCloseTitle")}
                className="ml-1 inline-flex shrink-0 items-center justify-center rounded-full p-1 text-slate-950/80 transition hover:bg-slate-950/15 hover:text-slate-950"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
        )}

        {dragHover && activeThread && !aiChatOpen && (
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

        {aiChatOpen ? (
          <Suspense
            fallback={
              <div
                className="flex flex-1 items-center justify-center gap-2 text-sm"
                style={{ color: "var(--text-secondary)" }}
              >
                <Loader2 className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
                <span>{t("loadingPanelLabel")}</span>
              </div>
            }
          >
            <AiChatPanel
              config={aiConfig}
              messages={aiMessages}
              streamingText={aiStreamingText}
              isStreaming={aiIsStreaming}
              error={aiError}
              onSend={(prompt) => void handleAiSend(prompt)}
              onClearHistory={() => void handleAiClearHistory()}
              onOpenSettings={() => {
                setSettingsInitialTab("ai");
                setSettingsOpen(true);
              }}
              onClose={() => {
                setAiChatOpen(false);
                if (layout.mode === "single") setMobilePane("list");
              }}
            />
          </Suspense>
        ) : activeThread ? (
          activeThreadRootMsg ? (
            <ReplyThreadPanel
              threadTitle={activeThread.title}
              rootMessage={activeThreadRootMsg}
              replies={replyGraph.repliesByRoot[activeThreadRootMsg.msg_id] ?? []}
              threadKind={activeThread.kind}
              peerReadTs={activeThread.peer_read_ts}
              pendingAttachment={pendingAttachment}
              onPendingAttachmentChange={updatePendingAttachment}
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
              sendOnEnter={sendOnEnter}
              hasBlockingOverlay={hasBlockingOverlay}
              getSenderAvatar={(uid) => avatarByUserId.get(uid)}
              onSaveFile={handleSaveRemoteFile}
              onRetrySend={handleRetryFailedMessage}
              onDiscardFailed={handleDiscardFailedMessage}
              onReact={handleReactMessage}
              onRecall={handleRecallMessage}
              onDeleteLocal={handleDeleteLocalMessage}
              onOpenImage={openLightboxForUrl}
              onOpenProfile={(uid) => setProfileUid(uid)}
              onForward={(target) => setForwardMessages([target])}
              onTranslate={handleTranslateSingle}
              onHideTranslation={(msgId) =>
                setTranslationsByMsg((prev) => {
                  const next = { ...prev };
                  delete next[msgId];
                  return next;
                })
              }
              mentionMembers={composerMentionMembers}
              displayMentionMembers={messageMentionCandidates}
              onSendSticker={handleSendSticker}
              onTogglePin={handleTogglePinMessage}
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
                {layout.mode === "single" && (
                  <button
                    type="button"
                    onClick={() => setMobilePane("list")}
                    title="Quay lại danh sách hội thoại"
                    className="hover-menu -ml-1.5 rounded-lg p-1.5 transition"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <ArrowLeft className="h-5 w-5" />
                  </button>
                )}
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
                      ? t("groupChatLabel")
                      : activeThread.kind === "cloud"
                        ? t("myCloudShort")
                        : t("directChatLabel")}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                {activeThread.kind === "direct" &&
                  activeThread.id !== "cloud-me" &&
                  activeThread.id !== myProfile?.uid && (
                    <>
                      <button
                        type="button"
                        disabled={!activeCallCheck.allowed || callActionBusy}
                        onClick={() => void handleStartVoiceCall(activeThread.id, activeThread.title)}
                        title={
                          activeCallCheck.disabledReason === "relay_enabled"
                            ? t("callDisabledRelayTooltip")
                            : activeCallCheck.disabledReason === "busy"
                              ? t("callAlreadyActiveTooltip")
                              : t("callVoiceTitle")
                        }
                        className="rounded-lg p-2 transition hover:opacity-80 disabled:cursor-not-allowed disabled:opacity-40"
                        style={{
                          color: activeCallCheck.allowed
                            ? "var(--accent)"
                            : "var(--text-secondary)",
                          background: activeCallCheck.allowed
                            ? "var(--accent-soft)"
                            : "transparent",
                        }}
                      >
                        <Phone className="h-4 w-4" />
                      </button>
                      <button
                        type="button"
                        disabled={!activeCallCheck.allowed || callActionBusy}
                        onClick={() => void handleStartVideoCall(activeThread.id, activeThread.title)}
                        title={
                          activeCallCheck.disabledReason === "relay_enabled"
                            ? t("callDisabledRelayTooltip")
                            : activeCallCheck.disabledReason === "busy"
                              ? t("callAlreadyActiveTooltip")
                              : t("callVideoTitle")
                        }
                        aria-label={t("callVideoTitle")}
                        className="rounded-lg p-2 transition hover:opacity-80 disabled:cursor-not-allowed disabled:opacity-40"
                        style={{
                          color: activeCallCheck.allowed
                            ? "var(--accent)"
                            : "var(--text-secondary)",
                          background: activeCallCheck.allowed
                            ? "var(--accent-soft)"
                            : "transparent",
                        }}
                      >
                        <Video className="h-4 w-4" />
                      </button>
                    </>
                  )}
                {aiConfig?.enabled && (
                  <button
                    type="button"
                    disabled={Boolean(aiSummaryByThread[activeThread.id]?.loading)}
                    onClick={() => void handleSummarizeUnread(activeThread.id)}
                    title={t("aiSummarizeUnreadBtn")}
                    className="flex items-center gap-1.5 rounded-lg px-2 py-1.5 text-xs font-medium transition hover:opacity-85 disabled:opacity-60"
                    style={{
                      color:
                        unreadDividerCount > 0 || Boolean(aiSummaryByThread[activeThread.id])
                          ? "var(--accent)"
                          : "var(--text-secondary)",
                      background:
                        unreadDividerCount > 0 || Boolean(aiSummaryByThread[activeThread.id])
                          ? "var(--accent-soft)"
                          : "transparent",
                    }}
                  >
                    {aiSummaryByThread[activeThread.id]?.loading ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <Sparkles className="h-4 w-4" />
                    )}
                    {unreadDividerCount > 0 && (
                      <span className="hidden sm:inline">{t("aiSummarizeUnreadBtn")}</span>
                    )}
                  </button>
                )}
                <button
                  onClick={handleToggleThreadTranslate}
                  title={
                    activeThreadLang
                      ? t("autoTranslateOn", { lang: activeThreadLang })
                      : t("autoTranslateOff", { lang: translateCode })
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
                  onClick={() => {
                    setInChatSearchOpen((v) => {
                      const next = !v;
                      if (next) setTimeout(() => inChatSearchInputRef.current?.focus(), 20);
                      return next;
                    });
                  }}
                  title={t("searchInChatTitle")}
                  className="rounded-lg p-2 hover:opacity-80"
                  style={{
                    color: inChatSearchOpen ? "var(--accent)" : "var(--text-secondary)",
                    background: inChatSearchOpen ? "var(--accent-soft)" : "transparent",
                  }}
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
                  title={t("threadInfoTitle")}
                  className="rounded-lg p-2 hover:opacity-80"
                  style={{
                    color: drawerOpen && !activeThreadRootId ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <Info className="h-4 w-4" />
                </button>
              </div>
            </header>

            {currentPinned && (
              <div
                className="relative shrink-0 text-[13px]"
                style={{
                  background: "var(--bg-surface)",
                  borderBottom: "1px solid var(--border-subtle)",
                }}
              >
                <div className="flex items-center justify-between gap-3 px-4 py-2">
                  <button
                    type="button"
                    onClick={() => void jumpToMsg(currentPinned.msg_id)}
                    className="flex min-w-0 flex-1 items-center gap-2.5 text-left transition hover:opacity-85"
                  >
                    <Pin
                      className="h-4 w-4 shrink-0 rotate-45"
                      style={{ color: "var(--accent)" }}
                    />
                    <div
                      className="min-w-0 flex-1 border-l-2 pl-2"
                      style={{ borderColor: "var(--accent)" }}
                    >
                      <div
                        className="flex items-center gap-1.5 text-xs font-semibold"
                        style={{ color: "var(--accent)" }}
                      >
                        <span>{t("pinnedBadgeTitle")}</span>
                        {pinnedMessages.length > 1 && (
                          <span>
                            ({(pinnedIdx % pinnedMessages.length) + 1}/{pinnedMessages.length})
                          </span>
                        )}
                      </div>
                      <div className="truncate" style={{ color: "var(--text-primary)" }}>
                        {currentPinned.sender_name ? `${currentPinned.sender_name}: ` : ""}
                        {currentPinned.text || currentPinned.attachment_name || t("attachmentFallbackLabel")}
                      </div>
                    </div>
                  </button>
                  <div className="flex shrink-0 items-center gap-1">
                    {pinnedMessages.length > 1 && (
                      <button
                        type="button"
                        onClick={() => setPinnedIdx((i) => (i + 1) % pinnedMessages.length)}
                        title={t("forward")}
                        className="hover-menu rounded-lg p-1.5"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <ChevronDown className="h-4 w-4" />
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => setPinnedListOpen((v) => !v)}
                      title={
                        pinnedListOpen
                          ? t("close")
                          : `${t("folderAll")} (${pinnedMessages.length})`
                      }
                      className="hover-menu rounded-lg px-2 py-1 text-xs font-medium"
                      style={{ color: "var(--accent)" }}
                    >
                      {pinnedListOpen ? t("close") : `${t("folderAll")} (${pinnedMessages.length})`}
                    </button>
                    <button
                      type="button"
                      onClick={() => handleTogglePinMessage(currentPinned)}
                      title={t("unpinThisMsg")}
                      className="hover-menu rounded-lg p-1.5"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <X className="h-4 w-4" />
                    </button>
                  </div>
                </div>

                {pinnedListOpen && (
                  <div
                    className="absolute inset-x-0 top-full z-30 max-h-64 overflow-y-auto px-3 py-2 shadow-2xl"
                    style={{
                      background: "var(--bg-sidebar)",
                      borderBottom: "1px solid var(--border-subtle)",
                    }}
                  >
                    <div
                      className="mb-1.5 px-1 text-xs font-semibold uppercase tracking-wider"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      {t("pinnedListHeader", { count: pinnedMessages.length })}
                    </div>
                    <div className="space-y-1">
                      {pinnedMessages.map((pm, idx) => (
                        <div
                          key={pm.msg_id}
                          className="flex items-center justify-between gap-2 rounded-xl px-2.5 py-1.5"
                          style={{ background: "var(--bg-surface)" }}
                        >
                          <button
                            type="button"
                            onClick={() => {
                              setPinnedIdx(idx);
                              setPinnedListOpen(false);
                              void jumpToMsg(pm.msg_id);
                            }}
                            className="min-w-0 flex-1 text-left transition hover:opacity-85"
                          >
                            <div className="truncate text-xs font-semibold" style={{ color: "var(--accent)" }}>
                              {pm.sender_name || t("msgFallbackLabel")} ·{" "}
                              {pm.ts > 0 ? formatThreadTime(pm.ts, lang) : ""}
                            </div>
                            <div className="truncate text-[13px]" style={{ color: "var(--text-primary)" }}>
                              {pm.text || pm.attachment_name || t("attachmentFallbackLabel")}
                            </div>
                          </button>
                          <button
                            type="button"
                            onClick={() => handleTogglePinMessage(pm)}
                            title={t("unpinThisMsg")}
                            className="hover-menu shrink-0 rounded-lg p-1.5"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            <PinOff className="h-3.5 w-3.5" />
                          </button>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            )}

            {inChatSearchOpen && (
              <div
                className="flex shrink-0 items-center gap-2 px-4 py-2 text-[13.5px]"
                style={{
                  background: "var(--bg-surface)",
                  borderBottom: "1px solid var(--border-subtle)",
                }}
              >
                <Search className="h-4 w-4 shrink-0" style={{ color: "var(--text-secondary)" }} />
                <input
                  ref={inChatSearchInputRef}
                  value={inChatSearchQuery}
                  onChange={(e) => {
                    setInChatSearchQuery(e.target.value);
                    setInChatMatchIdx(0);
                  }}
                  onKeyDown={(e) => {
                    if (isImeComposingEvent(e)) return;
                    if (e.key === "Escape") {
                      e.preventDefault();
                      setInChatSearchOpen(false);
                      setInChatSearchQuery("");
                    } else if (e.key === "Enter" && inChatMatches.length > 0) {
                      e.preventDefault();
                      setInChatMatchIdx((i) =>
                        e.shiftKey
                          ? (i - 1 + inChatMatches.length) % inChatMatches.length
                          : (i + 1) % inChatMatches.length,
                      );
                    }
                  }}
                  placeholder={t("searchInChatPlaceholder")}
                  className="min-w-0 flex-1 bg-transparent text-[14px] outline-none"
                />
                {inChatSearchQuery.trim() && (
                  <span className="shrink-0 text-xs font-mono" style={{ color: "var(--text-secondary)" }}>
                    {inChatMatches.length > 0
                      ? `${Math.min(inChatMatchIdx, inChatMatches.length - 1) + 1}/${inChatMatches.length}`
                      : "0/0"}
                  </span>
                )}
                <button
                  type="button"
                  disabled={inChatMatches.length === 0}
                  onClick={() =>
                    setInChatMatchIdx(
                      (i) => (i - 1 + inChatMatches.length) % inChatMatches.length,
                    )
                  }
                  title="Kết quả trước"
                  className="hover-menu rounded p-1 disabled:opacity-35"
                >
                  <ChevronUp className="h-4 w-4" />
                </button>
                <button
                  type="button"
                  disabled={inChatMatches.length === 0}
                  onClick={() =>
                    setInChatMatchIdx((i) => (i + 1) % inChatMatches.length)
                  }
                  title="Kết quả sau"
                  className="hover-menu rounded p-1 disabled:opacity-35"
                >
                  <ChevronDown className="h-4 w-4" />
                </button>
                <button
                  type="button"
                  onClick={() => setInChatSearchOpen(false)}
                  title="Đóng tìm kiếm (Esc)"
                  className="hover-menu rounded p-1"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
            )}

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
                    {t("translatingThreadTo")}
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
                      {t("translatingBatch")}
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    disabled={threadTranslating || messages.length === 0}
                    onClick={() => {
                      if (!activeThreadLang) return;
                      void translateBatchMessages(messages, activeThreadLang, false, true);
                    }}
                    className="hover-menu flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition disabled:opacity-50"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <RefreshCw className={`h-3.5 w-3.5 ${threadTranslating ? "animate-spin" : ""}`} />
                    {t("retranslateAll")}
                  </button>
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
                    {t("stopTranslatingBtn")}
                  </button>
                </div>
              </div>
            )}

            {aiSummaryByThread[activeThread.id] && (() => {
              const summaryEntry = aiSummaryByThread[activeThread.id];
              return (
                <div
                  className="shrink-0 px-4 py-3 text-[13px]"
                  style={{
                    background: "var(--bg-surface)",
                    borderBottom: "1px solid var(--border-subtle)",
                  }}
                >
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex min-w-0 items-center gap-2">
                      <span
                        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-lg"
                        style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                      >
                        {summaryEntry.loading ? (
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                        ) : (
                          <Sparkles className="h-3.5 w-3.5" />
                        )}
                      </span>
                      <div className="min-w-0">
                        <div className="flex flex-wrap items-center gap-1.5 font-semibold">
                          <span>{t("aiSummaryCardTitle")}</span>
                          {summaryEntry.result && (
                            <span
                              className="rounded-full px-2 py-0.5 text-[11px] font-medium"
                              style={{
                                background: "var(--bg-sidebar)",
                                color: "var(--text-secondary)",
                              }}
                            >
                              {t("aiSummaryMessageCount", {
                                count: summaryEntry.result.message_count,
                              })}
                            </span>
                          )}
                          {summaryEntry.result?.model && (
                            <span
                              className="rounded-full px-2 py-0.5 font-mono text-[10.5px]"
                              style={{
                                background: "var(--accent-soft)",
                                color: "var(--accent)",
                              }}
                            >
                              {summaryEntry.result.model}
                            </span>
                          )}
                        </div>
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      {!summaryEntry.loading && (
                        <button
                          type="button"
                          onClick={() => void handleSummarizeUnread(activeThread.id)}
                          className="hover-menu rounded-lg px-2 py-1 text-xs font-medium transition"
                          style={{ color: "var(--accent)" }}
                        >
                          {t("aiSummaryRetryBtn")}
                        </button>
                      )}
                      <button
                        type="button"
                        onClick={() => handleDismissSummary(activeThread.id)}
                        title={t("close")}
                        className="hover-menu rounded-lg p-1 transition"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <X className="h-4 w-4" />
                      </button>
                    </div>
                  </div>

                  {summaryEntry.loading && (
                    <div className="mt-2 space-y-1.5">
                      <div
                        className="flex items-center gap-1.5 text-xs"
                        style={{ color: "var(--text-secondary)" }}
                      >
                        <Loader2 className="h-3.5 w-3.5 animate-spin" style={{ color: "var(--accent)" }} />
                        <span>{t("aiSummarizingLabel")}</span>
                      </div>
                      {summaryEntry.streamingText && (
                        <div
                          className="max-h-36 overflow-y-auto whitespace-pre-wrap rounded-lg px-3 py-2 text-xs leading-relaxed"
                          style={{
                            background: "var(--bg-sidebar)",
                            border: "1px solid var(--border-subtle)",
                            color: "var(--text-primary)",
                          }}
                        >
                          {summaryEntry.streamingText}
                        </div>
                      )}
                    </div>
                  )}

                  {summaryEntry.error && (
                    <div
                      className="mt-2 flex flex-wrap items-center justify-between gap-2 rounded-lg px-3 py-2 text-xs"
                      style={{ background: "var(--danger-bg)", color: "var(--danger-text)" }}
                    >
                      <span className="wrap-anywhere flex-1">{summaryEntry.error}</span>
                      <button
                        type="button"
                        onClick={() => {
                          setSettingsInitialTab("ai");
                          setSettingsOpen(true);
                        }}
                        className="rounded-lg px-2.5 py-1 text-xs font-semibold underline"
                      >
                        {t("aiSummarySettingsBtn")}
                      </button>
                    </div>
                  )}

                  {summaryEntry.result && (
                    <div className="mt-2 max-h-56 space-y-2 overflow-y-auto pr-1 text-[13px] leading-relaxed">
                      {summaryEntry.result.summary && (
                        <p className="whitespace-pre-wrap" style={{ color: "var(--text-primary)" }}>
                          {summaryEntry.result.summary}
                        </p>
                      )}
                      {summaryEntry.result.bullet_points.length > 0 && (
                        <div className="space-y-1">
                          <div
                            className="text-[11px] font-semibold uppercase tracking-wider"
                            style={{ color: "var(--text-secondary)" }}
                          >
                            {t("aiSummaryBulletsHeading")}
                          </div>
                          <ul className="list-disc space-y-0.5 pl-5 text-[12.5px]">
                            {summaryEntry.result.bullet_points.map((pt, i) => (
                              <li key={i}>{pt}</li>
                            ))}
                          </ul>
                        </div>
                      )}
                      {summaryEntry.result.action_items.length > 0 && (
                        <div className="space-y-1">
                          <div
                            className="text-[11px] font-semibold uppercase tracking-wider"
                            style={{ color: "var(--accent)" }}
                          >
                            {t("aiSummaryActionsHeading")}
                          </div>
                          <ul className="space-y-1 text-[12.5px]">
                            {summaryEntry.result.action_items.map((act, i) => (
                              <li key={i} className="flex items-start gap-1.5">
                                <Check
                                  className="mt-0.5 h-3.5 w-3.5 shrink-0"
                                  style={{ color: "var(--accent)" }}
                                />
                                <span>{act}</span>
                              </li>
                            ))}
                          </ul>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })()}

            <div
              ref={scrollContainerRef}
              onWheel={(e) => {
                if (e.deltaY < 0) stickToBottomRef.current = false;
              }}
              onPointerDown={(e) => {
                if (e.target === e.currentTarget) {
                  stickToBottomRef.current = false;
                }
              }}
              onScroll={(e) => {
                const el = e.currentTarget;
                if (initialScrollReadyRef.current && el.scrollTop < 120 && hasMoreHistory && !loadingOlder) {
                  void loadOlderMessages();
                }
                const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
                if (performance.now() - lastProgrammaticScrollAtRef.current > 120) {
                  if (distFromBottom > 120) {
                    stickToBottomRef.current = false;
                  } else if (distFromBottom < 40) {
                    stickToBottomRef.current = true;
                  }
                }
                const farUp = distFromBottom > 220;
                setShowScrollDown(farUp);
                if (!farUp) setUnreadBelow(0);
                if (distFromBottom < 90 && selectedId) {
                  tryAutoMarkReadActiveThread();
                }
              }}
              className="relative flex-1 overflow-y-auto px-5 py-2.5"
            >
              {loadingMessages && messages.length === 0 && (
                <div className="flex h-full items-center justify-center gap-2 text-[14px]" style={{ color: "var(--text-secondary)" }}>
                  <RefreshCw className="h-4 w-4 animate-spin" style={{ color: "var(--accent)" }} />
                  <span>{t("loadingMessages")}</span>
                </div>
              )}
              {!loadingMessages && messages.length === 0 && (
                <div className="flex h-full items-center justify-center text-[14px]" style={{ color: "var(--text-secondary)" }}>
                  {t("noMessagesYet")}
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
                    {loadingOlder ? t("loadingOlderMessages") : t("loadOlderMessages")}
                  </button>
                </div>
              )}
              <div ref={messagesContentRef}>
                {messages.map((m, idx) => {
                  const prev = idx > 0 ? messages[idx - 1] : null;
                  const next = idx < messages.length - 1 ? messages[idx + 1] : null;
                  const showDate = !prev || dayKey(prev.ts) !== dayKey(m.ts);
                  const showUnread = firstUnreadMsgId === m.msg_id && unreadDividerCount > 0;
                  const groupedWithPrev =
                    !!prev &&
                    !showDate &&
                    !showUnread &&
                    prev.sender_id === m.sender_id &&
                    prev.is_outgoing === m.is_outgoing &&
                    m.ts - prev.ts < 300_000;
                  const nextShowDate = !next || dayKey(m.ts) !== dayKey(next.ts);
                  const nextShowUnread = Boolean(
                    next && firstUnreadMsgId === next.msg_id && unreadDividerCount > 0,
                  );
                  const groupedWithNext =
                    !!next &&
                    !nextShowDate &&
                    !nextShowUnread &&
                    next.sender_id === m.sender_id &&
                    next.is_outgoing === m.is_outgoing &&
                    next.ts - m.ts < 300_000;

                  return (
                    <div
                      key={m.msg_id}
                      className={idx < messages.length - 20 ? "msg-row-perf" : undefined}
                    >
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
                      {showUnread && (
                        <div id="unread-divider" className="my-2 flex items-center gap-3">
                          <div className="h-px flex-1" style={{ background: "var(--accent)", opacity: 0.35 }} />
                          <span
                            className="rounded-full px-3 py-0.5 text-xs font-semibold"
                            style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                          >
                            {t("unreadDivider", { count: unreadDividerCount })}
                          </span>
                          {aiConfig?.enabled && (
                            <button
                              type="button"
                              disabled={Boolean(aiSummaryByThread[activeThread.id]?.loading)}
                              onClick={() => void handleSummarizeUnread(activeThread.id)}
                              className="inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-xs font-semibold transition hover:opacity-85 disabled:opacity-60"
                              style={{
                                background: "var(--bg-sidebar)",
                                border: "1px solid var(--border-subtle)",
                                color: "var(--accent)",
                              }}
                            >
                              {aiSummaryByThread[activeThread.id]?.loading ? (
                                <Loader2 className="h-3 w-3 animate-spin" />
                              ) : (
                                <Sparkles className="h-3 w-3" />
                              )}
                              <span>{t("aiSummarizeUnreadBtn")}</span>
                            </button>
                          )}
                          <div className="h-px flex-1" style={{ background: "var(--accent)", opacity: 0.35 }} />
                        </div>
                      )}
                      <MessageBubble
                        msg={m}
                        threadKind={activeThread.kind}
                        groupedWithPrev={groupedWithPrev}
                        groupedWithNext={groupedWithNext}
                        highlighted={highlightedId === m.msg_id || inChatActiveMatchId === m.msg_id}
                        isSeen={
                          m.is_outgoing &&
                          !m.is_recalled &&
                          !m.send_status &&
                          (activeThread.kind === "cloud" || (activeThread.peer_read_ts ?? 0) >= m.ts)
                        }
                        senderAvatar={avatarByUserId.get(m.sender_id)}
                        translation={translationsByMsg[m.msg_id]}
                        replaceWithTranslation={Boolean(activeThreadLang)}
                        replyCount={replyGraph.repliesByRoot[m.msg_id]?.length ?? 0}
                        highlightQuery={inChatSearchOpen ? inChatSearchQuery : ""}
                        selectMode={selectMode}
                        selected={selectedMsgIds.has(m.msg_id)}
                        onToggleSelect={handleToggleSelectMsg}
                        onStartSelect={handleStartSelect}
                        onTogglePin={handleTogglePinMessage}
                        onReply={handleReplyMessage}
                        onReact={handleReactMessage}
                        onRecall={handleRecallMessage}
                        onDeleteLocal={handleDeleteLocalMessage}
                        onJumpToMsg={handleJumpToMsgVoid}
                        onOpenImage={openLightboxForUrl}
                        onOpenProfile={handleOpenProfileModal}
                        mentionMembers={messageMentionCandidates}
                        onForward={handleForwardSingle}
                        onTranslate={handleTranslateSingle}
                        onHideTranslation={handleHideTranslation}
                        onOpenThread={handleOpenReplyThread}
                        onRetrySend={handleRetryFailedMessage}
                        onDiscardFailed={handleDiscardFailedMessage}
                        onSaveFile={handleSaveRemoteFile}
                        onOpenExternal={handleOpenExternalUrl}
                        onCopyError={showError}
                        onVotePoll={handleVotePoll}
                        onAddPollOption={handleAddPollOption}
                        onLockPoll={handleLockPoll}
                        onRefreshPoll={handleRefreshPoll}
                        onStartVoiceCall={handleStartVoiceCall}
                        callDisabledReason={activeCallCheck.disabledReason}
                      />
                    </div>
                  );
                })}
                <div ref={bottomRef} />
              </div>
            </div>

            {showScrollDown && (
              <button
                type="button"
                onClick={() => {
                  stickToBottomRef.current = true;
                  bottomRef.current?.scrollIntoView({ behavior: "smooth" });
                  setUnreadBelow(0);
                  setShowScrollDown(false);
                  if (selectedId) completeMarkReadForThread(selectedId);
                }}
                title={t("scrollToBottomTitle")}
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

            {selectMode ? (
              <footer
                className="flex shrink-0 flex-wrap items-center justify-between gap-3 px-4 py-3"
                style={{
                  background: "var(--bg-sidebar)",
                  borderTop: "1px solid var(--border-subtle)",
                }}
              >
                <div className="text-[14px] font-medium">
                  {t("selectedMessagesCount", { count: selectedMsgIds.size })}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    disabled={selectedMsgIds.size === 0}
                    onClick={() => {
                      const list = messages.filter(
                        (x) =>
                          selectedMsgIds.has(x.msg_id) &&
                          !x.is_recalled &&
                          !x.send_status,
                      );
                      if (list.length > 0) setForwardMessages(list);
                    }}
                    className="tf-btn flex items-center gap-1.5 rounded-xl px-3.5 py-2 text-[13.5px] font-semibold disabled:cursor-not-allowed disabled:opacity-45"
                  >
                    <Forward className="h-4 w-4" />
                    <span>{t("forwardSelected")}</span>
                  </button>
                  <button
                    type="button"
                    disabled={selectedMsgIds.size === 0}
                    onClick={() => void handleBulkDeleteSelected()}
                    className="flex items-center gap-1.5 rounded-xl bg-rose-600/90 px-3.5 py-2 text-[13.5px] font-semibold text-white transition hover:bg-rose-600 disabled:cursor-not-allowed disabled:opacity-45"
                  >
                    <Trash2 className="h-4 w-4" />
                    <span>{t("deleteLocalSelected")}</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setSelectMode(false);
                      setSelectedMsgIds(new Set());
                    }}
                    className="hover-surface rounded-xl px-3 py-2 text-[13.5px] font-medium transition"
                    style={{
                      background: "var(--bg-surface)",
                      color: "var(--text-secondary)",
                    }}
                  >
                    {t("cancel")}
                  </button>
                </div>
              </footer>
            ) : (
              <Composer
                threadKey={activeThread.id}
                replyTo={replyTo}
                focusSignal={replyFocusSeq}
                draft={draftsByThread[activeThread.id] ?? ""}
                onDraftChange={(val, tid) => setDraftForThread(tid ?? activeThread.id, val)}
                pendingAttachment={pendingAttachment}
                onPendingAttachmentChange={updatePendingAttachment}
                mentionMembers={composerMentionMembers}
                onSendSticker={handleSendSticker}
                onCancelReply={() => setReplyTo(null)}
                onSendText={handleSendText}
                onSendAttachment={handleSendAttachment}
                onBuzz={handleSendBuzz}
                onOpenReminder={
                  activeThread.kind !== "cloud" ? () => setCreateReminderOpen(true) : undefined
                }
                onOpenPoll={
                  activeThread.kind === "group" ? () => setCreatePollOpen(true) : undefined
                }
                onOpenSchedule={() => {
                  setScheduledInitialThreadId(activeThread.id);
                  setScheduledModalOpen(true);
                }}
                buzzCooldownSec={buzzCooldownSec}
                sendOnEnter={sendOnEnter}
                globalPasteFallback={!activeThreadRootId}
                onCaptureScreen={() => void handleCaptureScreen()}
                capturingScreen={capturingScreen}
                onEditPendingImage={
                  pendingAttachment?.kind === "raw" && pendingAttachment.isImage
                    ? handleEditPendingImage
                    : undefined
                }
                onError={showError}
              />
            )}
          </>
          )
        ) : (
          <div
            className="flex flex-1 items-center justify-center text-sm"
            style={{ color: "var(--text-secondary)" }}
          >
            {t("selectConversationPrompt")}
          </div>
        )}
      </main>

      {/* Right Info Drawer */}
      {drawerOpen && activeThread && !aiChatOpen && (
        <>
          {layout.autoCompactDrawer && (
            <div
              className="fixed inset-0 z-20 bg-black/40"
              onClick={() => setDrawerOpen(false)}
            />
          )}
          <aside
            className={
              layout.autoCompactDrawer
                ? "fixed inset-y-0 right-0 z-30 flex w-80 max-w-[90vw] shrink-0 flex-col overflow-hidden p-4 shadow-2xl"
                : "static z-10 flex w-80 shrink-0 flex-col overflow-hidden p-4"
            }
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
              {t("drawerTitle")}
            </h3>
            <button
              type="button"
              onClick={() => setDrawerOpen(false)}
              className="rounded-lg p-1 hover:bg-white/10"
              style={{ color: "var(--text-secondary)" }}
              title={t("close")}
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
                ? activeThreadDetails?.total_members
                  ? t("groupMembersLine", { count: activeThreadDetails.total_members })
                  : t("groupChatLabel")
                : activeThread.kind === "cloud"
                  ? t("myCloudShort")
                  : t("directChatLabel")}
            </div>
            {activeThread.kind === "direct" && (
              <button
                type="button"
                onClick={() => setProfileUid(activeThread.id)}
                className="mt-1 text-xs hover:underline"
                style={{ color: "var(--accent)" }}
              >
                {t("lookupProfile")}
              </button>
            )}
            <div className="mt-0.5 text-xs opacity-60">ID: {activeThread.id}</div>
          </div>

          <div className="mt-4 grid grid-cols-4 gap-1.5 text-xs">
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
              <span>{activeThread.is_pinned ? t("unpinShort") : t("pinShort")}</span>
            </button>
            <button
              type="button"
              onClick={() => {
                if (isThreadCurrentlyMuted(activeThread)) {
                  void handleToggleMute(activeThread);
                } else {
                  setDrawerMuteOpen((v) => !v);
                }
              }}
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: isThreadCurrentlyMuted(activeThread) ? "#fbbf24" : "var(--text-secondary)",
              }}
            >
              {isThreadCurrentlyMuted(activeThread) ? (
                <Bell className="h-4 w-4" />
              ) : (
                <BellOff className="h-4 w-4" />
              )}
              <span>
                {isThreadCurrentlyMuted(activeThread)
                  ? formatMuteUntil(activeThread.muted_until_ts)
                    ? t("unmuteUntilShort", { time: formatMuteUntil(activeThread.muted_until_ts) ?? "" })
                    : t("unmuteShort")
                  : t("muteShort")}
              </span>
            </button>
            <button
              type="button"
              onClick={() =>
                void (activeThread.unread_count > 0
                  ? handleMarkRead(activeThread)
                  : handleMarkUnread(activeThread))
              }
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: activeThread.unread_count > 0 ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              {activeThread.unread_count > 0 ? (
                <>
                  <CheckCheck className="h-4 w-4" />
                  <span>{t("readShort")}</span>
                </>
              ) : (
                <>
                  <MessageSquare className="h-4 w-4" />
                  <span>{t("unreadShort")}</span>
                </>
              )}
            </button>
            <button
              type="button"
              onClick={() => void handleToggleArchive(activeThread, !activeThread.is_archived)}
              className="flex flex-col items-center gap-1 rounded-xl py-2 transition hover:opacity-85"
              style={{
                background: "var(--bg-surface)",
                color: activeThread.is_archived ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              {activeThread.is_archived ? (
                <>
                  <ArchiveRestore className="h-4 w-4" />
                  <span>{t("unarchiveShort")}</span>
                </>
              ) : (
                <>
                  <Archive className="h-4 w-4" />
                  <span>{t("archiveShort")}</span>
                </>
              )}
            </button>
          </div>

          {drawerMuteOpen && !isThreadCurrentlyMuted(activeThread) && (
            <div
              className="mt-2 rounded-xl p-2"
              style={{ background: "var(--bg-surface)", border: "1px solid var(--border-subtle)" }}
            >
              <div className="mb-1.5 text-[11px] font-medium" style={{ color: "var(--text-secondary)" }}>
                {t("muteDurationTitle")}
              </div>
              <div className="flex flex-wrap gap-1.5">
                {[
                  { label: t("mute1h"), secs: 3600 },
                  { label: t("mute4h"), secs: 14400 },
                  { label: t("mute8h"), secs: 28800 },
                  { label: t("mute24h"), secs: 86400 },
                  { label: t("muteForever"), secs: undefined },
                ].map((opt) => (
                  <button
                    key={opt.label}
                    type="button"
                    onClick={() => void handleToggleMute(activeThread, opt.secs)}
                    className="hover-menu rounded-lg px-2 py-1 text-[11.5px] font-medium transition"
                    style={{
                      background: "var(--bg-sidebar)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {opt.label}
                  </button>
                ))}
              </div>
            </div>
          )}

          <div className="mt-4 flex-1 overflow-y-auto pr-0.5">
            {activeThread.kind === "group" && (
              <div className="mb-4">
                <div className="mb-2 flex items-center justify-between gap-2">
                  <div
                    className="text-[13px] font-semibold"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    {t("membersTitle", {
                      count:
                        activeThreadDetails?.total_members ??
                        activeThreadDetails?.members.length ??
                        "...",
                    })}
                  </div>
                  <button
                    type="button"
                    onClick={() => setAddMembersOpen(true)}
                    title={t("addGroupMembersTitle")}
                    className="hover-menu inline-flex items-center gap-1 rounded-lg px-2 py-1 text-[11.5px] font-semibold transition"
                    style={{ background: "var(--accent-soft)", color: "var(--accent)" }}
                  >
                    <UserPlus className="h-3.5 w-3.5" />
                    <span>{t("add")}</span>
                  </button>
                </div>
                {kickTarget && kickTarget.groupId === activeThread.id && (
                  <div
                    className="mb-2 rounded-xl p-2.5 text-xs"
                    style={{
                      background: "var(--danger-bg)",
                      border: "1px solid rgba(244, 63, 94, 0.35)",
                    }}
                  >
                    <div className="font-medium leading-snug" style={{ color: "var(--danger-text)" }}>
                      {t("groupKickConfirmDesc", { name: kickTarget.member.name })}
                    </div>
                    {kickError && (
                      <div className="mt-1.5 text-[11px] leading-snug text-rose-400">
                        {kickError}
                      </div>
                    )}
                    <div className="mt-2 flex items-center justify-end gap-1.5">
                      <button
                        type="button"
                        disabled={kickBusy}
                        onClick={() => {
                          setKickTarget(null);
                          setKickError(null);
                        }}
                        className="hover-menu rounded-lg px-2.5 py-1 text-[11.5px] font-medium transition disabled:opacity-50"
                        style={{ background: "var(--bg-surface)", color: "var(--text-primary)" }}
                      >
                        {t("socialCancelBtn")}
                      </button>
                      <button
                        type="button"
                        disabled={kickBusy}
                        onClick={() => void handleConfirmKickMember()}
                        className="rounded-lg bg-rose-600 px-2.5 py-1 text-[11.5px] font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
                      >
                        {t("socialConfirmActionBtn")}
                      </button>
                    </div>
                  </div>
                )}
                {detailsLoading && !activeThreadDetails ? (
                  <div className="text-[13px] opacity-60">{t("loadingMembers")}</div>
                ) : activeThreadDetails && activeThreadDetails.members.length > 0 ? (
                  (() => {
                    const myUid = myProfile?.uid?.trim() || "";
                    const groupCreatorId = activeThreadDetails.creator_id?.trim() || "";
                    const groupAdminIds = activeThreadDetails.admin_ids ?? [];
                    const isSelfCreator = Boolean(myUid && groupCreatorId && myUid === groupCreatorId);
                    const isSelfAdmin = Boolean(myUid && groupAdminIds.includes(myUid));
                    return (
                      <div className="max-h-48 space-y-1.5 overflow-y-auto">
                        {activeThreadDetails.members.map((mem) => {
                          const memberIsCreator = Boolean(groupCreatorId && mem.id === groupCreatorId);
                          const memberIsAdmin = groupAdminIds.includes(mem.id);
                          const canKick = Boolean(
                            myUid &&
                              mem.id !== myUid &&
                              (isSelfCreator || (isSelfAdmin && !memberIsCreator && !memberIsAdmin)),
                          );
                          return (
                            <div
                              key={mem.id}
                              className="hover-menu flex w-full items-center gap-1.5 rounded-lg px-1.5 py-1 transition"
                            >
                              <button
                                type="button"
                                onClick={() => setProfileUid(mem.id)}
                                title={t("lookupProfile")}
                                className="flex min-w-0 flex-1 items-center gap-2 text-left"
                              >
                                <ThreadAvatar
                                  avatar={mem.avatar}
                                  title={mem.name}
                                  kind="direct"
                                  size="sm"
                                />
                                <div className="min-w-0 flex-1">
                                  <div className="truncate text-[13.5px] font-medium">{mem.name}</div>
                                  {mem.role && mem.role !== "Thành viên" && (
                                    <div className="text-xs" style={{ color: "var(--accent)" }}>
                                      {mem.role}
                                    </div>
                                  )}
                                </div>
                              </button>
                              {canKick && (
                                <button
                                  type="button"
                                  disabled={kickBusy}
                                  onClick={() => {
                                    setKickError(null);
                                    setKickTarget({ groupId: activeThread.id, member: mem });
                                  }}
                                  title={t("groupKickMemberTitle")}
                                  className="shrink-0 rounded-md p-1.5 text-rose-500 transition hover:bg-rose-500/15 disabled:opacity-50"
                                >
                                  <UserMinus className="h-3.5 w-3.5" />
                                </button>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    );
                  })()
                ) : (
                  <div className="text-[13px] opacity-60">{t("noMembersList")}</div>
                )}
              </div>
            )}

            <div>
              <div className="mb-2 flex gap-1 rounded-xl p-1" style={{ background: "var(--bg-surface)" }}>
                <button
                  type="button"
                  onClick={() => setDetailsTab("media")}
                  className="flex flex-1 items-center justify-center gap-1 rounded-lg py-1 text-[12px] font-medium transition"
                  style={{
                    background: detailsTab === "media" ? "var(--bg-hover)" : "transparent",
                    color: detailsTab === "media" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <ImageIcon className="h-3.5 w-3.5" />
                  {t("tabMedia", { count: activeThreadDetails?.media_items.length ?? 0 })}
                </button>
                <button
                  type="button"
                  onClick={() => setDetailsTab("files")}
                  className="flex flex-1 items-center justify-center gap-1 rounded-lg py-1 text-[12px] font-medium transition"
                  style={{
                    background: detailsTab === "files" ? "var(--bg-hover)" : "transparent",
                    color: detailsTab === "files" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <FileText className="h-3.5 w-3.5" />
                  {t("tabFiles", { count: activeThreadDetails?.file_items.length ?? 0 })}
                </button>
                <button
                  type="button"
                  onClick={() => setDetailsTab("links")}
                  className="flex flex-1 items-center justify-center gap-1 rounded-lg py-1 text-[12px] font-medium transition"
                  style={{
                    background: detailsTab === "links" ? "var(--bg-hover)" : "transparent",
                    color: detailsTab === "links" ? "var(--accent)" : "var(--text-secondary)",
                  }}
                >
                  <Link2 className="h-3.5 w-3.5" />
                  {t("tabLinks", { count: activeThreadDetails?.link_items?.length ?? 0 })}
                </button>
              </div>

              {detailsTab === "media" ? (
                activeThreadDetails && activeThreadDetails.media_items.length > 0 ? (
                  <div className="grid grid-cols-3 gap-1.5">
                    {activeThreadDetails.media_items.slice(0, 18).map((item) => {
                      const tLower = item.msg_type.toLowerCase();
                      const isVid =
                        tLower.includes("video") ||
                        tLower === "4" ||
                        tLower === "44" ||
                        /\.(mp4|mov|webm)(\?|$)/i.test(item.attachment_url || "");
                      return (
                        <button
                          key={item.msg_id}
                          type="button"
                          onClick={() => {
                            if (isVid) {
                              void jumpToMsg(item.msg_id);
                            } else if (item.attachment_url) {
                              openLightboxForUrl(
                                item.attachment_url,
                                item.attachment_name || "Hình ảnh",
                                { threadId: item.thread_id, msgId: item.msg_id },
                              );
                            }
                          }}
                          title={isVid ? `Video: ${item.attachment_name || "Xem video"}` : item.attachment_name || "Hình ảnh"}
                          className="relative aspect-square overflow-hidden rounded-lg bg-black/25"
                        >
                          {isVid ? (
                            <div className="flex h-full w-full flex-col items-center justify-center gap-1 bg-slate-900/80 p-1 text-white">
                              <span className="flex h-8 w-8 items-center justify-center rounded-full bg-white/20">
                                <Play className="h-4 w-4 fill-white" />
                              </span>
                              <span className="max-w-full truncate text-[10px] opacity-80">
                                {item.attachment_name || "Video"}
                              </span>
                            </div>
                          ) : (
                            item.attachment_url && (
                              <RelayImage
                                src={item.attachment_url}
                                threadId={item.thread_id}
                                msgId={item.msg_id}
                                alt={item.attachment_name || "Media"}
                                referrerPolicy="no-referrer"
                                className="h-full w-full object-cover"
                                loading="lazy"
                              />
                            )
                          )}
                        </button>
                      );
                    })}
                  </div>
                ) : (
                  <div className="py-4 text-center text-xs opacity-60">{t("noMediaYet")}</div>
                )
              ) : detailsTab === "files" ? (
                activeThreadDetails && activeThreadDetails.file_items.length > 0 ? (
                <div className="space-y-1.5">
                  {activeThreadDetails.file_items.slice(0, 20).map((item) => {
                    const drawerAction = classifyDrawerFileAction(item);
                    const normalizedUrl = normalizeMediaRequestUrl(item.attachment_url);
                    return (
                      <button
                        key={item.msg_id}
                        type="button"
                        onClick={() => {
                          if (drawerAction === "open_external" && normalizedUrl) {
                            handleOpenExternalUrl(normalizedUrl);
                          } else if (drawerAction === "save_file" && normalizedUrl) {
                            handleSaveRemoteFile(
                              normalizedUrl,
                              item.attachment_name || "attachment",
                              { threadId: item.thread_id, msgId: item.msg_id },
                            );
                          } else {
                            void jumpToMsg(item.msg_id);
                          }
                        }}
                        title={
                          drawerAction === "open_external"
                            ? t("openExternalBrowser")
                            : t("clickToDownloadFile")
                        }
                        className="hover-menu flex w-full items-center justify-between gap-2 rounded-lg px-2 py-1.5 text-left text-xs transition"
                        style={{ background: "var(--bg-surface)" }}
                      >
                        <div className="flex min-w-0 flex-1 items-center gap-2">
                          {drawerAction === "open_external" ? (
                            <ExternalLink className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                          ) : (
                            <FileText className="h-4 w-4 shrink-0" style={{ color: "var(--accent)" }} />
                          )}
                          <span className="truncate">{item.attachment_name || item.text}</span>
                        </div>
                        {drawerAction === "open_external" ? (
                          <ExternalLink className="h-3.5 w-3.5 shrink-0 opacity-75" style={{ color: "var(--accent)" }} />
                        ) : (
                          <Download className="h-3.5 w-3.5 shrink-0 opacity-75" style={{ color: "var(--accent)" }} />
                        )}
                      </button>
                    );
                  })}
                </div>
                ) : (
                  <div className="py-4 text-center text-xs opacity-60">{t("noFilesYet")}</div>
                )
              ) : activeThreadDetails && (activeThreadDetails.link_items?.length ?? 0) > 0 ? (
                <div className="space-y-1.5">
                  {activeThreadDetails.link_items.slice(0, 25).map((item) => {
                    const url = extractFirstUrl(item.text);
                    return (
                      <div
                        key={item.msg_id}
                        className="flex items-start justify-between gap-1.5 rounded-lg px-2 py-1.5 text-xs"
                        style={{ background: "var(--bg-surface)" }}
                      >
                        <button
                          type="button"
                          onClick={() => void jumpToMsg(item.msg_id)}
                          title={t("jumpToMsgTitle")}
                          className="min-w-0 flex-1 text-left transition hover:opacity-85"
                        >
                          <div className="truncate font-medium" style={{ color: "var(--accent)" }}>
                            {url || item.text}
                          </div>
                          <div className="truncate text-[11px] opacity-75">
                            {item.sender_name || t("msgFallbackLabel")} · {formatThreadTime(item.ts, lang)}
                          </div>
                        </button>
                        {url && (
                          <a
                            href={url}
                            target="_blank"
                            rel="noreferrer"
                            title={url}
                            className="hover-menu shrink-0 rounded p-1"
                            style={{ color: "var(--accent)" }}
                          >
                            <ExternalLink className="h-3.5 w-3.5" />
                          </a>
                        )}
                      </div>
                    );
                  })}
                </div>
              ) : (
                <div className="py-4 text-center text-xs opacity-60">{t("noLinksYet")}</div>
              )}
            </div>
          </div>

          {activeThread.kind === "direct" && (
            <div
              className="mt-3 shrink-0 pt-3"
              style={{ borderTop: "1px solid var(--border-subtle)" }}
            >
              <button
                type="button"
                onClick={() => {
                  setProfileInitial(null);
                  setProfileUid(activeThread.id);
                }}
                className="hover-menu flex w-full items-center justify-center gap-2 rounded-xl px-3 py-2 text-xs font-semibold transition"
                style={{
                  background: "var(--bg-surface)",
                  color: "var(--text-primary)",
                  border: "1px solid var(--border-subtle)",
                }}
              >
                <Shield className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />
                <span className="truncate">{t("dmManageRelationshipBtn")}</span>
              </button>
            </div>
          )}

          {activeThread.kind === "group" && (
            <div
              className="mt-3 shrink-0 pt-3"
              style={{ borderTop: "1px solid var(--border-subtle)" }}
            >
              <div
                className="mb-1.5 text-[11px] font-semibold uppercase tracking-wider"
                style={{ color: "var(--text-secondary)" }}
              >
                {t("groupDangerZoneTitle")}
              </div>
              <button
                type="button"
                onClick={() => handleOpenLeaveGroupDialog(activeThread)}
                className="flex w-full items-center justify-center gap-2 rounded-xl px-3 py-2 text-xs font-semibold text-rose-500 transition hover:bg-rose-500/15"
                style={{
                  background: "var(--danger-bg)",
                  border: "1px solid rgba(244, 63, 94, 0.3)",
                }}
              >
                <LogOut className="h-3.5 w-3.5 shrink-0" />
                <span>{t("groupLeaveBtn")}</span>
              </button>
            </div>
          )}
        </aside>
        </>
      )}
        </>
      )}

      {threadMenu && (
        <div
          className="fixed z-50 max-h-[75vh] w-56 overflow-y-auto rounded-xl py-1 shadow-2xl"
          style={{
            left: Math.max(8, Math.min(threadMenu.x, window.innerWidth - 232)),
            top: Math.max(8, Math.min(threadMenu.y, window.innerHeight - 330)),
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
                <PinOff className="h-3.5 w-3.5" /> {t("unpinThread")}
              </>
            ) : (
              <>
                <Pin className="h-3.5 w-3.5" /> {t("pinThread")}
              </>
            )}
          </button>
          <button
            type="button"
            onClick={() => {
              void (threadMenu.thread.unread_count > 0
                ? handleMarkRead(threadMenu.thread)
                : handleMarkUnread(threadMenu.thread));
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            {threadMenu.thread.unread_count > 0 ? (
              <>
                <CheckCheck className="h-3.5 w-3.5" /> {t("markAsRead")}
              </>
            ) : (
              <>
                <MessageSquare className="h-3.5 w-3.5" /> {t("markAsUnread")}
              </>
            )}
          </button>
          {isThreadCurrentlyMuted(threadMenu.thread) ? (
            <button
              type="button"
              onClick={() => {
                void handleToggleMute(threadMenu.thread);
                setThreadMenu(null);
              }}
              className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
            >
              <Bell className="h-3.5 w-3.5" />
              <span>
                {formatMuteUntil(threadMenu.thread.muted_until_ts)
                  ? t("unmuteUntil", { time: formatMuteUntil(threadMenu.thread.muted_until_ts) ?? "" })
                  : t("unmuteThread")}
              </span>
            </button>
          ) : (
            <div className="px-3 py-1.5">
              <div
                className="mb-1.5 flex items-center gap-1.5 text-[12px] font-medium"
                style={{ color: "var(--text-secondary)" }}
              >
                <BellOff className="h-3.5 w-3.5" />
                <span>{t("muteTitle")}</span>
              </div>
              <div className="flex flex-wrap gap-1">
                {[
                  { label: "1h", secs: 3600 },
                  { label: "4h", secs: 14400 },
                  { label: "8h", secs: 28800 },
                  { label: "24h", secs: 86400 },
                  { label: t("muteForever"), secs: undefined },
                ].map((opt) => (
                  <button
                    key={opt.label}
                    type="button"
                    onClick={() => {
                      void handleToggleMute(threadMenu.thread, opt.secs);
                      setThreadMenu(null);
                    }}
                    className="hover-menu rounded-md px-2 py-1 text-[11px] font-medium transition"
                    style={{
                      background: "var(--bg-surface)",
                      color: "var(--text-primary)",
                    }}
                  >
                    {opt.label}
                  </button>
                ))}
              </div>
            </div>
          )}
          <div className="my-1 h-px" style={{ background: "var(--border-subtle)" }} />
          <button
            type="button"
            onClick={() => {
              void handleToggleArchive(threadMenu.thread, !threadMenu.thread.is_archived);
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            {threadMenu.thread.is_archived ? (
              <>
                <ArchiveRestore className="h-3.5 w-3.5" /> {t("unarchiveThread")}
              </>
            ) : (
              <>
                <Archive className="h-3.5 w-3.5" /> {t("archiveThread")}
              </>
            )}
          </button>
          <div className="my-1 h-px" style={{ background: "var(--border-subtle)" }} />
          {customFolders.length > 3 && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setThreadMenuFoldersExpanded((v) => !v);
              }}
              className="hover-menu flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-[13px]"
              style={{ color: "var(--text-secondary)" }}
            >
              <span>{t("addToFolderCount", { count: customFolders.length })}</span>
              {threadMenuFoldersExpanded ? (
                <ChevronUp className="h-3.5 w-3.5 shrink-0" />
              ) : (
                <ChevronDown className="h-3.5 w-3.5 shrink-0" />
              )}
            </button>
          )}
          {(customFolders.length <= 3 || threadMenuFoldersExpanded) &&
            customFolders.map((cf) => {
              const added = cf.thread_ids.includes(threadMenu.thread.id);
              return (
                <button
                  key={cf.id}
                  type="button"
                  onClick={() => {
                    void handleToggleThreadInFolder(cf, threadMenu.thread.id);
                    setThreadMenu(null);
                  }}
                  className="hover-menu flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-[13px]"
                >
                  <span className="flex min-w-0 items-center gap-2">
                    {renderFolderIcon(cf.icon, "h-3.5 w-3.5 shrink-0")}
                    <span className="truncate">{cf.name}</span>
                  </span>
                  {added && <Check className="h-3.5 w-3.5 shrink-0" style={{ color: "var(--accent)" }} />}
                </button>
              );
            })}
          <button
            type="button"
            onClick={() => {
              setEditingFolder(null);
              setFolderModalOpen(true);
              setThreadMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13px]"
            style={{ color: "var(--accent)" }}
          >
            <FolderPlus className="h-3.5 w-3.5" /> {t("manageFoldersEllipsis")}
          </button>
          {threadMenu.thread.kind === "group" && (
            <>
              <div className="my-1 h-px" style={{ background: "var(--border-subtle)" }} />
              <button
                type="button"
                onClick={() => {
                  const target = threadMenu.thread;
                  setThreadMenu(null);
                  handleOpenLeaveGroupDialog(target);
                }}
                className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px] text-rose-500"
              >
                <LogOut className="h-3.5 w-3.5" /> {t("groupLeaveBtn")}
              </button>
            </>
          )}
        </div>
      )}

      {folderMenu && (
        <div
          className="fixed z-50 w-52 rounded-xl py-1 shadow-2xl"
          style={{
            left: Math.max(8, Math.min(folderMenu.x, window.innerWidth - 220)),
            top: Math.max(8, Math.min(folderMenu.y, window.innerHeight - 150)),
            background: "var(--bg-sidebar)",
            border: "1px solid var(--border-subtle)",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          <button
            type="button"
            onClick={() => {
              void handleMarkFolderRead(`custom:${folderMenu.folder.id}`);
              setFolderMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            <CheckCheck className="h-3.5 w-3.5" /> {t("markFolderAllRead")}
          </button>
          <button
            type="button"
            onClick={() => {
              setEditingFolder(folderMenu.folder);
              setFolderModalOpen(true);
              setFolderMenu(null);
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px]"
          >
            <FolderPlus className="h-3.5 w-3.5" /> {t("editFolder")}
          </button>
          <button
            type="button"
            onClick={() => {
              const fid = folderMenu.folder.id;
              setFolderMenu(null);
              void bridge.deleteChatFolder(fid).then((list) => {
                setCustomFolders(list);
                if (folder === `custom:${fid}`) setFolder("all");
              });
            }}
            className="hover-menu flex w-full items-center gap-2 px-3 py-2 text-left text-[13.5px] text-rose-500"
          >
            <Trash2 className="h-3.5 w-3.5" /> {t("deleteFolder")}
          </button>
        </div>
      )}

      {utilityModals}

      {quickSearchOpen && (
        <Suspense fallback={null}>
          <QuickSearchModal
            open={quickSearchOpen}
            threads={threads}
            onClose={() => setQuickSearchOpen(false)}
            onSelectMessage={(hit) => void handleSelectSearchResult(hit)}
            onSelectThread={(threadId) => openThreadById(threadId)}
            onFindPhone={(phone) => {
              setFindUserInitialPhone(phone);
              setFindUserOpen(true);
            }}
          />
        </Suspense>
      )}

      {activeThread && activeThread.kind === "group" && addMembersOpen && (
        <Suspense fallback={null}>
          <AddGroupMembersModal
            open={addMembersOpen}
            threadId={activeThread.id}
            groupTitle={activeThread.title}
            threads={threads}
            existingMemberIds={(activeThreadDetails?.members ?? activeMentionMembers).map((m) => m.id)}
            onClose={() => setAddMembersOpen(false)}
            onAdded={(res) => {
              if (res.added.length > 0) {
                setDetailsRefreshTick((n) => n + 1);
              }
            }}
          />
        </Suspense>
      )}

      {activeThread && createReminderOpen && (
        <Suspense fallback={null}>
          <CreateReminderModal
            open={createReminderOpen}
            threadTitle={activeThread.title}
            onClose={() => setCreateReminderOpen(false)}
            onSubmit={async (title, startTimeMs, repeat) => {
              const msg = await bridge.createReminder(
                activeThread.id,
                title,
                startTimeMs,
                repeat,
              );
              setMessages((prev) => upsertMsg(prev, msg));
              setThreads((prev) => bumpThreadList(prev, msg, false));
              setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
            }}
          />
        </Suspense>
      )}

      {activeThread && activeThread.kind === "group" && createPollOpen && (
        <Suspense fallback={null}>
          <CreatePollModal
            open={createPollOpen}
            groupTitle={activeThread.title}
            onClose={() => setCreatePollOpen(false)}
            onSubmit={async (payload) => {
              const msg = await bridge.createPoll(
                activeThread.id,
                payload.question,
                payload.options,
                payload.expiredTimeMs,
                payload.allowMulti,
                payload.allowAddOption,
                payload.isAnonymous,
              );
              setMessages((prev) => upsertMsg(prev, msg));
              setThreads((prev) => bumpThreadList(prev, msg, false));
              setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
            }}
          />
        </Suspense>
      )}

      {forwardMessages.length > 0 && (
        <Suspense fallback={null}>
          <ForwardModal
            messages={forwardMessages}
            threads={threads}
            onClose={() => setForwardMessages([])}
            onForwardTo={async (targetThreadId, msgs) => {
              try {
                let forwardedAny = false;
                for (const msg of msgs) {
                  if (msg.is_recalled) continue;
                  const sent = await bridge.forwardMessage(
                    msg.thread_id || selectedIdRef.current || selectedId || "",
                    msg.msg_id,
                    targetThreadId,
                    {
                      text: msg.text,
                      msgType: msg.msg_type,
                      attachmentUrl: msg.attachment_url,
                      attachmentName: msg.attachment_name,
                    },
                  );
                  forwardedAny = true;
                  setThreads((prev) => bumpThreadList(prev, sent, false));
                  if (targetThreadId === selectedIdRef.current) {
                    setMessages((prev) => upsertMsg(prev, sent));
                  }
                }
                if (forwardedAny && targetThreadId === selectedIdRef.current) {
                  setDetailsRefreshTick((n) => n + 1);
                  setTimeout(() => bottomRef.current?.scrollIntoView({ behavior: "smooth" }), 40);
                }
                setSelectMode(false);
                setSelectedMsgIds(new Set());
              } catch (err) {
                showError(err);
              }
            }}
          />
        </Suspense>
      )}

      {lightboxState && (
        <Suspense fallback={null}>
          <MediaLightbox
            items={lightboxState.items}
            activeIndex={lightboxState.index}
            onIndexChange={(idx) =>
              setLightboxState((prev) => (prev ? { ...prev, index: idx } : null))
            }
            onClose={() => setLightboxState(null)}
            onNotify={showError}
          />
        </Suspense>
      )}

      {profileUid && (
        <Suspense fallback={null}>
          <UserProfileModal
            userId={profileUid}
            initialProfile={profileInitial}
            onClose={() => {
              setProfileUid(null);
              setProfileInitial(null);
            }}
            onOpenImage={openLightboxForUrl}
            onOpenGroup={(gid) => {
              leaveWebApps();
              selectAndRevealThread(gid, { forceScroll: true });
              setMobilePane("chat");
            }}
            onStartChat={handleStartChatFromProfile}
            onRelationshipChanged={handleRelationshipChanged}
          />
        </Suspense>
      )}

      {leaveGroupTarget && (
        <div
          role="dialog"
          aria-modal="true"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-4 backdrop-blur-xs"
          onClick={() => {
            if (!leaveGroupBusy) {
              setLeaveGroupTarget(null);
              setLeaveGroupError(null);
            }
          }}
        >
          <div
            className="w-full max-w-md overflow-hidden rounded-2xl p-5 shadow-2xl"
            style={{
              background: "var(--bg-sidebar)",
              border: "1px solid var(--border-subtle)",
              color: "var(--text-primary)",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between gap-2">
              <div className="flex min-w-0 items-center gap-2 font-semibold">
                <LogOut className="h-4 w-4 shrink-0 text-rose-500" />
                <span className="truncate text-[15px]">{t("groupLeaveConfirmTitle")}</span>
              </div>
              <button
                type="button"
                disabled={leaveGroupBusy}
                onClick={() => {
                  setLeaveGroupTarget(null);
                  setLeaveGroupError(null);
                }}
                className="hover-menu rounded-lg p-1 transition disabled:opacity-50"
                style={{ color: "var(--text-secondary)" }}
                title={t("close")}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <p className="mt-3 text-[13px] leading-relaxed" style={{ color: "var(--text-secondary)" }}>
              {t("groupLeaveConfirmDesc", { name: leaveGroupTarget.title })}
            </p>

            <label
              className="mt-3.5 flex cursor-pointer items-start gap-2.5 rounded-xl p-3 text-left transition"
              style={{ background: "var(--bg-surface)", border: "1px solid var(--border-subtle)" }}
            >
              <input
                type="checkbox"
                checked={leaveGroupSilent}
                disabled={leaveGroupBusy}
                onChange={(e) => setLeaveGroupSilent(e.target.checked)}
                className="mt-0.5 h-4 w-4 shrink-0 accent-rose-500"
              />
              <span className="min-w-0 flex-1">
                <span className="block text-[13px] font-medium">{t("groupLeaveSilentLabel")}</span>
                <span
                  className="mt-0.5 block text-[11.5px] leading-snug"
                  style={{ color: "var(--text-secondary)" }}
                >
                  {t("groupLeaveSilentHint")}
                </span>
              </span>
            </label>

            {leaveGroupError && (
              <div
                role="alert"
                className="mt-3 rounded-xl px-3 py-2 text-xs leading-snug"
                style={{
                  background: "var(--danger-bg)",
                  color: "var(--danger-text)",
                  border: "1px solid rgba(244, 63, 94, 0.35)",
                }}
              >
                {leaveGroupError}
              </div>
            )}

            <div className="mt-5 flex items-center justify-end gap-2">
              <button
                type="button"
                disabled={leaveGroupBusy}
                onClick={() => {
                  setLeaveGroupTarget(null);
                  setLeaveGroupError(null);
                }}
                className="hover-menu rounded-xl px-4 py-2 text-xs font-medium transition disabled:opacity-50"
                style={{ background: "var(--bg-surface)", color: "var(--text-primary)" }}
              >
                {t("socialCancelBtn")}
              </button>
              <button
                type="button"
                disabled={leaveGroupBusy}
                onClick={() => void handleConfirmLeaveGroup()}
                className="inline-flex items-center gap-1.5 rounded-xl bg-rose-600 px-4 py-2 text-xs font-semibold text-white transition hover:bg-rose-500 disabled:opacity-50"
              >
                {leaveGroupBusy ? (
                  <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <LogOut className="h-3.5 w-3.5" />
                )}
                <span>{t("groupLeaveBtn")}</span>
              </button>
            </div>
          </div>
        </div>
      )}

      {screenshotEditor && (
        <Suspense fallback={null}>
          <ScreenshotEditorModal
            open
            sourceUrl={screenshotEditor.sourceUrl}
            initialMode={screenshotEditor.initialMode}
            monitors={screenshotEditor.monitors}
            activeMonitorIndex={screenshotEditor.activeMonitorIndex}
            hideWindowOnCapture={hideWindowOnCapture}
            capturing={capturingScreen}
            canRecapture={screenshotEditor.canRecapture}
            onToggleHideWindow={(val) => {
              setHideWindowOnCapture(val);
              try {
                localStorage.setItem("teza.screenshotHideWindow", val ? "1" : "0");
              } catch {
                // Ignore storage quota errors
              }
            }}
            onRecapture={(opts) => {
              void handleCaptureScreen({
                monitorIndex: opts.monitorIndex,
                captureAll: opts.captureAll,
              });
            }}
            onClose={closeScreenshotEditor}
            onAttach={(att, cap) => {
              updatePendingAttachment(att);
              if (selectedId && cap) {
                setDraftForThread(selectedId, cap);
              }
            }}
            onSend={async (att, cap) => {
              const sent = await handleSendAttachment(att, cap);
              if (sent && screenshotEditor.initialMode === "annotate") {
                updatePendingAttachment(null);
              }
              return sent;
            }}
            onError={showError}
          />
        </Suspense>
      )}

      {!isLocked && (
        <DownloadOverlay
          activeDownloads={activeDownloads}
          completedDownload={completedDownload}
          onDismissCompleted={() => setCompletedDownload(null)}
          onOpenFile={(path) => bridge.openSavedFile(path)}
          onRevealFile={(path) => bridge.revealSavedFile(path)}
          onError={showError}
        />
      )}

      {webNotifyToast && !isLocked && (
        <div
          role="button"
          tabIndex={0}
          onMouseEnter={() => setWebToastHovered(true)}
          onMouseLeave={() => setWebToastHovered(false)}
          onClick={() => {
            const targetId = webNotifyToast.thread_id;
            dismissWebNotifyToast();
            openThreadById(targetId);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              const targetId = webNotifyToast.thread_id;
              dismissWebNotifyToast();
              openThreadById(targetId);
            }
          }}
          className={`fixed z-[75] flex w-[320px] max-w-[calc(100vw-2rem)] cursor-pointer items-center gap-2.5 overflow-hidden rounded-xl px-3 py-2 shadow-2xl transition hover:brightness-105 ${
            webNotifyToast.is_buzz && !webToastHovered ? "notify-buzz-shaking" : ""
          } ${
            webNotifyToast.position === "top-left"
              ? "left-4 top-4"
              : webNotifyToast.position === "top-right"
                ? "right-4 top-4"
                : webNotifyToast.position === "bottom-left"
                  ? "bottom-4 left-4"
                  : "bottom-4 right-4"
          }`}
          style={{
            background: "var(--bg-sidebar)",
            border: webNotifyToast.is_buzz
              ? "1px solid rgba(245, 158, 11, 0.55)"
              : "1px solid var(--border-subtle)",
            color: "var(--text-primary)",
          }}
        >
          <span
            aria-hidden="true"
            className="absolute inset-y-0 left-0 w-[3px]"
            style={{ background: webNotifyToast.is_buzz ? "#f59e0b" : "var(--accent)" }}
          />
          <div className="shrink-0 pl-0.5">
            <ThreadAvatar
              avatar={webNotifyToast.avatar}
              title={webNotifyToast.title}
              kind="direct"
              size="sm"
            />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center justify-between gap-1.5">
              <div className="min-w-0 truncate text-[13px] font-semibold leading-tight">
                {webNotifyToast.title}
              </div>
              <button
                type="button"
                onMouseDown={(e) => {
                  if (e.button !== 0) return;
                  e.preventDefault();
                  e.stopPropagation();
                  dismissWebNotifyToast();
                }}
                onClick={(e) => {
                  e.stopPropagation();
                  dismissWebNotifyToast();
                }}
                onKeyDown={(e) => e.stopPropagation()}
                className="hover-menu -mr-1 shrink-0 rounded-md p-0.5 transition"
                style={{ color: "var(--text-secondary)" }}
                title={t("notifyCloseTitle")}
                aria-label={t("notifyCloseTitle")}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
            <div
              className="mt-0.5 line-clamp-2 break-words text-[12px] leading-[1.35]"
              style={{ color: "var(--text-secondary)" }}
            >
              {webNotifyToast.body}
            </div>
            <div
              className="mt-0.5 truncate text-[10.5px] font-medium leading-tight"
              style={{ color: "var(--accent)" }}
            >
              {t("notifyClickHint")}
            </div>
          </div>
        </div>
      )}

      {authed &&
        !isLocked &&
        callState.active &&
        callState.phase !== "idle" &&
        callState.phase !== "incoming_ringing" &&
        (showVideoOverlay && videoOverlayMode !== "minimized" ? (
          <Suspense fallback={null}>
            <VideoCallOverlay
              active={callState.active}
              telemetry={callTelemetry}
              devices={callDevices}
              devicesLoading={callDevicesLoading}
              cameraDevices={cameraDevices}
              selectedCameraId={selectedCameraId}
              localStream={localVideoStream}
              localCameraStatus={localCameraStatus}
              localCameraError={localCameraError}
              hasRemoteVideoFrame={hasRemoteVideoFrame}
              remoteVideoDims={remoteVideoDims}
              busy={callActionBusy}
              hangupBusy={callHangupBusy}
              viewMode={videoOverlayMode}
              onViewModeChange={(mode) => setVideoOverlayMode(mode)}
              onCollapseToBar={() => setVideoOverlayMode("minimized")}
              onRemoteCanvasChange={handleRemoteCanvasChange}
              onToggleMute={(nextMuted) => void handleToggleCallMute(nextMuted)}
              onToggleCamera={(nextEnabled) => void handleToggleCallCamera(nextEnabled)}
              onHangup={() => void handleHangupCall()}
              onMinimize={() => setVideoOverlayMode("floating")}
              onSelectThread={(partnerId) => openThreadById(partnerId)}
              onRefreshDevices={refreshCallDevices}
              onRefreshCameras={refreshCameraDevices}
              onSelectDevices={(inp, out) => void handleSelectCallDevices(inp, out)}
              onSelectCamera={handleSelectCameraDevice}
            />
          </Suspense>
        ) : (
          <ActiveCallBar
            active={callState.active}
            telemetry={callTelemetry}
            devices={callDevices}
            devicesLoading={callDevicesLoading}
            busy={callActionBusy}
            hangupBusy={callHangupBusy}
            localCameraStatus={localCameraStatus}
            onToggleMute={(nextMuted) => void handleToggleCallMute(nextMuted)}
            onToggleCamera={(nextEnabled) => void handleToggleCallCamera(nextEnabled)}
            onExpandVideo={() => setVideoOverlayMode("floating")}
            onHangup={() => void handleHangupCall()}
            onSelectThread={(partnerId) => openThreadById(partnerId)}
            onRefreshDevices={refreshCallDevices}
            onSelectDevices={(inp, out) => void handleSelectCallDevices(inp, out)}
            onDevicesPopoverChange={setActiveCallDevicesOpen}
          />
        ))}

      {authed &&
        !isLocked &&
        callState.phase === "incoming_ringing" &&
        callState.active && (
          <IncomingCallModal
            active={callState.active}
            devices={callDevices}
            devicesLoading={callDevicesLoading}
            busy={callActionBusy}
            onAnswer={(cid) => void handleAnswerCall(cid)}
            onDecline={(cid) => void handleDeclineCall(cid)}
            onRefreshDevices={refreshCallDevices}
            onSelectDevices={(inp, out) => void handleSelectCallDevices(inp, out)}
          />
        )}

      {callNotice && !isLocked && (
        <div className="fixed right-4 top-20 z-[80] max-w-sm">
          <button
            type="button"
            onClick={dismissCallNotice}
            title={t("notifyCloseTitle")}
            className="flex items-center gap-2 rounded-xl px-3.5 py-2 text-left text-xs font-medium shadow-2xl transition hover:opacity-90"
            style={{
              background:
                callNotice.tone === "error"
                  ? "var(--danger-bg)"
                  : callNotice.tone === "warn"
                    ? "rgba(245, 158, 11, 0.16)"
                    : "var(--bg-sidebar)",
              border:
                callNotice.tone === "warn"
                  ? "1px solid rgba(245, 158, 11, 0.45)"
                  : "1px solid var(--border-subtle)",
              color:
                callNotice.tone === "error"
                  ? "var(--danger-text)"
                  : callNotice.tone === "warn"
                    ? "#f59e0b"
                    : "var(--text-primary)",
            }}
          >
            {callState.active?.call_type === "DirectVideo" ? (
              <Video className="h-3.5 w-3.5 shrink-0" />
            ) : (
              <Phone className="h-3.5 w-3.5 shrink-0" />
            )}
            <span className="min-w-0 flex-1">{callNotice.text}</span>
            <X className="h-3.5 w-3.5 shrink-0 opacity-70" />
          </button>
        </div>
      )}

      {socialNotice && !isLocked && (
        <div
          className={`fixed right-4 z-[85] max-w-sm ${callNotice ? "top-32" : "top-20"}`}
        >
          <button
            type="button"
            onClick={dismissSocialNotice}
            title={t("notifyCloseTitle")}
            className="flex items-center gap-2 rounded-xl px-3.5 py-2 text-left text-xs font-medium shadow-2xl transition hover:opacity-90"
            style={{
              background:
                socialNotice.tone === "error"
                  ? "var(--danger-bg)"
                  : "var(--bg-sidebar)",
              border:
                socialNotice.tone === "error"
                  ? "1px solid rgba(244, 63, 94, 0.45)"
                  : socialNotice.tone === "warn"
                    ? "1px solid rgba(245, 158, 11, 0.5)"
                    : socialNotice.tone === "success"
                      ? "1px solid rgba(16, 185, 129, 0.5)"
                      : "1px solid var(--border-subtle)",
              color:
                socialNotice.tone === "error"
                  ? "var(--danger-text)"
                  : "var(--text-primary)",
            }}
          >
            <UserCheck
              className="h-3.5 w-3.5 shrink-0"
              style={{
                color:
                  socialNotice.tone === "error"
                    ? "var(--danger-text)"
                    : socialNotice.tone === "warn"
                      ? "#f59e0b"
                      : socialNotice.tone === "success"
                        ? "#10b981"
                        : "var(--accent)",
              }}
            />
            <span className="min-w-0 flex-1">{socialNotice.text}</span>
            <X className="h-3.5 w-3.5 shrink-0 opacity-70" />
          </button>
        </div>
      )}

      {toastError && (
        <div
          role="alert"
          onClick={dismissToastError}
          title={t("notifyCloseTitle")}
          className="fixed right-4 top-16 z-[90] flex max-w-md cursor-pointer items-center gap-2 rounded-xl bg-rose-600/95 px-3.5 py-2 text-xs text-white shadow-2xl transition hover:bg-rose-600"
        >
          <span className="min-w-0 flex-1 break-words">{toastError}</span>
          <button
            type="button"
            onMouseDown={(e) => {
              if (e.button !== 0) return;
              e.preventDefault();
              e.stopPropagation();
              dismissToastError();
            }}
            onClick={(e) => {
              e.stopPropagation();
              dismissToastError();
            }}
            title={t("notifyCloseTitle")}
            aria-label={t("notifyCloseTitle")}
            className="shrink-0 rounded-md p-0.5 text-white/85 transition hover:bg-white/15 hover:text-white"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      <PasscodeLockScreen
        open={authed && isLocked}
        myName={myProfile?.display_name}
        onUnlock={async (pin) => {
          const ok = await verifyPasscode(pin, passcodeCfg);
          if (ok) {
            lastActivityAtRef.current = Date.now();
            setIsLocked(false);
          }
          return ok;
        }}
        onLogout={() => {
          savePasscodeConfig(DEFAULT_PASSCODE_CONFIG);
          setPasscodeCfg(DEFAULT_PASSCODE_CONFIG);
          void bridge.logout().then(() => {
            setAuthed(false);
            resetSessionUi();
            void refreshQr();
          });
        }}
      />
    </div>
  );
}
