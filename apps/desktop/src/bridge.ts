import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AccountProfile,
  AddGroupMembersResultDto,
  AiApprovalDecision,
  AiCliProbeDto,
  AiComposeDraftEvent,
  AiConfigDto,
  AiConfigUpdateDto,
  AiEmergencyStopResult,
  AiInstallZeroClawResponseDto,
  AiMcpStatusDto,
  AiOpenThreadEvent,
  AiStoredMessageDto,
  AiStreamDeltaEvent,
  AiSummaryDeltaEvent,
  AiSummaryDto,
  AiToolActivityEvent,
  AiToolApprovalRequest,
  AiToolApprovalResolvedEvent,
  AiToolAuditEntry,
  ApiAuditLogDto,
  ApiClientDto,
  AppUpdateInfo,
  AttachmentMeta,
  CallAudioTestResultDto,
  CallDevicesDto,
  CallStateDto,
  ChatFolderDto,
  CreateApiClientInput,
  CreateScheduledMessageInput,
  CreatedApiClientDto,
  FolderFilter,
  FriendActionResultDto,
  FriendRequestItemDto,
  FriendRequestsListDto,
  GDriveCloudItemDto,
  GDriveConfigDto,
  GDriveConfigPatch,
  GDrivePruneResultDto,
  GDriveStatusDto,
  GDriveStorageOverviewDto,
  GDriveThreadUsageDto,
  KickGroupMembersResultDto,
  LeaveGroupResultDto,
  LocalApiConfigDto,
  LocalApiStatusDto,
  MediaVaultConfigDto,
  MediaVaultStatsDto,
  MessageDto,
  MessagesAroundResult,
  NotifyToastPayload,
  PollDto,
  RelayConfigSummary,
  RelayConfigUpdate,
  RelayTestResult,
  SavedFileResultDto,
  ScheduledMessageDto,
  ScreenCaptureDto,
  SocialWsEventDto,
  StickerItem,
  ThreadDetailsDto,
  ThreadDto,
  ThreadMemberDto,
  TranslateConfigSummary,
  TranslateConfigUpdate,
  TranslateProvider,
  TranslationResult,
  UpdateApiClientInput,
  UpdateScheduledMessageInput,
  UserProfileDto,
  WebAppAccountDto,
  WebAppAccountInput,
  ZeroClawInstallProgressDto,
} from "./types";
import {
  DEFAULT_AI_SYSTEM_PROMPT,
  DEFAULT_CLOUD_API_BASE_URL,
  DEFAULT_CLOUD_API_MODEL,
  DEFAULT_LOCAL_API_MIN_INTERVAL_MS,
  DEFAULT_LOCAL_API_PORT,
  DEFAULT_ZEROCLAW_MODEL,
  DEFAULT_ZEROCLAW_PATH,
  detectAiCloudProvider,
  getAiCloudProviderPreset,
  type AiCommandResultDto,
  type AiModelInfoDto,
  type AiSessionDto,
} from "./utils/automationHelpers.ts";

export type {
  AiCliProbeDto,
  AiCommandResultDto,
  AiConfigDto,
  AiConfigUpdateDto,
  AiInstallZeroClawResponseDto,
  AiModelInfoDto,
  AiSessionDto,
  AiStoredMessageDto,
  AiSummaryDto,
} from "./utils/automationHelpers.ts";
import { matchesSearchQuery } from "./utils/searchNormalize.ts";
import { filterDemoWebAppAccounts, upsertDemoWebAppAccount } from "./utils/webappAccounts.ts";

function inferDownloadExtension(targetUrl: string): string | null {
  const dataMatch = targetUrl.match(/^data:([a-z0-9.+-]+)\/([a-z0-9.+-]+)(?:;|,)/i);
  if (dataMatch) {
    const type = dataMatch[1].toLowerCase();
    const sub = dataMatch[2].toLowerCase();
    if (sub === "jpeg" || sub === "jpg" || sub === "jfif") return "jpg";
    if (sub === "x-png" || sub === "apng") return "png";
    if (sub === "svg+xml") return "svg";
    if (sub === "mpeg") return type === "audio" ? "mp3" : "mpg";
    if (sub === "quicktime") return "mov";
    if (sub === "x-matroska") return "mkv";
    if (sub === "x-msvideo") return "avi";
    if (sub === "x-wav") return "wav";
    if (sub === "mp4") return type === "audio" ? "m4a" : "mp4";
    if (/^[a-z0-9]{2,8}$/.test(sub)) return sub;
  }
  try {
    let urlToInspect = targetUrl;
    if (/^(?:teza-media:\/\/|https?:\/\/teza-media\.localhost\/)/i.test(urlToInspect)) {
      const parsed = new URL(urlToInspect);
      const inner = parsed.searchParams.get("url");
      if (inner) urlToInspect = inner;
    }
    const parsed = new URL(urlToInspect);
    const decodedPath = decodeURIComponent(parsed.pathname || "");
    const extMatch = decodedPath.match(/\.([A-Za-z0-9]{2,8})$/);
    if (extMatch) {
      const ext = extMatch[1].toLowerCase();
      return ext === "jpeg" || ext === "jfif" ? "jpg" : ext;
    }
  } catch {
    // Ignore malformed URLs in browser fallback
  }
  return null;
}

function ensureDownloadFileName(suggestedName: string, targetUrl: string): string {
  const clean = (suggestedName || "").trim() || "download";
  if (/\.[A-Za-z0-9]{2,8}$/.test(clean)) return clean;
  const inferred = inferDownloadExtension(targetUrl);
  return inferred ? `${clean}.${inferred}` : clean;
}

let DEMO_AI_CONFIG: AiConfigDto = {
  enabled: false,
  backend: "zeroclaw",
  model: DEFAULT_ZEROCLAW_MODEL,
  api_base_url: DEFAULT_CLOUD_API_BASE_URL,
  has_api_key: false,
  api_key_preview: "",
  zeroclaw_path: DEFAULT_ZEROCLAW_PATH,
  zeroclaw_gateway_url: "",
  has_zeroclaw_token: false,
  zeroclaw_token_preview: "",
  zeroclaw_safe_tools: false,
  teza_access_level: "isolated",
  system_access_level: "safe",
  zeroclaw_allowed_roots: [],
  zeroclaw_allowed_thread_ids: [],
  system_prompt: DEFAULT_AI_SYSTEM_PROMPT,
  summary_language: "vi",
  max_summary_messages: 40,
};
const DEMO_AI_INIT_TS = Date.now();
let DEMO_AI_SESSIONS: AiSessionDto[] = [
  {
    id: "default",
    title: "Cuộc trò chuyện AI",
    created_ts: DEMO_AI_INIT_TS,
    updated_ts: DEMO_AI_INIT_TS,
    message_count: 0,
    last_preview: "",
  },
];
let DEMO_AI_MESSAGES: AiStoredMessageDto[] = [];
let DEMO_AI_PENDING_APPROVALS: AiToolApprovalRequest[] = [];
const DEMO_AI_TOOL_AUDIT_LOGS: AiToolAuditEntry[] = [];
const DEMO_AI_STREAM_LISTENERS = new Set<(ev: AiStreamDeltaEvent) => void>();
const DEMO_AI_SUMMARY_LISTENERS = new Set<(ev: AiSummaryDeltaEvent) => void>();
const DEMO_AI_TOOL_ACTIVITY_LISTENERS = new Set<(ev: AiToolActivityEvent) => void>();
const DEMO_AI_TOOL_APPROVAL_LISTENERS = new Set<(ev: AiToolApprovalRequest) => void>();
const DEMO_AI_TOOL_APPROVAL_RESOLVED_LISTENERS = new Set<
  (ev: AiToolApprovalResolvedEvent) => void
>();
const DEMO_AI_COMPOSE_DRAFT_LISTENERS = new Set<(ev: AiComposeDraftEvent) => void>();
const DEMO_AI_OPEN_THREAD_LISTENERS = new Set<(ev: AiOpenThreadEvent) => void>();

function normalizeDemoAiPreview(raw: string, maxLen = 96): string {
  const collapsed = (raw || "").replace(/\s+/g, " ").trim();
  if (!collapsed) return "";
  return collapsed.length > maxLen ? `${collapsed.slice(0, maxLen - 1)}…` : collapsed;
}

function deriveDemoAiSessionTitle(raw: string): string {
  const collapsed = (raw || "").replace(/\s+/g, " ").trim();
  if (!collapsed) return "Cuộc trò chuyện mới";
  return collapsed.length > 48 ? `${collapsed.slice(0, 47)}…` : collapsed;
}

function hydrateDemoAiSession(session: AiSessionDto): AiSessionDto {
  const msgs = DEMO_AI_MESSAGES.filter((m) => m.session_id === session.id);
  const lastMsg = msgs.length > 0 ? msgs[msgs.length - 1] : null;
  return {
    ...session,
    message_count: msgs.length,
    last_preview: lastMsg ? normalizeDemoAiPreview(lastMsg.content) : "",
  };
}

function touchDemoAiSessionOnMessage(msg: AiStoredMessageDto): void {
  const idx = DEMO_AI_SESSIONS.findIndex((s) => s.id === msg.session_id);
  if (idx < 0) {
    const created: AiSessionDto = {
      id: msg.session_id,
      title: msg.role === "user" ? deriveDemoAiSessionTitle(msg.content) : "Cuộc trò chuyện mới",
      created_ts: msg.ts,
      updated_ts: msg.ts,
      message_count: 1,
      last_preview: normalizeDemoAiPreview(msg.content),
    };
    DEMO_AI_SESSIONS = [created, ...DEMO_AI_SESSIONS];
    return;
  }
  const existing = DEMO_AI_SESSIONS[idx];
  const shouldAutoTitle =
    msg.role === "user" &&
    (!existing.title.trim() || existing.title.trim() === "Cuộc trò chuyện mới");
  const updated: AiSessionDto = {
    ...existing,
    title: shouldAutoTitle ? deriveDemoAiSessionTitle(msg.content) : existing.title,
    updated_ts: Math.max(existing.updated_ts, msg.ts),
  };
  DEMO_AI_SESSIONS = DEMO_AI_SESSIONS.map((s) =>
    s.id === msg.session_id ? hydrateDemoAiSession(updated) : s,
  );
}

let DEMO_SCHEDULES: ScheduledMessageDto[] = [];
const DEMO_SCHEDULE_LISTENERS = new Set<(ev: ScheduledMessageDto | null) => void>();

let DEMO_LOCAL_API_CONFIG: LocalApiConfigDto = {
  enabled: false,
  port: DEFAULT_LOCAL_API_PORT,
  min_send_interval_ms: DEFAULT_LOCAL_API_MIN_INTERVAL_MS,
};
let DEMO_API_CLIENTS: ApiClientDto[] = [];
let DEMO_API_AUDIT_LOGS: ApiAuditLogDto[] = [];
const DEMO_LOCAL_API_AUDIT_LISTENERS = new Set<(ev: ApiAuditLogDto) => void>();

function emitDemoScheduleUpdated(item: ScheduledMessageDto | null) {
  for (const fn of DEMO_SCHEDULE_LISTENERS) {
    try {
      fn(item);
    } catch {
      // Ignore demo listener error
    }
  }
}

export const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const MAX_ATTACHMENT_BYTES = 100 * 1024 * 1024;
const RAW_IPC_SINGLE_SHOT_MAX_BYTES = 2 * 1024 * 1024;
const RAW_IPC_CHUNK_BYTES = 2 * 1024 * 1024;
const BASE64_CHUNK_SIZE = 0x8000;

function uint8ArrayToBase64(bytes: Uint8Array): string {
  if (bytes.byteLength === 0) return "";
  const chunks: string[] = [];
  for (let offset = 0; offset < bytes.byteLength; offset += BASE64_CHUNK_SIZE) {
    const slice = bytes.subarray(offset, Math.min(offset + BASE64_CHUNK_SIZE, bytes.byteLength));
    chunks.push(String.fromCharCode(...slice));
  }
  return btoa(chunks.join(""));
}

export type AuthStatusResponse =
  | { status: "needs_qr" }
  | { status: "expired"; reason: string }
  | { status: "authenticated"; profile: AccountProfile };

export interface QrStartResponse {
  token_id: string;
  qr_svg: string;
  qr_timeout: number;
}

export interface MessagesPage {
  messages: MessageDto[];
  has_more: boolean;
}

const DEMO_THREADS: ThreadDto[] = [
  {
    id: "cloud-me",
    title: "Saved Messages",
    avatar: "",
    kind: "cloud",
    last_preview: "Ghi chú cá nhân & file đã lưu",
    last_ts: Date.now() - 60_000,
    unread_count: 0,
    is_pinned: true,
    is_muted: false,
    last_read_in_ts: 0,
    peer_read_ts: 0,
    muted_until_ts: 0,
    is_archived: false,
  },
  {
    id: "grp-rust-vn",
    title: "Nhóm Kỹ thuật Desktop",
    avatar: "",
    kind: "group",
    last_preview: "An: Đã cập nhật bộ gõ tiếng Việt IME!",
    last_ts: Date.now() - 300_000,
    unread_count: 2,
    is_pinned: true,
    is_muted: false,
    last_read_in_ts: 0,
    peer_read_ts: 0,
    muted_until_ts: 0,
    is_archived: false,
  },
  {
    id: "dm-minh",
    title: "Trần Minh",
    avatar: "",
    kind: "direct",
    last_preview: "Chiều nay họp tổng kết dự án nhé!",
    last_ts: Date.now() - 1_200_000,
    unread_count: 1,
    is_pinned: false,
    is_muted: false,
    last_read_in_ts: 0,
    peer_read_ts: 0,
    muted_until_ts: 0,
    is_archived: false,
  },
];

const DEMO_FOLDERS: ChatFolderDto[] = [
  {
    id: "work-demo",
    name: "Công việc",
    icon: "briefcase",
    color: "#cfa13a",
    thread_ids: ["grp-rust-vn"],
    excluded_thread_ids: [],
    include_kinds: [],
    only_unread: false,
    exclude_muted: false,
    position: 1,
    remote_label_id: null,
  },
];

const DEMO_MESSAGES: Record<string, MessageDto[]> = {
  "grp-rust-vn": [
    {
      msg_id: "d1",
      thread_id: "grp-rust-vn",
      sender_id: "u2",
      sender_name: "An",
      text: "Giao diện 3 cột mới gọn hơn hẳn bản cũ.",
      msg_type: "chat.text",
      ts: Date.now() - 360_000,
      is_outgoing: false,
      is_recalled: false,
      is_pinned: true,
      quote: null,
      reactions: [{ emoji: "👍", count: 4, me: true }],
      attachment_url: null,
      attachment_name: null,
    },
    {
      msg_id: "d2",
      thread_id: "grp-rust-vn",
      sender_id: "me",
      sender_name: "Bạn",
      text: "Chuẩn luôn, bấm Ctrl+K tìm tin nhắn không dấu tức thì.",
      msg_type: "chat.text",
      ts: Date.now() - 240_000,
      is_outgoing: true,
      is_recalled: false,
      is_pinned: false,
      quote: {
        msg_id: "d1",
        sender_name: "An",
        snippet: "Giao diện 3 cột mới gọn hơn hẳn bản cũ.",
      },
      reactions: [],
      attachment_url: null,
      attachment_name: null,
    },
  ],
};

let DEMO_WEBAPP_ACCOUNTS: WebAppAccountDto[] = [];

let DEMO_MEDIA_VAULT_CONFIG: MediaVaultConfigDto = {
  custom_vault_dir: null,
  active_vault_dir: "LocalAppData/Teza/media/demo",
  quota_mb: 1024,
  encrypt_at_rest: true,
  auto_cache_images: true,
  auto_cache_voice: true,
  auto_cache_video_files: false,
};

let DEMO_MEDIA_VAULT_STATS: MediaVaultStatsDto = {
  total_bytes: 0,
  object_count: 0,
  pinned_bytes: 0,
  pinned_count: 0,
  image_bytes: 0,
  video_bytes: 0,
  voice_bytes: 0,
  file_bytes: 0,
  thumb_bytes: 0,
  cloud_synced_count: 0,
  cloud_total_bytes: 0,
};

let DEMO_TRANSLATE_CONFIG: TranslateConfigSummary = {
  provider: "deepl",
  detect_mode: "api_cache",
  target_lang: "VI",
  has_deepl_key: false,
  deepl_preview: "",
};

let DEMO_GDRIVE_CONFIG: GDriveConfigDto = {
  client_id: "",
  has_client_secret: false,
  has_builtin_oauth: true,
  token_broker_url: "",
  has_builtin_token_broker: true,
  oauth_mode: "builtin_broker",
  sync_enabled: true,
  sync_media: true,
  sync_history: true,
  auto_sync_interval_secs: 300,
};

let DEMO_GDRIVE_STATUS: GDriveStatusDto = {
  relay_blocked: false,
  oauth_configured: true,
  connected: false,
  account_email: null,
  account_name: null,
  root_folder_id: null,
  vault_unlocked: false,
  has_remote_manifest: false,
  sync_enabled: true,
  sync_media: true,
  sync_history: true,
  syncing: false,
  phase: "idle",
  progress_done: 0,
  progress_total: 0,
  pending_media_count: 2,
  pending_media_bytes: 18_432_000,
  last_sync_ts: null,
  last_history_sync_ts: null,
  last_error: null,
};

const DEMO_GDRIVE_HISTORY_BYTES = 1_245_184;
const DEMO_GDRIVE_HISTORY_FILE_COUNT = 3;

let DEMO_GDRIVE_CLOUD_ITEMS: GDriveCloudItemDto[] = [
  {
    sha256: "demo-sha256-video-01",
    cloud_file_id: "gd-demo-file-01",
    kind: "video",
    mime: "video/mp4",
    size_bytes: 48_234_496,
    original_name: "sprint-demo-recording.mp4",
    thread_id: "grp-rust-vn",
    thread_name: "Nhóm Kỹ thuật Desktop",
    msg_id: "d-vid-1",
    cloud_synced_ts: Date.now() - 120 * 86_400_000,
    local_state: 0,
  },
  {
    sha256: "demo-sha256-file-02",
    cloud_file_id: "gd-demo-file-02",
    kind: "file",
    mime: "application/pdf",
    size_bytes: 14_680_064,
    original_name: "architecture-spec-v2.pdf",
    thread_id: "grp-rust-vn",
    thread_name: "Nhóm Kỹ thuật Desktop",
    msg_id: "d-file-2",
    cloud_synced_ts: Date.now() - 45 * 86_400_000,
    local_state: 1,
  },
  {
    sha256: "demo-sha256-image-03",
    cloud_file_id: "gd-demo-file-03",
    kind: "image",
    mime: "image/png",
    size_bytes: 4_194_304,
    original_name: "ui-wireframe-final.png",
    thread_id: "dm-minh",
    thread_name: "Trần Minh",
    msg_id: "d-img-3",
    cloud_synced_ts: Date.now() - 10 * 86_400_000,
    local_state: 1,
  },
  {
    sha256: "demo-sha256-voice-04",
    cloud_file_id: "gd-demo-file-04",
    kind: "voice",
    mime: "audio/ogg",
    size_bytes: 1_572_864,
    original_name: "voice-note-01.ogg",
    thread_id: "cloud-me",
    thread_name: "Saved Messages",
    msg_id: "d-voice-4",
    cloud_synced_ts: Date.now() - 200 * 86_400_000,
    local_state: 1,
  },
];

const DEMO_GDRIVE_LISTENERS = new Set<(status: GDriveStatusDto) => void>();

const DEMO_FRIENDS = new Set<string>(["dm-minh"]);
const DEMO_BLOCKED = new Set<string>();
let DEMO_FRIEND_REQUESTS_RECEIVED: FriendRequestItemDto[] = [
  {
    user_id: "u-linh",
    display_name: "Nguyễn Linh",
    account_name: "Nguyễn Linh",
    avatar: "",
    message: "Xin chào, kết bạn với mình nhé!",
    created_ts: Date.now() - 1_800_000,
    direction: "received",
    is_seen: false,
  },
];
let DEMO_FRIEND_REQUESTS_SENT: FriendRequestItemDto[] = [
  {
    user_id: "u-huy",
    display_name: "Phạm Huy",
    account_name: "Phạm Huy",
    avatar: "",
    message: "Chào bạn, mình kết bạn trên Teza nhé!",
    created_ts: Date.now() - 3_600_000,
    direction: "sent",
    is_seen: true,
  },
];
const DEMO_GROUP_MEMBERS: Record<
  string,
  { creator_id: string; admin_ids: string[]; members: ThreadMemberDto[] }
> = {
  "grp-rust-vn": {
    creator_id: "me",
    admin_ids: ["u2"],
    members: [
      { id: "me", name: "Bạn", avatar: "", role: "Trưởng nhóm" },
      { id: "u2", name: "An", avatar: "", role: "Phó nhóm" },
      { id: "dm-minh", name: "Trần Minh", avatar: "", role: "Thành viên" },
    ],
  },
};
const DEMO_SOCIAL_LISTENERS = new Set<(ev: SocialWsEventDto) => void>();

function emitDemoSocialEvent(ev: SocialWsEventDto): void {
  const snapshot: SocialWsEventDto = { ...ev };
  for (const listener of DEMO_SOCIAL_LISTENERS) {
    listener(snapshot);
  }
}

function emitDemoGDriveStatus(): GDriveStatusDto {
  const snapshot: GDriveStatusDto = { ...DEMO_GDRIVE_STATUS };
  for (const listener of DEMO_GDRIVE_LISTENERS) {
    listener(snapshot);
  }
  return snapshot;
}

function buildDemoGDriveOverview(limit = 50): GDriveStorageOverviewDto {
  let imagesBytes = 0;
  let videosBytes = 0;
  let voiceBytes = 0;
  let filesBytes = 0;
  const threadMap = new Map<string, GDriveThreadUsageDto>();

  for (const item of DEMO_GDRIVE_CLOUD_ITEMS) {
    if (item.kind === "image" || item.kind === "sticker") {
      imagesBytes += item.size_bytes;
    } else if (item.kind === "video") {
      videosBytes += item.size_bytes;
    } else if (item.kind === "voice" || item.kind === "audio") {
      voiceBytes += item.size_bytes;
    } else {
      filesBytes += item.size_bytes;
    }

    const tid = item.thread_id ?? "unknown";
    const existing = threadMap.get(tid);
    if (existing) {
      existing.total_bytes += item.size_bytes;
      existing.object_count += 1;
    } else {
      threadMap.set(tid, {
        thread_id: tid,
        thread_name: item.thread_name ?? tid,
        total_bytes: item.size_bytes,
        object_count: 1,
      });
    }
  }

  const mediaTotalBytes = imagesBytes + videosBytes + voiceBytes + filesBytes;
  const tezaTotalBytes = mediaTotalBytes + DEMO_GDRIVE_HISTORY_BYTES;
  const byThread = [...threadMap.values()].sort((a, b) => b.total_bytes - a.total_bytes);
  const largestItems = [...DEMO_GDRIVE_CLOUD_ITEMS]
    .sort((a, b) => b.size_bytes - a.size_bytes)
    .slice(0, Math.max(1, limit));

  return {
    quota: {
      drive_limit_bytes: 15 * 1024 * 1024 * 1024,
      drive_usage_bytes: 2_684_354_560 + tezaTotalBytes,
      drive_usage_in_drive_bytes: 2_516_582_400 + tezaTotalBytes,
      drive_usage_in_trash_bytes: 167_772_160,
    },
    teza_cloud_total_bytes: tezaTotalBytes,
    teza_cloud_object_count: DEMO_GDRIVE_CLOUD_ITEMS.length,
    images_bytes: imagesBytes,
    videos_bytes: videosBytes,
    voice_bytes: voiceBytes,
    files_bytes: filesBytes,
    history_bytes: DEMO_GDRIVE_HISTORY_BYTES,
    history_file_count: DEMO_GDRIVE_HISTORY_FILE_COUNT,
    by_thread: byThread,
    largest_items: largestItems,
  };
}

function touchDemoThread(threadId: string, preview: string, ts: number) {
  const thread = DEMO_THREADS.find((t) => t.id === threadId);
  if (thread) {
    thread.last_preview = preview.startsWith("Bạn:") ? preview : `Bạn: ${preview}`;
    thread.last_ts = Math.max(thread.last_ts, ts);
  }
}

export const bridge = {
  async authStatus(): Promise<AuthStatusResponse> {
    if (!isTauri()) return { status: "needs_qr" };
    return invoke<AuthStatusResponse>("auth_get_status");
  },
  async startQr(): Promise<QrStartResponse> {
    if (!isTauri()) {
      return { token_id: "preview", qr_svg: "", qr_timeout: 180 };
    }
    return invoke<QrStartResponse>("auth_start_qr");
  },
  async logout(): Promise<void> {
    if (!isTauri()) return;
    return invoke("auth_logout");
  },
  async listThreads(folder: FolderFilter): Promise<ThreadDto[]> {
    if (!isTauri()) {
      return DEMO_THREADS.filter((t) => {
        if (folder === "unread") return t.unread_count > 0;
        if (folder === "direct") return t.kind === "direct";
        if (folder === "group") return t.kind === "group";
        if (folder === "cloud") return t.kind === "cloud";
        return true;
      }).sort(
        (a, b) =>
          (b.is_pinned ? 1 : 0) - (a.is_pinned ? 1 : 0) ||
          b.last_ts - a.last_ts ||
          a.id.localeCompare(b.id),
      );
    }
    return invoke<ThreadDto[]>("threads_list", { folder });
  },
  async syncThreads(folder: FolderFilter): Promise<ThreadDto[]> {
    if (!isTauri()) return this.listThreads(folder);
    return invoke<ThreadDto[]>("threads_sync", { folder });
  },
  async ensureDirectThread(threadId: string, title: string, avatar?: string): Promise<ThreadDto> {
    if (!isTauri()) {
      const cleanId = threadId.trim();
      const cleanTitle = title.trim() || cleanId;
      const cleanAvatar = (avatar ?? "").trim();
      const existing = DEMO_THREADS.find((t) => t.id === cleanId);
      if (existing) {
        if (cleanTitle && (!existing.title || existing.title === existing.id)) {
          existing.title = cleanTitle;
        }
        if (cleanAvatar && !existing.avatar) {
          existing.avatar = cleanAvatar;
        }
        return { ...existing };
      }
      const created: ThreadDto = {
        id: cleanId,
        title: cleanTitle,
        avatar: cleanAvatar,
        kind: "direct",
        last_preview: "",
        last_ts: 0,
        unread_count: 0,
        is_pinned: false,
        is_muted: false,
        last_read_in_ts: 0,
        peer_read_ts: 0,
        muted_until_ts: 0,
        is_archived: false,
      };
      DEMO_THREADS.unshift(created);
      return { ...created };
    }
    return invoke<ThreadDto>("thread_ensure_direct", {
      threadId,
      title,
      avatar: avatar ?? null,
    });
  },
  async setActiveThread(threadId: string | null): Promise<void> {
    if (!isTauri()) return;
    return invoke("thread_set_active", { threadId });
  },
  async markRead(threadId: string): Promise<void> {
    if (!isTauri()) {
      const thread = DEMO_THREADS.find((t) => t.id === threadId);
      if (thread) thread.unread_count = 0;
      return;
    }
    return invoke("thread_mark_read", { threadId });
  },
  async markUnread(threadId: string): Promise<void> {
    if (!isTauri()) {
      const thread = DEMO_THREADS.find((t) => t.id === threadId);
      if (thread) thread.unread_count = Math.max(1, thread.unread_count);
      return;
    }
    return invoke("thread_mark_unread", { threadId });
  },
  async togglePin(threadId: string, pinned: boolean): Promise<void> {
    if (!isTauri()) {
      const thread = DEMO_THREADS.find((t) => t.id === threadId);
      if (thread) thread.is_pinned = pinned;
      return;
    }
    return invoke("thread_toggle_pin", { threadId, pinned });
  },
  async toggleMute(threadId: string, muted: boolean, durationSecs?: number): Promise<number> {
    const until =
      muted && durationSecs && durationSecs > 0 ? Date.now() + durationSecs * 1000 : 0;
    if (!isTauri()) {
      const thread = DEMO_THREADS.find((t) => t.id === threadId);
      if (thread) {
        thread.is_muted = muted;
        thread.muted_until_ts = until;
      }
      return until;
    }
    return invoke<number>("thread_toggle_mute", {
      threadId,
      muted,
      durationSecs: durationSecs ?? null,
    });
  },
  async toggleArchive(threadId: string, archived: boolean): Promise<void> {
    if (!isTauri()) {
      const thread = DEMO_THREADS.find((t) => t.id === threadId);
      if (thread) thread.is_archived = archived;
      return;
    }
    return invoke("thread_toggle_archive", { threadId, archived });
  },
  async showNotifyPopup(payload: NotifyToastPayload): Promise<void> {
    if (!isTauri()) return;
    return invoke("notify_show_popup", { payload });
  },
  async getCurrentNotifyPopup(): Promise<NotifyToastPayload | null> {
    if (!isTauri()) return null;
    return invoke<NotifyToastPayload | null>("notify_get_current");
  },
  async dismissNotifyPopup(): Promise<void> {
    if (!isTauri()) return;
    return invoke("notify_dismiss");
  },
  async clickNotifyPopup(threadId: string): Promise<void> {
    if (!isTauri()) return;
    return invoke("notify_click", { threadId });
  },
  async buzzShakeScreen(): Promise<void> {
    if (!isTauri()) return;
    return invoke("buzz_shake_screen");
  },
  async getThreadDetails(threadId: string): Promise<ThreadDetailsDto> {
    if (!isTauri()) {
      const msgs = DEMO_MESSAGES[threadId] ?? [];
      const group = DEMO_GROUP_MEMBERS[threadId];
      return {
        thread_id: threadId,
        total_members: group ? group.members.length : null,
        creator_id: group?.creator_id ?? "",
        admin_ids: group ? [...group.admin_ids] : [],
        members: group ? group.members.map((m) => ({ ...m })) : [],
        media_items: msgs.filter((m) => !!m.attachment_url && m.msg_type.includes("photo")),
        file_items: msgs.filter((m) => !!m.attachment_url && !m.msg_type.includes("photo")),
        link_items: msgs.filter((m) => /https?:\/\//i.test(m.text)),
      };
    }
    return invoke<ThreadDetailsDto>("thread_get_details", { threadId });
  },
  async listChatFolders(): Promise<ChatFolderDto[]> {
    if (!isTauri()) return [...DEMO_FOLDERS].sort((a, b) => a.position - b.position);
    return invoke<ChatFolderDto[]>("folders_list");
  },
  async saveChatFolder(folder: ChatFolderDto): Promise<ChatFolderDto[]> {
    if (!isTauri()) {
      const idx = DEMO_FOLDERS.findIndex((f) => f.id === folder.id);
      if (idx >= 0) DEMO_FOLDERS[idx] = folder;
      else DEMO_FOLDERS.push(folder);
      return [...DEMO_FOLDERS].sort((a, b) => a.position - b.position);
    }
    return invoke<ChatFolderDto[]>("folder_save", { folder });
  },
  async deleteChatFolder(folderId: string): Promise<ChatFolderDto[]> {
    if (!isTauri()) {
      const idx = DEMO_FOLDERS.findIndex((f) => f.id === folderId);
      if (idx >= 0) DEMO_FOLDERS.splice(idx, 1);
      return [...DEMO_FOLDERS].sort((a, b) => a.position - b.position);
    }
    return invoke<ChatFolderDto[]>("folder_delete", { folderId });
  },
  async reorderChatFolders(orderedIds: string[]): Promise<ChatFolderDto[]> {
    if (!isTauri()) {
      const posMap = new Map<string, number>();
      orderedIds.forEach((id, idx) => posMap.set(id, idx + 1));
      for (const f of DEMO_FOLDERS) {
        const nextPos = posMap.get(f.id);
        if (nextPos !== undefined) f.position = nextPos;
      }
      return [...DEMO_FOLDERS].sort((a, b) => a.position - b.position);
    }
    return invoke<ChatFolderDto[]>("folders_reorder", { orderedIds });
  },
  async syncRemoteLabelFolders(): Promise<ChatFolderDto[]> {
    if (!isTauri()) return [...DEMO_FOLDERS].sort((a, b) => a.position - b.position);
    return invoke<ChatFolderDto[]>("folders_sync_remote_labels");
  },
  async getUserProfile(userId: string): Promise<UserProfileDto> {
    if (!isTauri()) {
      const t = DEMO_THREADS.find((x) => x.id === userId);
      const req =
        DEMO_FRIEND_REQUESTS_RECEIVED.find((x) => x.user_id === userId) ??
        DEMO_FRIEND_REQUESTS_SENT.find((x) => x.user_id === userId);
      const name = t?.title ?? req?.display_name ?? `Người dùng ${userId}`;
      return {
        uid: userId,
        display_name: name,
        account_name: req?.account_name ?? name,
        avatar: t?.avatar ?? req?.avatar ?? "",
        cover: "",
        status: "Đang hoạt động trên Teza (Chế độ xem trước)",
        gender: null,
        sdob: "",
        phone_number: "",
        is_friend: DEMO_FRIENDS.has(userId),
        is_blocked: DEMO_BLOCKED.has(userId),
        is_self: userId === "cloud-me" || userId === "me",
        mutual_groups: DEMO_THREADS.filter((g) => g.kind === "group").map((g) => ({
          id: g.id,
          title: g.title,
          avatar: g.avatar,
          total_members: DEMO_GROUP_MEMBERS[g.id]?.members.length ?? 12,
        })),
        local_message_count: (DEMO_MESSAGES[userId] ?? []).length,
        local_media_count: 0,
        last_interaction_ts: t?.last_ts ?? 0,
      };
    }
    return invoke<UserProfileDto>("user_get_profile", { userId });
  },
  async findUserByPhone(phone: string, countryCode?: string): Promise<UserProfileDto> {
    if (!isTauri()) {
      const trimmed = phone.trim();
      const hasPlus = trimmed.startsWith("+");
      const cleaned = trimmed.replace(/[\s\-().\t+]/g, "");
      if (!/^\d{7,15}$/.test(cleaned)) {
        throw new Error("Invalid phone number");
      }
      const cc = (countryCode || "84").replace(/\D/g, "") || "84";
      let norm = cleaned;
      if (hasPlus) {
        if ((norm.startsWith("82") || norm.startsWith("84")) && norm[2] === "0") {
          norm = norm.slice(0, 2) + norm.slice(3);
        }
      } else if (norm.startsWith("0")) {
        norm = `${cc}${norm.slice(1)}`;
      } else if (!norm.startsWith(cc) && !norm.startsWith("84") && !norm.startsWith("82")) {
        norm = `${cc}${norm}`;
      }
      const uid = `phone-${norm}`;
      return {
        uid,
        display_name: `Contact +${norm}`,
        account_name: `Contact +${norm}`,
        avatar: "",
        cover: "",
        status: "Phone lookup (Preview mode)",
        gender: null,
        sdob: "",
        phone_number: `+${norm}`,
        is_friend: DEMO_FRIENDS.has(uid),
        is_blocked: DEMO_BLOCKED.has(uid),
        is_self: false,
        mutual_groups: [],
        local_message_count: 0,
        local_media_count: 0,
        last_interaction_ts: 0,
      };
    }
    const cleanCountry = countryCode?.trim();
    return invoke<UserProfileDto>("user_find_by_phone", {
      phone,
      ...(cleanCountry ? { countryCode: cleanCountry } : {}),
    });
  },
  async listPinnedMessages(threadId: string): Promise<MessageDto[]> {
    if (!isTauri()) {
      return (DEMO_MESSAGES[threadId] ?? []).filter((m) => m.is_pinned && !m.is_recalled);
    }
    return invoke<MessageDto[]>("messages_list_pinned", { threadId });
  },
  async loadMessages(
    threadId: string,
    beforeTs?: number,
    syncRemote = false,
    beforeMsgId?: string,
  ): Promise<MessagesPage> {
    if (!isTauri()) {
      return { messages: DEMO_MESSAGES[threadId] ?? [], has_more: false };
    }
    return invoke<MessagesPage>("messages_load", {
      threadId,
      beforeTs: beforeTs ?? null,
      beforeMsgId: beforeMsgId ?? null,
      limit: 50,
      syncRemote,
    });
  },
  async loadMessagesAround(threadId: string, targetTs: number): Promise<MessagesAroundResult> {
    if (!isTauri()) {
      const all = DEMO_MESSAGES[threadId] ?? [];
      const sorted = [...all].sort((a, b) => a.ts - b.ts);
      let idx = sorted.findIndex((m) => m.ts >= targetTs);
      if (idx < 0) idx = Math.max(0, sorted.length - 1);
      const start = Math.max(0, idx - 25);
      const end = Math.min(sorted.length, idx + 1001);
      return {
        messages: sorted.slice(start, end),
        has_more: start > 0,
        has_newer: end < sorted.length,
      };
    }
    const res = await invoke<
      | (MessagesAroundResult & { has_more?: boolean })
      | (MessagesPage & { has_newer?: boolean })
      | MessageDto[]
    >("messages_load_around", { threadId, targetTs });
    if (Array.isArray(res)) {
      return { messages: res, has_more: false, has_newer: false };
    }
    return {
      messages: Array.isArray(res.messages) ? res.messages : [],
      has_more: Boolean(res.has_more),
      has_newer: Boolean(res.has_newer),
    };
  },
  async getMessageById(threadId: string, msgId: string): Promise<MessageDto | null> {
    if (!isTauri()) {
      return (DEMO_MESSAGES[threadId] ?? []).find((m) => m.msg_id === msgId) ?? null;
    }
    return invoke<MessageDto | null>("message_get_by_id", { threadId, msgId });
  },
  async sendText(threadId: string, text: string): Promise<MessageDto> {
    if (!isTauri()) {
      const msg: MessageDto = {
        msg_id: `preview-${Date.now()}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text,
        msg_type: "chat.text",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: null,
        attachment_name: null,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("message_send_text", { threadId, text });
  },
  async sendQuote(threadId: string, text: string, quoted: MessageDto): Promise<MessageDto> {
    if (!isTauri()) {
      const msg: MessageDto = {
        msg_id: `preview-${Date.now()}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text,
        msg_type: "chat.text",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: {
          msg_id: quoted.msg_id,
          sender_name: quoted.sender_name,
          snippet: quoted.text,
        },
        reactions: [],
        attachment_url: null,
        attachment_name: null,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("message_send_quote", {
      threadId,
      text,
      quotedMsgId: quoted.msg_id,
    });
  },
  async forwardMessage(
    sourceThreadId: string,
    sourceMsgId: string,
    targetThreadId: string,
    fallback?: {
      text?: string;
      msgType?: string;
      attachmentUrl?: string | null;
      attachmentName?: string | null;
    },
  ): Promise<MessageDto> {
    if (!isTauri()) {
      const sourceList = DEMO_MESSAGES[sourceThreadId] ?? [];
      const source =
        sourceList.find((m) => m.msg_id === sourceMsgId) ??
        Object.values(DEMO_MESSAGES)
          .flat()
          .find((m) => m.msg_id === sourceMsgId) ??
        null;
      const msgType = source?.msg_type || fallback?.msgType || "chat.text";
      const attachmentUrl = source?.attachment_url ?? fallback?.attachmentUrl ?? null;
      const attachmentName = source?.attachment_name ?? fallback?.attachmentName ?? null;
      const text = source?.text ?? fallback?.text ?? "";
      const now = Date.now();
      const msg: MessageDto = {
        msg_id: `preview-fwd-${now}-${Math.random().toString(36).slice(2, 7)}`,
        thread_id: targetThreadId,
        sender_id: "me",
        sender_name: "Bạn",
        text,
        msg_type: msgType,
        ts: now,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: attachmentUrl,
        attachment_name: attachmentName,
      };
      DEMO_MESSAGES[targetThreadId] = [...(DEMO_MESSAGES[targetThreadId] ?? []), msg];
      const preview =
        text.trim() ||
        (attachmentName ? `[Tập tin] ${attachmentName}` : "") ||
        (msgType.includes("sticker")
          ? "[Nhãn dán]"
          : msgType.includes("photo") || msgType.includes("gif")
            ? "[Hình ảnh]"
            : msgType.includes("video")
              ? "[Video]"
              : msgType.includes("voice")
                ? "[Tin nhắn thoại]"
                : "[Tập tin]");
      touchDemoThread(targetThreadId, preview, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("message_forward", {
      sourceThreadId,
      sourceMsgId,
      targetThreadId,
      fallbackText: fallback?.text ?? null,
      fallbackMsgType: fallback?.msgType ?? null,
      fallbackAttachmentUrl: fallback?.attachmentUrl ?? null,
      fallbackAttachmentName: fallback?.attachmentName ?? null,
    });
  },
  async reactMessage(threadId: string, msgId: string, emoji: string): Promise<MessageDto> {
    if (!isTauri()) {
      const list = DEMO_MESSAGES[threadId] ?? [];
      const target = list.find((m) => m.msg_id === msgId);
      if (!target) throw new Error("Message not found");
      const reactions = target.reactions.map((r) => ({ ...r }));
      const clicked = reactions.find((r) => r.emoji === emoji);
      if (clicked?.me) {
        clicked.me = false;
        clicked.count = Math.max(0, clicked.count - 1);
      } else {
        const prevMe = reactions.find((r) => r.me);
        if (prevMe) {
          prevMe.me = false;
          prevMe.count = Math.max(0, prevMe.count - 1);
        }
        if (clicked) {
          clicked.count += 1;
          clicked.me = true;
        } else {
          reactions.push({ emoji, count: 1, me: true });
        }
      }
      target.reactions = reactions.filter((r) => r.count > 0);
      return { ...target, reactions: [...target.reactions] };
    }
    return invoke<MessageDto>("message_react", { threadId, msgId, emoji });
  },
  async recallMessage(threadId: string, msgId: string): Promise<MessageDto> {
    if (!isTauri()) {
      const list = DEMO_MESSAGES[threadId] ?? [];
      const target = list.find((m) => m.msg_id === msgId);
      if (!target) throw new Error("Message not found");
      target.is_recalled = true;
      target.text = "[Tin nhắn đã thu hồi]";
      target.attachment_url = null;
      target.attachment_name = null;
      return { ...target };
    }
    return invoke<MessageDto>("message_recall", { threadId, msgId });
  },
  async deleteMessageLocal(threadId: string, msgId: string): Promise<void> {
    if (!isTauri()) {
      DEMO_MESSAGES[threadId] = (DEMO_MESSAGES[threadId] ?? []).filter((m) => m.msg_id !== msgId);
      return;
    }
    return invoke("message_delete_local", { threadId, msgId });
  },
  async pickAttachmentFile(): Promise<AttachmentMeta | null> {
    if (!isTauri()) return null;
    return invoke<AttachmentMeta | null>("pick_attachment_file");
  },
  async inspectAttachmentPath(filePath: string): Promise<AttachmentMeta> {
    if (!isTauri()) {
      const name = filePath.split(/[\\/]/).pop() || "attachment.bin";
      return {
        path: filePath,
        file_name: name,
        size_bytes: 1,
        is_image: /\.(png|jpe?g|webp|gif|bmp)$/i.test(name),
      };
    }
    return invoke<AttachmentMeta>("inspect_attachment_path", { filePath });
  },
  async readAttachmentImageDataUrl(filePath: string): Promise<string> {
    const trimmed = filePath.trim();
    if (!isTauri()) {
      if (/^data:image\//i.test(trimmed) || /^blob:/i.test(trimmed)) {
        return trimmed;
      }
      throw new Error("Local file preview is only available in the desktop app.");
    }
    return invoke<string>("read_attachment_image_data_url", { filePath: trimmed });
  },
  async sendAttachmentPath(
    threadId: string,
    filePath: string,
    caption: string,
  ): Promise<MessageDto> {
    if (!isTauri()) {
      const fileName = filePath.split(/[\\/]/).pop() || "attachment.bin";
      const isImage = /\.(png|jpe?g|webp|gif|bmp)$/i.test(fileName);
      const msg: MessageDto = {
        msg_id: `preview-att-${Date.now()}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text: isImage
          ? caption.trim() || "[Hình ảnh]"
          : caption.trim()
            ? `[Tập tin] ${fileName}\n${caption.trim()}`
            : `[Tập tin] ${fileName}`,
        msg_type: isImage ? "chat.photo" : "chat.file",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: null,
        attachment_name: fileName,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("message_send_attachment_path", {
      threadId,
      filePath,
      caption,
    });
  },
  async sendRawAttachment(
    threadId: string,
    fileName: string,
    bytes: Uint8Array,
    caption: string,
    previewUrl?: string | null,
  ): Promise<MessageDto> {
    if (bytes.byteLength === 0) {
      throw new Error("Tệp đính kèm rỗng (0 byte)");
    }
    if (bytes.byteLength > MAX_ATTACHMENT_BYTES) {
      throw new Error(`Tệp vượt quá giới hạn ${MAX_ATTACHMENT_BYTES / (1024 * 1024)} MB`);
    }
    if (!isTauri()) {
      const isImage = /\.(png|jpe?g|webp|gif|bmp)$/i.test(fileName);
      const msg: MessageDto = {
        msg_id: `preview-raw-${Date.now()}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text: isImage
          ? caption.trim() || "[Hình ảnh]"
          : caption.trim()
            ? `[Tập tin] ${fileName}\n${caption.trim()}`
            : `[Tập tin] ${fileName}`,
        msg_type: isImage ? "chat.photo" : "chat.file",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: previewUrl ?? null,
        attachment_name: fileName,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    if (bytes.byteLength <= RAW_IPC_SINGLE_SHOT_MAX_BYTES) {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
      return invoke<MessageDto>("message_send_base64_attachment", {
        threadId,
        fileName,
        caption,
        dataBase64: uint8ArrayToBase64(bytes),
      });
    }
    const uploadId = await invoke<string>("raw_attachment_stage_begin", { fileName });
    try {
      for (let offset = 0; offset < bytes.byteLength; offset += RAW_IPC_CHUNK_BYTES) {
        await new Promise<void>((resolve) => setTimeout(resolve, 0));
        const slice = bytes.subarray(
          offset,
          Math.min(offset + RAW_IPC_CHUNK_BYTES, bytes.byteLength),
        );
        await invoke<number>("raw_attachment_stage_chunk", {
          uploadId,
          chunkBase64: uint8ArrayToBase64(slice),
        });
      }
      return await invoke<MessageDto>("raw_attachment_stage_send", {
        uploadId,
        threadId,
        caption,
      });
    } catch (err) {
      void invoke("raw_attachment_stage_abort", { uploadId }).catch(() => undefined);
      throw err;
    }
  },
  async searchGlobal(query: string): Promise<MessageDto[]> {
    if (!isTauri()) {
      const q = query.trim();
      if (!q) return [];
      return Object.values(DEMO_MESSAGES)
        .flat()
        .filter((m) => matchesSearchQuery(`${m.text}\n${m.sender_name}`, q));
    }
    return invoke<MessageDto[]>("search_global", { query, limit: 30 });
  },
  async getTranslateConfig(): Promise<TranslateConfigSummary> {
    if (!isTauri()) {
      return { ...DEMO_TRANSLATE_CONFIG };
    }
    return invoke<TranslateConfigSummary>("translate_get_config");
  },
  async saveTranslateConfig(update: TranslateConfigUpdate): Promise<TranslateConfigSummary> {
    if (!isTauri()) {
      const clearKey = update.clear_deepl_key === true;
      const newKey = (update.deepl_key ?? "").trim();
      const hasKey = clearKey ? false : newKey ? true : DEMO_TRANSLATE_CONFIG.has_deepl_key;
      const preview = clearKey
        ? ""
        : newKey
          ? `${newKey.slice(0, 4)}****`
          : DEMO_TRANSLATE_CONFIG.deepl_preview;
      DEMO_TRANSLATE_CONFIG = {
        provider: update.provider ?? "deepl",
        detect_mode: update.detect_mode ?? DEMO_TRANSLATE_CONFIG.detect_mode,
        target_lang: update.target_lang,
        has_deepl_key: hasKey,
        deepl_preview: preview,
      };
      return { ...DEMO_TRANSLATE_CONFIG };
    }
    return invoke<TranslateConfigSummary>("translate_save_config", { update });
  },
  async translateMessages(
    items: { msg_id: string; text: string }[],
    targetLang?: string,
    provider?: TranslateProvider,
    forceRefresh = false,
  ): Promise<TranslationResult[]> {
    if (!isTauri()) {
      return items.map((it) => ({
        msg_id: it.msg_id,
        translated_text: `[Bản dịch mẫu -> ${targetLang || "VI"}] ${it.text}`,
        detected_source_lang: "AUTO",
      }));
    }
    return invoke<TranslationResult[]>("translate_messages", {
      items,
      targetLang: targetLang ?? null,
      provider: provider ?? null,
      forceRefresh,
    });
  },
  async clearTranslateCache(msgIds?: string[], targetLang?: string): Promise<number> {
    if (!isTauri()) return 0;
    return invoke<number>("translate_clear_cache", {
      msgIds: msgIds ?? null,
      targetLang: targetLang ?? null,
    });
  },
  async saveRemoteFile(
    url: string,
    suggestedName: string,
    downloadId?: string,
    opts?: { threadId?: string; msgId?: string },
  ): Promise<SavedFileResultDto | null> {
    const trimmed = url.trim();
    const targetUrl =
      /^data:/i.test(trimmed) || /^blob:/i.test(trimmed)
        ? trimmed
        : trimmed.replace(/\\\//g, "/").replace(/^\/\//, "https://");
    if (!isTauri()) {
      const a = document.createElement("a");
      a.href = targetUrl;
      a.download = ensureDownloadFileName(suggestedName, targetUrl);
      a.target = "_blank";
      a.rel = "noreferrer";
      document.body.appendChild(a);
      a.click();
      a.remove();
      return null;
    }
    return invoke<SavedFileResultDto | null>("save_remote_file", {
      url: targetUrl,
      suggestedName,
      downloadId: downloadId ?? null,
      ...(opts?.threadId ? { threadId: opts.threadId } : {}),
      ...(opts?.msgId ? { msgId: opts.msgId } : {}),
    });
  },
  async openSavedFile(path: string): Promise<void> {
    if (!isTauri()) return;
    return invoke("open_saved_file", { path });
  },
  async revealSavedFile(path: string): Promise<void> {
    if (!isTauri()) return;
    return invoke("reveal_saved_file", { path });
  },
  async fetchImageDataUrl(url: string): Promise<string> {
    const trimmed = url.trim();
    if (
      !isTauri() ||
      /^data:(?:image|audio|video)\//i.test(trimmed) ||
      /^blob:/i.test(trimmed)
    ) {
      return trimmed;
    }
    const normalized = trimmed.replace(/\\\//g, "/").replace(/^\/\//, "https://");
    return invoke<string>("fetch_image_data_url", { url: normalized });
  },
  async togglePinMessage(
    threadId: string,
    msgId: string,
    pinned: boolean,
  ): Promise<MessageDto> {
    if (!isTauri()) {
      const list = DEMO_MESSAGES[threadId] ?? [];
      const target = list.find((m) => m.msg_id === msgId);
      if (!target) throw new Error("Message not found");
      target.is_pinned = pinned;
      return { ...target };
    }
    return invoke<MessageDto>("message_toggle_pin", { threadId, msgId, pinned });
  },
  async listStickers(): Promise<StickerItem[]> {
    if (!isTauri()) {
      const glyphs = ["😀", "🎉", "❤️", "👍", "🔥", "😂", "🥳", "✨", "🤩", "🙏", "☕", "🚀"];
      return glyphs.map((glyph, idx) => {
        const id = idx + 1;
        const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" viewBox="0 0 120 120"><rect width="120" height="120" rx="24" fill="#1e293b"/><text x="60" y="76" text-anchor="middle" font-size="60">${glyph}</text></svg>`;
        return {
          id,
          cate_id: 0,
          sticker_type: 1,
          url: `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`,
          category: "Mặc định",
        };
      });
    }
    return invoke<StickerItem[]>("stickers_list");
  },
  async searchStickers(keyword: string): Promise<StickerItem[]> {
    if (!isTauri()) {
      return this.listStickers();
    }
    return invoke<StickerItem[]>("stickers_search", { keyword });
  },
  async suggestStickers(keyword: string, limit = 16): Promise<StickerItem[]> {
    const trimmed = keyword.trim();
    if (!trimmed) return [];
    if (!isTauri()) {
      const all = await this.listStickers();
      return all.slice(0, Math.max(1, Math.min(limit, 8)));
    }
    return invoke<StickerItem[]>("stickers_suggest", { keyword: trimmed, limit });
  },
  async sendSticker(threadId: string, sticker: StickerItem): Promise<MessageDto> {
    if (!isTauri()) {
      const msg: MessageDto = {
        msg_id: `preview-stk-${Date.now()}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text: "[Nhãn dán]",
        msg_type: "chat.sticker",
        ts: Date.now(),
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: sticker.url,
        attachment_name: `sticker_${sticker.cate_id}_${sticker.id}.png`,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, "[Nhãn dán]", msg.ts);
      return msg;
    }
    return invoke<MessageDto>("message_send_sticker", {
      threadId,
      stickerId: sticker.id,
      cateId: sticker.cate_id,
      stickerType: sticker.sticker_type,
    });
  },
  async getRelayConfig(): Promise<RelayConfigSummary> {
    if (!isTauri()) {
      return {
        enabled: false,
        mode: "socks5",
        host: "10.7.12.233",
        port: 1088,
        username: "teza",
        has_password: false,
        password_preview: "",
        route_webview_media: true,
      };
    }
    return invoke<RelayConfigSummary>("relay_get_config");
  },
  async saveRelayConfig(update: RelayConfigUpdate): Promise<RelayConfigSummary> {
    if (!isTauri()) {
      return {
        enabled: update.enabled,
        mode: update.mode,
        host: update.host,
        port: update.port,
        username: update.username,
        has_password: !!update.password && !update.clear_password,
        password_preview: update.password && !update.clear_password ? "••••••••" : "",
        route_webview_media: update.route_webview_media ?? true,
      };
    }
    return invoke<RelayConfigSummary>("relay_save_config", { update });
  },
  async testRelayConnection(update?: RelayConfigUpdate): Promise<RelayTestResult> {
    if (!isTauri()) {
      return { latency_ms: 28 };
    }
    return invoke<RelayTestResult>("relay_test_connection", { update: update ?? null });
  },
  async checkAppUpdate(): Promise<AppUpdateInfo | null> {
    if (!isTauri()) return null;
    return invoke<AppUpdateInfo | null>("app_check_update");
  },
  async installAppUpdate(): Promise<void> {
    if (!isTauri()) return;
    return invoke("app_install_update");
  },
  async setCloseToTray(enabled: boolean): Promise<void> {
    if (!isTauri()) return;
    return invoke("app_set_close_to_tray", { enabled });
  },
  async appSetPasscodeLocked(locked: boolean): Promise<void> {
    if (!isTauri()) return;
    return invoke("app_set_passcode_locked", { locked });
  },
  async webappOpen(
    id: string,
    url: string,
    bounds: { x: number; y: number; width: number; height: number },
  ): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_open", {
      id,
      url,
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: bounds.height,
    });
  },
  async webappSetBounds(
    id: string,
    bounds: { x: number; y: number; width: number; height: number },
  ): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_set_bounds", {
      id,
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: bounds.height,
    });
  },
  async webappHideAll(): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_hide_all");
  },
  async webappClose(id: string): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_close", { id });
  },
  async webappNavigate(
    id: string,
    action: "back" | "forward" | "reload" | "home",
    url?: string,
  ): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_navigate", {
      id,
      action,
      url: url ?? null,
    });
  },
  async listWebAppAccounts(appId?: string, origin?: string): Promise<WebAppAccountDto[]> {
    if (!isTauri()) {
      return filterDemoWebAppAccounts(DEMO_WEBAPP_ACCOUNTS, appId, origin);
    }
    return invoke<WebAppAccountDto[]>("webapp_accounts_list", {
      appId: appId ?? null,
      origin: origin ?? null,
    });
  },
  async saveWebAppAccount(input: WebAppAccountInput): Promise<WebAppAccountDto> {
    if (!isTauri()) {
      const { next, saved } = upsertDemoWebAppAccount(DEMO_WEBAPP_ACCOUNTS, input);
      DEMO_WEBAPP_ACCOUNTS = next;
      return saved;
    }
    return invoke<WebAppAccountDto>("webapp_accounts_save", { input });
  },
  async deleteWebAppAccount(id: string): Promise<void> {
    if (!isTauri()) {
      DEMO_WEBAPP_ACCOUNTS = DEMO_WEBAPP_ACCOUNTS.filter((acc) => acc.id !== id);
      return;
    }
    return invoke("webapp_accounts_delete", { id });
  },
  async fillWebAppAccount(
    appId: string,
    accountId: string,
    submit?: boolean,
  ): Promise<void> {
    if (!isTauri()) return;
    return invoke("webapp_account_fill", {
      appId,
      accountId,
      submit: submit ?? false,
    });
  },
  async openExternalUrl(url: string): Promise<void> {
    if (!isTauri()) {
      window.open(url, "_blank", "noopener,noreferrer");
      return;
    }
    return invoke("open_external_url", { url });
  },
  async addGroupMembers(
    threadId: string,
    memberIds: string[],
  ): Promise<AddGroupMembersResultDto> {
    if (!isTauri()) {
      return {
        added: memberIds,
        pending: [],
        failed: [],
        message: "",
      };
    }
    return invoke<AddGroupMembersResultDto>("group_add_members", {
      threadId,
      memberIds,
    });
  },
  async createReminder(
    threadId: string,
    title: string,
    startTimeMs: number,
    repeat: number,
  ): Promise<MessageDto> {
    if (!isTauri()) {
      const now = Date.now();
      const meta = JSON.stringify({
        title: title.trim(),
        startTime: startTimeMs,
        repeat,
        reminderId: `demo-rem-${now}`,
      });
      const msg: MessageDto = {
        msg_id: `demo-rem-${now}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text: `Nhắc hẹn: ${title.trim()}`,
        msg_type: "chat.reminder",
        ts: now,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: null,
        attachment_name: `teza-reminder:${meta}`,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("reminder_create", {
      threadId,
      title,
      startTimeMs,
      repeat,
    });
  },
  async createPoll(
    threadId: string,
    question: string,
    options: string[],
    expiredTimeMs: number,
    allowMulti: boolean,
    allowAddOption: boolean,
    isAnonymous: boolean,
  ): Promise<MessageDto> {
    if (!isTauri()) {
      const now = Date.now();
      const dto: PollDto = {
        poll_id: now,
        question: question.trim(),
        options: options
          .map((o) => o.trim())
          .filter(Boolean)
          .map((content, idx) => ({
            id: idx + 1,
            content,
            votes: 0,
            voted: false,
          })),
        allow_multi: allowMulti,
        allow_add_option: allowAddOption,
        is_anonymous: isAnonymous,
        expired_time: expiredTimeMs,
        closed: false,
      };
      const msg: MessageDto = {
        msg_id: `demo-poll-${now}`,
        thread_id: threadId,
        sender_id: "me",
        sender_name: "Bạn",
        text: `Bình chọn: ${dto.question}`,
        msg_type: "chat.poll",
        ts: now,
        is_outgoing: true,
        is_recalled: false,
        is_pinned: false,
        quote: null,
        reactions: [],
        attachment_url: null,
        attachment_name: `teza-poll:${JSON.stringify(dto)}`,
      };
      DEMO_MESSAGES[threadId] = [...(DEMO_MESSAGES[threadId] ?? []), msg];
      touchDemoThread(threadId, msg.text, msg.ts);
      return msg;
    }
    return invoke<MessageDto>("poll_create", {
      threadId,
      question,
      options,
      expiredTimeMs,
      allowMulti,
      allowAddOption,
      isAnonymous,
    });
  },
  async getPollDetail(
    pollId: number,
    threadId?: string,
    msgId?: string,
  ): Promise<PollDto> {
    if (!isTauri()) {
      const list = threadId ? DEMO_MESSAGES[threadId] ?? [] : Object.values(DEMO_MESSAGES).flat();
      const found = list.find((m) => {
        if (msgId && m.msg_id === msgId) return true;
        const raw = m.attachment_name?.replace(/^teza-poll:/, "");
        if (!raw || !m.attachment_name?.startsWith("teza-poll:")) return false;
        try {
          return (JSON.parse(raw) as Partial<PollDto>).poll_id === pollId;
        } catch {
          return false;
        }
      });
      const raw = found?.attachment_name?.replace(/^teza-poll:/, "");
      if (raw) return JSON.parse(raw) as PollDto;
      throw new Error("Không tìm thấy bình chọn");
    }
    return invoke<PollDto>("poll_get_detail", {
      pollId,
      threadId: threadId ?? null,
      msgId: msgId ?? null,
    });
  },
  async votePoll(
    pollId: number,
    optionIds: number[],
    threadId?: string,
    msgId?: string,
  ): Promise<PollDto> {
    if (!isTauri()) {
      const list = threadId ? DEMO_MESSAGES[threadId] ?? [] : Object.values(DEMO_MESSAGES).flat();
      const found = list.find((m) => {
        if (msgId && m.msg_id === msgId) return true;
        const raw = m.attachment_name?.replace(/^teza-poll:/, "");
        if (!raw || !m.attachment_name?.startsWith("teza-poll:")) return false;
        try {
          return (JSON.parse(raw) as Partial<PollDto>).poll_id === pollId;
        } catch {
          return false;
        }
      });
      const raw = found?.attachment_name?.replace(/^teza-poll:/, "");
      if (!found || !raw) throw new Error("Không tìm thấy bình chọn");
      const poll = JSON.parse(raw) as PollDto;
      const picked = new Set(optionIds);
      const next: PollDto = {
        ...poll,
        options: poll.options.map((opt) => {
          const willVote = picked.has(opt.id);
          const delta = (willVote ? 1 : 0) - (opt.voted ? 1 : 0);
          return {
            ...opt,
            voted: willVote,
            votes: Math.max(0, opt.votes + delta),
          };
        }),
      };
      found.attachment_name = `teza-poll:${JSON.stringify(next)}`;
      return next;
    }
    return invoke<PollDto>("poll_vote", {
      pollId,
      optionIds,
      threadId: threadId ?? null,
      msgId: msgId ?? null,
    });
  },
  async addPollOptions(
    threadId: string,
    pollId: number,
    newOptions: string[],
    votedOptionIds: number[],
  ): Promise<PollDto> {
    if (!isTauri()) {
      const list = DEMO_MESSAGES[threadId] ?? [];
      const found = list.find((m) => {
        if (!m.attachment_name?.startsWith("teza-poll:")) return false;
        try {
          return (JSON.parse(m.attachment_name.slice("teza-poll:".length)) as Partial<PollDto>).poll_id === pollId;
        } catch {
          return false;
        }
      });
      if (!found || !found.attachment_name?.startsWith("teza-poll:")) {
        throw new Error("Không tìm thấy bình chọn");
      }
      const poll = JSON.parse(found.attachment_name.slice("teza-poll:".length)) as PollDto;
      const cleaned = newOptions.map((o) => o.trim()).filter(Boolean);
      const maxId = poll.options.reduce((acc, o) => Math.max(acc, o.id), 0);
      const keepVoted = new Set(poll.allow_multi ? votedOptionIds : []);
      const existingUpdated = poll.options.map((opt) => {
        const willVote = keepVoted.has(opt.id);
        const delta = (willVote ? 1 : 0) - (opt.voted ? 1 : 0);
        return {
          ...opt,
          voted: willVote,
          votes: Math.max(0, opt.votes + delta),
        };
      });
      const appended = cleaned.map((content, idx) => ({
        id: maxId + idx + 1,
        content,
        votes: 1,
        voted: true,
      }));
      const next: PollDto = {
        ...poll,
        options: [...existingUpdated, ...appended],
      };
      found.attachment_name = `teza-poll:${JSON.stringify(next)}`;
      return next;
    }
    return invoke<PollDto>("poll_add_options", {
      threadId,
      pollId,
      newOptions,
      votedOptionIds,
    });
  },
  async lockPoll(threadId: string, pollId: number): Promise<PollDto> {
    if (!isTauri()) {
      const list = DEMO_MESSAGES[threadId] ?? [];
      const found = list.find((m) => {
        if (!m.attachment_name?.startsWith("teza-poll:")) return false;
        try {
          return (JSON.parse(m.attachment_name.slice("teza-poll:".length)) as Partial<PollDto>).poll_id === pollId;
        } catch {
          return false;
        }
      });
      if (!found || !found.attachment_name?.startsWith("teza-poll:")) {
        throw new Error("Không tìm thấy bình chọn");
      }
      const poll = JSON.parse(found.attachment_name.slice("teza-poll:".length)) as PollDto;
      const next: PollDto = { ...poll, closed: true };
      found.attachment_name = `teza-poll:${JSON.stringify(next)}`;
      return next;
    }
    return invoke<PollDto>("poll_lock", {
      threadId,
      pollId,
    });
  },
  async captureScreen(opts?: {
    hideWindow?: boolean;
    monitorIndex?: number | null;
    captureAll?: boolean;
  }): Promise<ScreenCaptureDto> {
    if (!isTauri()) {
      throw new Error("Chức năng chụp ảnh màn hình chỉ khả dụng trên ứng dụng desktop");
    }
    return invoke<ScreenCaptureDto>("capture_screen", {
      hideWindow: opts?.hideWindow ?? true,
      monitorIndex: opts?.monitorIndex ?? null,
      captureAll: opts?.captureAll ?? false,
    });
  },
  async callGetState(): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_get_state");
  },
  async callListDevices(): Promise<CallDevicesDto> {
    if (!isTauri()) {
      return {
        inputs: [{ name: "Default Microphone", is_default: true, is_virtual: false }],
        outputs: [{ name: "Default Speakers", is_default: true, is_virtual: false }],
        selected_input: null,
        selected_output: null,
      };
    }
    return invoke<CallDevicesDto>("call_list_devices");
  },
  async callSetDevices(
    inputDevice: string | null,
    outputDevice: string | null,
  ): Promise<CallDevicesDto> {
    if (!isTauri()) {
      return {
        inputs: [{ name: "Default Microphone", is_default: true, is_virtual: false }],
        outputs: [{ name: "Default Speakers", is_default: true, is_virtual: false }],
        selected_input: inputDevice?.trim() || null,
        selected_output: outputDevice?.trim() || null,
      };
    }
    return invoke<CallDevicesDto>("call_set_devices", {
      inputDevice: inputDevice ?? null,
      outputDevice: outputDevice ?? null,
    });
  },
  async callTestAudio(): Promise<CallAudioTestResultDto> {
    if (!isTauri()) {
      throw new Error("Kiểm tra âm thanh chỉ khả dụng trên ứng dụng desktop");
    }
    return invoke<CallAudioTestResultDto>("call_test_audio");
  },
  async callStartVoice(partnerId: string, partnerName?: string): Promise<CallStateDto> {
    if (!isTauri()) {
      throw new Error("Tính năng gọi thoại chỉ khả dụng trên ứng dụng desktop");
    }
    return invoke<CallStateDto>("call_start_voice", {
      partnerId,
      partnerName: partnerName ?? null,
    });
  },
  async callStartVideo(partnerId: string, partnerName?: string): Promise<CallStateDto> {
    if (!isTauri()) {
      throw new Error("Tính năng gọi video chỉ khả dụng trên ứng dụng desktop");
    }
    return invoke<CallStateDto>("call_start_video", {
      partnerId,
      partnerName: partnerName ?? null,
    });
  },
  async callAnswer(callId?: number): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_answer", { callId: callId ?? null });
  },
  async callDecline(callId?: number): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_decline", { callId: callId ?? null });
  },
  async callHangup(): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_hangup");
  },
  async callSetMute(muted: boolean): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_set_mute", { muted });
  },
  async callSetCamera(enabled: boolean): Promise<CallStateDto> {
    if (!isTauri()) {
      return {
        available: false,
        unavailable_reason: "desktop_only",
        phase: "idle",
        active: null,
      };
    }
    return invoke<CallStateDto>("call_set_camera", { enabled });
  },
  async callPushVideoFrame(callId: number, dataB64: string): Promise<boolean> {
    if (!isTauri()) {
      return false;
    }
    return invoke<boolean>("call_push_video_frame", { callId, dataB64 });
  },
  async callRequestVideoKeyframe(callId?: number): Promise<boolean> {
    if (!isTauri()) {
      return false;
    }
    return invoke<boolean>("call_request_video_keyframe", {
      callId: callId ?? null,
    });
  },
  async mediaGetProtocolToken(): Promise<string> {
    if (!isTauri()) return "";
    return invoke<string>("media_get_protocol_token");
  },
  async mediaGetConfig(): Promise<MediaVaultConfigDto> {
    if (!isTauri()) {
      return { ...DEMO_MEDIA_VAULT_CONFIG };
    }
    return invoke<MediaVaultConfigDto>("media_get_config");
  },
  async mediaSaveConfig(config: MediaVaultConfigDto): Promise<MediaVaultConfigDto> {
    if (!isTauri()) {
      const customDir = config.custom_vault_dir?.trim() || null;
      DEMO_MEDIA_VAULT_CONFIG = {
        ...config,
        custom_vault_dir: customDir,
        active_vault_dir: customDir ?? "LocalAppData/Teza/media/demo",
      };
      return { ...DEMO_MEDIA_VAULT_CONFIG };
    }
    return invoke<MediaVaultConfigDto>("media_save_config", { config });
  },
  async mediaGetStats(): Promise<MediaVaultStatsDto> {
    if (!isTauri()) {
      return { ...DEMO_MEDIA_VAULT_STATS };
    }
    return invoke<MediaVaultStatsDto>("media_get_stats");
  },
  async mediaClearCache(includePinned?: boolean): Promise<MediaVaultStatsDto> {
    if (!isTauri()) {
      if (includePinned) {
        DEMO_MEDIA_VAULT_STATS = {
          total_bytes: 0,
          object_count: 0,
          pinned_bytes: 0,
          pinned_count: 0,
          image_bytes: 0,
          video_bytes: 0,
          voice_bytes: 0,
          file_bytes: 0,
          thumb_bytes: 0,
          cloud_synced_count: 0,
          cloud_total_bytes: 0,
        };
      } else {
        DEMO_MEDIA_VAULT_STATS = {
          ...DEMO_MEDIA_VAULT_STATS,
          total_bytes: DEMO_MEDIA_VAULT_STATS.pinned_bytes,
          object_count: DEMO_MEDIA_VAULT_STATS.pinned_count,
          image_bytes: 0,
          video_bytes: 0,
          voice_bytes: 0,
          file_bytes: 0,
          thumb_bytes: 0,
        };
      }
      return { ...DEMO_MEDIA_VAULT_STATS };
    }
    return invoke<MediaVaultStatsDto>("media_clear_cache", {
      includePinned: includePinned ?? false,
    });
  },
  async mediaPickVaultDir(): Promise<string | null> {
    if (!isTauri()) return null;
    return invoke<string | null>("media_pick_vault_dir");
  },
  async mediaSetPin(sha256: string, pinned: boolean): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("media_set_pin", { sha256, pinned });
  },
  async mediaOpenVaultDir(): Promise<void> {
    if (!isTauri()) return;
    return invoke<void>("media_open_vault_dir");
  },
  async gdriveGetConfig(): Promise<GDriveConfigDto> {
    if (!isTauri()) {
      return { ...DEMO_GDRIVE_CONFIG };
    }
    return invoke<GDriveConfigDto>("gdrive_get_config");
  },
  async gdriveSaveConfig(patch: GDriveConfigPatch): Promise<GDriveConfigDto> {
    if (!isTauri()) {
      if (patch.resetCustomOAuth) {
        DEMO_GDRIVE_CONFIG.client_id = "";
        DEMO_GDRIVE_CONFIG.has_client_secret = false;
        DEMO_GDRIVE_CONFIG.token_broker_url = "";
      } else {
        if (patch.clientId !== undefined && patch.clientId !== null) {
          DEMO_GDRIVE_CONFIG.client_id = patch.clientId.trim();
        }
        if (patch.clientSecret !== undefined && patch.clientSecret !== null) {
          DEMO_GDRIVE_CONFIG.has_client_secret = patch.clientSecret.trim().length > 0;
        }
        if (patch.tokenBrokerUrl !== undefined && patch.tokenBrokerUrl !== null) {
          DEMO_GDRIVE_CONFIG.token_broker_url = patch.tokenBrokerUrl.trim().replace(/\/+$/, "");
        }
      }
      if (typeof patch.syncEnabled === "boolean") {
        DEMO_GDRIVE_CONFIG.sync_enabled = patch.syncEnabled;
      }
      if (typeof patch.syncMedia === "boolean") {
        DEMO_GDRIVE_CONFIG.sync_media = patch.syncMedia;
      }
      if (typeof patch.syncHistory === "boolean") {
        DEMO_GDRIVE_CONFIG.sync_history = patch.syncHistory;
      }
      DEMO_GDRIVE_CONFIG.oauth_mode = DEMO_GDRIVE_CONFIG.client_id
        ? DEMO_GDRIVE_CONFIG.token_broker_url
          ? "custom_broker"
          : "custom_direct"
        : DEMO_GDRIVE_CONFIG.has_builtin_oauth
          ? DEMO_GDRIVE_CONFIG.has_builtin_token_broker
            ? "builtin_broker"
            : "builtin_direct"
          : "unconfigured";
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        oauth_configured:
          DEMO_GDRIVE_CONFIG.has_builtin_oauth || DEMO_GDRIVE_CONFIG.client_id.length > 0,
        sync_enabled: DEMO_GDRIVE_CONFIG.sync_enabled,
        sync_media: DEMO_GDRIVE_CONFIG.sync_media,
        sync_history: DEMO_GDRIVE_CONFIG.sync_history,
      };
      emitDemoGDriveStatus();
      return { ...DEMO_GDRIVE_CONFIG };
    }
    return invoke<GDriveConfigDto>("gdrive_save_config", {
      clientId: patch.clientId ?? null,
      clientSecret: patch.clientSecret ?? null,
      tokenBrokerUrl: patch.tokenBrokerUrl ?? null,
      resetCustomOAuth: patch.resetCustomOAuth ?? null,
      syncEnabled: patch.syncEnabled ?? null,
      syncMedia: patch.syncMedia ?? null,
      syncHistory: patch.syncHistory ?? null,
    });
  },
  async gdriveGetStatus(): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      return { ...DEMO_GDRIVE_STATUS };
    }
    return invoke<GDriveStatusDto>("gdrive_get_status");
  },
  async gdriveConnectStart(): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        connected: true,
        account_email: "demo@teza.app",
        account_name: "Teza Preview",
        root_folder_id: "demo-teza-vault-root",
        phase: "idle",
        last_error: null,
      };
      return emitDemoGDriveStatus();
    }
    return invoke<GDriveStatusDto>("gdrive_connect_start");
  },
  async gdriveConnectCancel(): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        phase: "idle",
        last_error: null,
      };
      return emitDemoGDriveStatus();
    }
    return invoke<GDriveStatusDto>("gdrive_connect_cancel");
  },
  async gdriveDisconnect(): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        connected: false,
        account_email: null,
        account_name: null,
        root_folder_id: null,
        vault_unlocked: false,
        syncing: false,
        phase: "idle",
        progress_done: 0,
        progress_total: 0,
        last_error: null,
      };
      return emitDemoGDriveStatus();
    }
    return invoke<GDriveStatusDto>("gdrive_disconnect");
  },
  async gdriveUnlockVault(passphrase: string): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      if (passphrase.trim().length < 8) {
        throw new Error("Passphrase must be at least 8 characters long.");
      }
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        vault_unlocked: true,
        has_remote_manifest: true,
        phase: "idle",
        last_error: null,
      };
      return emitDemoGDriveStatus();
    }
    return invoke<GDriveStatusDto>("gdrive_unlock_vault", { passphrase });
  },
  async gdriveLockVault(): Promise<void> {
    if (!isTauri()) {
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        vault_unlocked: false,
        last_error: null,
      };
      emitDemoGDriveStatus();
      return;
    }
    await invoke<void>("gdrive_lock_vault");
  },
  async gdriveSyncNow(): Promise<GDriveStatusDto> {
    if (!isTauri()) {
      const now = Date.now();
      DEMO_GDRIVE_STATUS = {
        ...DEMO_GDRIVE_STATUS,
        syncing: false,
        phase: "idle",
        progress_done: 0,
        progress_total: 0,
        pending_media_count: 0,
        pending_media_bytes: 0,
        last_sync_ts: now,
        last_history_sync_ts: DEMO_GDRIVE_CONFIG.sync_history
          ? now
          : DEMO_GDRIVE_STATUS.last_history_sync_ts,
        last_error: null,
      };
      return emitDemoGDriveStatus();
    }
    return invoke<GDriveStatusDto>("gdrive_sync_now");
  },
  async gdriveGetStorageOverview(limit?: number): Promise<GDriveStorageOverviewDto> {
    if (!isTauri()) {
      return buildDemoGDriveOverview(limit ?? 50);
    }
    return invoke<GDriveStorageOverviewDto>("gdrive_get_storage_overview", {
      limit: limit ?? 50,
    });
  },
  async gdriveDeleteCloudItems(sha256s: string[]): Promise<GDrivePruneResultDto> {
    if (!isTauri()) {
      const targets = new Set(sha256s);
      let deletedCount = 0;
      let freedBytes = 0;
      const remaining: GDriveCloudItemDto[] = [];
      for (const item of DEMO_GDRIVE_CLOUD_ITEMS) {
        if (targets.has(item.sha256)) {
          deletedCount += 1;
          freedBytes += item.size_bytes;
        } else {
          remaining.push(item);
        }
      }
      DEMO_GDRIVE_CLOUD_ITEMS = remaining;
      emitDemoGDriveStatus();
      return {
        deleted_count: deletedCount,
        freed_bytes: freedBytes,
        failed_count: 0,
      };
    }
    return invoke<GDrivePruneResultDto>("gdrive_delete_cloud_items", { sha256s });
  },
  async gdriveDeleteCloudByThread(threadId: string): Promise<GDrivePruneResultDto> {
    if (!isTauri()) {
      let deletedCount = 0;
      let freedBytes = 0;
      const remaining: GDriveCloudItemDto[] = [];
      for (const item of DEMO_GDRIVE_CLOUD_ITEMS) {
        if (item.thread_id === threadId) {
          deletedCount += 1;
          freedBytes += item.size_bytes;
        } else {
          remaining.push(item);
        }
      }
      DEMO_GDRIVE_CLOUD_ITEMS = remaining;
      emitDemoGDriveStatus();
      return {
        deleted_count: deletedCount,
        freed_bytes: freedBytes,
        failed_count: 0,
      };
    }
    return invoke<GDrivePruneResultDto>("gdrive_delete_cloud_by_thread", { threadId });
  },
  async gdrivePruneCloudOlderThan(olderThanDays: number): Promise<GDrivePruneResultDto> {
    if (!isTauri()) {
      const cutoff = Date.now() - Math.max(0, olderThanDays) * 86_400_000;
      let deletedCount = 0;
      let freedBytes = 0;
      const remaining: GDriveCloudItemDto[] = [];
      for (const item of DEMO_GDRIVE_CLOUD_ITEMS) {
        if (item.cloud_synced_ts < cutoff) {
          deletedCount += 1;
          freedBytes += item.size_bytes;
        } else {
          remaining.push(item);
        }
      }
      DEMO_GDRIVE_CLOUD_ITEMS = remaining;
      emitDemoGDriveStatus();
      return {
        deleted_count: deletedCount,
        freed_bytes: freedBytes,
        failed_count: 0,
      };
    }
    return invoke<GDrivePruneResultDto>("gdrive_prune_cloud_older_than", { olderThanDays });
  },
  async onGDriveSyncStatus(handler: (status: GDriveStatusDto) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_GDRIVE_LISTENERS.add(handler);
      return () => {
        DEMO_GDRIVE_LISTENERS.delete(handler);
      };
    }
    return listen<GDriveStatusDto>("core://gdrive-sync-status", (event) => {
      handler(event.payload);
    });
  },
  async friendRemove(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      DEMO_FRIENDS.delete(userId);
      emitDemoSocialEvent({
        kind: "friend_removed",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "remove",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_remove", { userId });
  },
  async friendSendRequest(
    userId: string,
    message?: string | null,
  ): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      if (DEMO_FRIENDS.has(userId)) {
        return {
          ok: true,
          user_id: userId,
          action: "send_request",
          already_friends: true,
          auto_accepted: false,
          message: "",
        };
      }
      const incomingIdx = DEMO_FRIEND_REQUESTS_RECEIVED.findIndex((r) => r.user_id === userId);
      if (incomingIdx >= 0) {
        const [incoming] = DEMO_FRIEND_REQUESTS_RECEIVED.splice(incomingIdx, 1);
        DEMO_FRIENDS.add(userId);
        if (!DEMO_THREADS.some((t) => t.id === userId)) {
          DEMO_THREADS.push({
            id: userId,
            title: incoming.display_name,
            avatar: incoming.avatar,
            kind: "direct",
            last_preview: "Hai bạn đã trở thành bạn bè",
            last_ts: Date.now(),
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            last_read_in_ts: 0,
            peer_read_ts: 0,
            muted_until_ts: 0,
            is_archived: false,
          });
        }
        emitDemoSocialEvent({
          kind: "friend_added",
          thread_id: userId,
          user_id: userId,
          self_removed: false,
        });
        return {
          ok: true,
          user_id: userId,
          action: "send_request",
          already_friends: false,
          auto_accepted: true,
          message: "",
        };
      }
      const existingSent = DEMO_FRIEND_REQUESTS_SENT.find((r) => r.user_id === userId);
      const greeting = message?.trim() ?? "";
      if (existingSent) {
        existingSent.message = greeting || existingSent.message;
        existingSent.created_ts = Date.now();
      } else {
        const thread = DEMO_THREADS.find((t) => t.id === userId);
        const name = thread?.title ?? `Người dùng ${userId}`;
        DEMO_FRIEND_REQUESTS_SENT = [
          {
            user_id: userId,
            display_name: name,
            account_name: name,
            avatar: thread?.avatar ?? "",
            message: greeting,
            created_ts: Date.now(),
            direction: "sent",
            is_seen: false,
          },
          ...DEMO_FRIEND_REQUESTS_SENT,
        ];
      }
      emitDemoSocialEvent({
        kind: "friend_request_sent",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "send_request",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_send_request", {
      userId,
      message: message ?? null,
    });
  },
  async friendAcceptRequest(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      const incomingIdx = DEMO_FRIEND_REQUESTS_RECEIVED.findIndex((r) => r.user_id === userId);
      const incoming =
        incomingIdx >= 0 ? DEMO_FRIEND_REQUESTS_RECEIVED.splice(incomingIdx, 1)[0] : null;
      DEMO_FRIEND_REQUESTS_SENT = DEMO_FRIEND_REQUESTS_SENT.filter((r) => r.user_id !== userId);
      DEMO_FRIENDS.add(userId);
      if (!DEMO_THREADS.some((t) => t.id === userId)) {
        DEMO_THREADS.push({
          id: userId,
          title: incoming?.display_name ?? `Người dùng ${userId}`,
          avatar: incoming?.avatar ?? "",
          kind: "direct",
          last_preview: "Hai bạn đã trở thành bạn bè",
          last_ts: Date.now(),
          unread_count: 0,
          is_pinned: false,
          is_muted: false,
          last_read_in_ts: 0,
          peer_read_ts: 0,
          muted_until_ts: 0,
          is_archived: false,
        });
      }
      emitDemoSocialEvent({
        kind: "friend_added",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "accept",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_accept_request", { userId });
  },
  async friendRejectRequest(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      DEMO_FRIEND_REQUESTS_RECEIVED = DEMO_FRIEND_REQUESTS_RECEIVED.filter(
        (r) => r.user_id !== userId,
      );
      emitDemoSocialEvent({
        kind: "friend_request_rejected",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "reject",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_reject_request", { userId });
  },
  async friendUndoRequest(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      DEMO_FRIEND_REQUESTS_SENT = DEMO_FRIEND_REQUESTS_SENT.filter((r) => r.user_id !== userId);
      emitDemoSocialEvent({
        kind: "friend_request_undone",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "undo",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_undo_request", { userId });
  },
  async friendListRequests(): Promise<FriendRequestsListDto> {
    if (!isTauri()) {
      return {
        received: DEMO_FRIEND_REQUESTS_RECEIVED.map((item) => ({ ...item })),
        sent: DEMO_FRIEND_REQUESTS_SENT.map((item) => ({ ...item })),
      };
    }
    return invoke<FriendRequestsListDto>("friend_list_requests");
  },
  async friendBlockUser(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      DEMO_BLOCKED.add(userId);
      emitDemoSocialEvent({
        kind: "user_blocked",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "block",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_block_user", { userId });
  },
  async friendUnblockUser(userId: string): Promise<FriendActionResultDto> {
    if (!isTauri()) {
      DEMO_BLOCKED.delete(userId);
      emitDemoSocialEvent({
        kind: "user_unblocked",
        thread_id: userId,
        user_id: userId,
        self_removed: false,
      });
      return {
        ok: true,
        user_id: userId,
        action: "unblock",
        already_friends: false,
        auto_accepted: false,
        message: "",
      };
    }
    return invoke<FriendActionResultDto>("friend_unblock_user", { userId });
  },
  async groupLeave(groupId: string, silent?: boolean): Promise<LeaveGroupResultDto> {
    if (!isTauri()) {
      const idx = DEMO_THREADS.findIndex((t) => t.id === groupId);
      if (idx >= 0) DEMO_THREADS.splice(idx, 1);
      delete DEMO_GROUP_MEMBERS[groupId];
      emitDemoSocialEvent({
        kind: "group_left",
        thread_id: groupId,
        user_id: null,
        self_removed: true,
      });
      return {
        group_id: groupId,
        left: true,
        member_error: [],
        message: "",
      };
    }
    return invoke<LeaveGroupResultDto>("group_leave", {
      groupId,
      silent: Boolean(silent),
    });
  },
  async groupKickMembers(
    groupId: string,
    memberIds: string[],
  ): Promise<KickGroupMembersResultDto> {
    if (!isTauri()) {
      const group = DEMO_GROUP_MEMBERS[groupId];
      const removed: string[] = [];
      const failed: string[] = [];
      if (group) {
        for (const uid of new Set(memberIds)) {
          if (uid === "me" || !group.members.some((m) => m.id === uid)) {
            failed.push(uid);
          } else {
            removed.push(uid);
          }
        }
        const removedSet = new Set(removed);
        group.members = group.members.filter((m) => !removedSet.has(m.id));
        group.admin_ids = group.admin_ids.filter((id) => !removedSet.has(id));
      } else {
        removed.push(...new Set(memberIds));
      }
      if (removed.length > 0) {
        emitDemoSocialEvent({
          kind: "group_member_removed",
          thread_id: groupId,
          user_id: removed[0],
          self_removed: false,
        });
      }
      return {
        group_id: groupId,
        removed,
        failed,
        message: "",
      };
    }
    return invoke<KickGroupMembersResultDto>("group_kick_members", {
      groupId,
      memberIds,
    });
  },
  async onSocialEvent(handler: (ev: SocialWsEventDto) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_SOCIAL_LISTENERS.add(handler);
      return () => {
        DEMO_SOCIAL_LISTENERS.delete(handler);
      };
    }
    return listen<SocialWsEventDto>("core://social-event", (event) => {
      handler(event.payload);
    });
  },

  // ==================== Item #1: Optional AI Assistant ====================
  async aiGetConfig(): Promise<AiConfigDto> {
    if (!isTauri()) {
      return {
        ...DEMO_AI_CONFIG,
        zeroclaw_allowed_roots: [...(DEMO_AI_CONFIG.zeroclaw_allowed_roots ?? [])],
        zeroclaw_allowed_thread_ids: [...(DEMO_AI_CONFIG.zeroclaw_allowed_thread_ids ?? [])],
      };
    }
    return invoke<AiConfigDto>("ai_get_config");
  },
  async aiUpdateConfig(update: AiConfigUpdateDto): Promise<AiConfigDto> {
    if (!isTauri()) {
      const next: AiConfigDto = {
        ...DEMO_AI_CONFIG,
        zeroclaw_allowed_roots: [...(DEMO_AI_CONFIG.zeroclaw_allowed_roots ?? [])],
        zeroclaw_allowed_thread_ids: [...(DEMO_AI_CONFIG.zeroclaw_allowed_thread_ids ?? [])],
      };
      if (typeof update.enabled === "boolean") next.enabled = update.enabled;
      if (update.backend === "cloud_api" || update.backend === "zeroclaw") {
        next.backend = update.backend;
      }
      if (typeof update.api_base_url === "string") {
        next.api_base_url = update.api_base_url.trim() || DEFAULT_CLOUD_API_BASE_URL;
      }
      if (update.clear_api_key) {
        next.has_api_key = false;
        next.api_key_preview = "";
      } else if (typeof update.api_key === "string" && update.api_key.trim()) {
        const cleanKey = update.api_key.trim();
        next.has_api_key = true;
        next.api_key_preview =
          cleanKey.length > 8 ? `${cleanKey.slice(0, 4)}...${cleanKey.slice(-4)}` : "••••";
      }
      if (typeof update.model === "string") {
        next.model =
          update.model.trim() ||
          (next.backend === "cloud_api" ? DEFAULT_CLOUD_API_MODEL : DEFAULT_ZEROCLAW_MODEL);
      } else if (next.backend === "cloud_api" && next.model === DEFAULT_ZEROCLAW_MODEL) {
        next.model = DEFAULT_CLOUD_API_MODEL;
      }
      if (typeof update.zeroclaw_path === "string") {
        next.zeroclaw_path = update.zeroclaw_path.trim() || DEFAULT_ZEROCLAW_PATH;
      }
      if (typeof update.zeroclaw_gateway_url === "string") {
        next.zeroclaw_gateway_url = update.zeroclaw_gateway_url.trim();
      }
      if (typeof update.zeroclaw_safe_tools === "boolean") {
        next.zeroclaw_safe_tools = update.zeroclaw_safe_tools;
      }
      if (
        update.teza_access_level === "isolated" ||
        update.teza_access_level === "read_only" ||
        update.teza_access_level === "supervised" ||
        update.teza_access_level === "autonomous"
      ) {
        next.teza_access_level = update.teza_access_level;
      }
      if (
        update.system_access_level === "safe" ||
        update.system_access_level === "supervised" ||
        update.system_access_level === "full"
      ) {
        next.system_access_level = update.system_access_level;
      }
      if (Array.isArray(update.zeroclaw_allowed_roots)) {
        const seenRoots = new Set<string>();
        const cleanedRoots: string[] = [];
        for (const raw of update.zeroclaw_allowed_roots) {
          if (cleanedRoots.length >= 32) break;
          if (typeof raw !== "string") continue;
          const clean = raw.trim().replace(/^"(.*)"$/, "$1").trim();
          if (!clean || clean.length > 512) continue;
          const key = clean.replace(/[\\/]+$/, "").replace(/\//g, "\\").toLowerCase();
          if (seenRoots.has(key)) continue;
          seenRoots.add(key);
          cleanedRoots.push(clean);
        }
        next.zeroclaw_allowed_roots = cleanedRoots;
      }
      if (Array.isArray(update.zeroclaw_allowed_thread_ids)) {
        const seenIds = new Set<string>();
        const cleanedIds: string[] = [];
        for (const raw of update.zeroclaw_allowed_thread_ids) {
          if (cleanedIds.length >= 200) break;
          if (typeof raw !== "string") continue;
          const clean = raw.trim();
          if (!clean || seenIds.has(clean)) continue;
          seenIds.add(clean);
          cleanedIds.push(clean);
        }
        next.zeroclaw_allowed_thread_ids = cleanedIds;
      }
      if (typeof update.system_prompt === "string") {
        next.system_prompt = update.system_prompt;
      }
      if (typeof update.summary_language === "string") {
        next.summary_language = update.summary_language;
      }
      if (typeof update.max_summary_messages === "number") {
        next.max_summary_messages = update.max_summary_messages;
      }
      if (update.clear_zeroclaw_token) {
        next.has_zeroclaw_token = false;
        next.zeroclaw_token_preview = "";
      } else if (typeof update.zeroclaw_token === "string" && update.zeroclaw_token.trim()) {
        const cleanTok = update.zeroclaw_token.trim();
        next.has_zeroclaw_token = true;
        next.zeroclaw_token_preview =
          cleanTok.length > 8 ? `${cleanTok.slice(0, 4)}••••${cleanTok.slice(-4)}` : "••••";
      }
      DEMO_AI_CONFIG = next;
      return {
        ...next,
        zeroclaw_allowed_roots: [...(next.zeroclaw_allowed_roots ?? [])],
        zeroclaw_allowed_thread_ids: [...(next.zeroclaw_allowed_thread_ids ?? [])],
      };
    }
    return invoke<AiConfigDto>("ai_update_config", {
      update: {
        ...update,
        clear_api_key: Boolean(update.clear_api_key),
        clear_zeroclaw_token: Boolean(update.clear_zeroclaw_token),
      },
    });
  },
  async aiListModels(baseUrl?: string, apiKey?: string): Promise<AiModelInfoDto[]> {
    if (!isTauri()) {
      const targetUrl = baseUrl?.trim() || DEMO_AI_CONFIG.api_base_url;
      const preset = getAiCloudProviderPreset(detectAiCloudProvider(targetUrl));
      return (preset?.recommendedModels ?? []).map((m) => ({
        id: m.id,
        is_free: Boolean(m.isFree),
        owned_by: preset?.name ?? null,
      }));
    }
    return invoke<AiModelInfoDto[]>("ai_list_models", {
      baseUrl: baseUrl?.trim() ? baseUrl.trim() : null,
      apiKey: apiKey?.trim() ? apiKey.trim() : null,
    });
  },
  async aiProbeCli(cliPath?: string, gatewayUrl?: string): Promise<AiCliProbeDto> {
    if (!isTauri()) {
      return {
        available: false,
        resolved_path: null,
        version: null,
        mode: gatewayUrl?.trim() ? "gateway" : "cli",
        error: "ZeroClaw chỉ chạy trên ứng dụng Teza cài đặt cho máy tính.",
      };
    }
    return invoke<AiCliProbeDto>("ai_probe_cli", {
      cliPath: cliPath ?? null,
      gatewayUrl: gatewayUrl ?? null,
    });
  },
  async aiInstallZeroClaw(): Promise<AiInstallZeroClawResponseDto> {
    if (!isTauri()) {
      throw new Error("Tải và cài đặt ZeroClaw chỉ hỗ trợ trên ứng dụng máy tính.");
    }
    return invoke<AiInstallZeroClawResponseDto>("ai_install_zeroclaw");
  },
  async onZeroClawInstallProgress(
    handler: (ev: ZeroClawInstallProgressDto) => void,
  ): Promise<() => void> {
    if (!isTauri()) {
      return () => {};
    }
    return listen<ZeroClawInstallProgressDto>("core://zeroclaw-install-progress", (event) => {
      handler(event.payload);
    });
  },
  async aiListSessions(limit?: number): Promise<AiSessionDto[]> {
    if (!isTauri()) {
      const knownIds = new Set(DEMO_AI_SESSIONS.map((s) => s.id));
      for (const msg of DEMO_AI_MESSAGES) {
        if (!knownIds.has(msg.session_id)) {
          knownIds.add(msg.session_id);
          touchDemoAiSessionOnMessage(msg);
        }
      }
      const hydrated = DEMO_AI_SESSIONS.map((s) => hydrateDemoAiSession(s)).sort(
        (a, b) => b.updated_ts - a.updated_ts || b.id.localeCompare(a.id),
      );
      return limit && limit > 0 ? hydrated.slice(0, limit) : hydrated;
    }
    return invoke<AiSessionDto[]>("ai_list_sessions", {
      limit: limit ?? null,
    });
  },
  async aiCreateSession(title?: string): Promise<AiSessionDto> {
    if (!isTauri()) {
      const now = Date.now();
      const cleanTitle = (title ?? "").trim().slice(0, 120) || "Cuộc trò chuyện mới";
      const created: AiSessionDto = {
        id: `ai-${now}-${Math.random().toString(36).slice(2, 6)}`,
        title: cleanTitle,
        created_ts: now,
        updated_ts: now,
        message_count: 0,
        last_preview: "",
      };
      DEMO_AI_SESSIONS = [created, ...DEMO_AI_SESSIONS];
      return { ...created };
    }
    return invoke<AiSessionDto>("ai_create_session", {
      title: title ?? null,
    });
  },
  async aiRenameSession(sessionId: string, title: string): Promise<AiSessionDto> {
    if (!isTauri()) {
      const sid = (sessionId || "").trim();
      const cleanTitle = (title || "").trim().slice(0, 120);
      if (!sid) {
        throw new Error("Mã cuộc trò chuyện AI không hợp lệ.");
      }
      if (!cleanTitle) {
        throw new Error("Tiêu đề cuộc trò chuyện AI không được để trống.");
      }
      const existing = DEMO_AI_SESSIONS.find((s) => s.id === sid);
      if (!existing) {
        throw new Error("Không tìm thấy cuộc trò chuyện AI.");
      }
      const updated: AiSessionDto = {
        ...existing,
        title: cleanTitle,
      };
      DEMO_AI_SESSIONS = DEMO_AI_SESSIONS.map((s) => (s.id === sid ? updated : s));
      return hydrateDemoAiSession(updated);
    }
    return invoke<AiSessionDto>("ai_rename_session", {
      sessionId,
      title,
    });
  },
  async aiDeleteSession(sessionId: string): Promise<number> {
    if (!isTauri()) {
      const sid = (sessionId || "").trim();
      const before = DEMO_AI_MESSAGES.length;
      DEMO_AI_MESSAGES = DEMO_AI_MESSAGES.filter((m) => m.session_id !== sid);
      DEMO_AI_SESSIONS = DEMO_AI_SESSIONS.filter((s) => s.id !== sid);
      return before - DEMO_AI_MESSAGES.length;
    }
    return invoke<number>("ai_delete_session", {
      sessionId,
    });
  },
  async aiListMessages(sessionId?: string, limit?: number): Promise<AiStoredMessageDto[]> {
    if (!isTauri()) {
      const sid = sessionId || "default";
      const list = DEMO_AI_MESSAGES.filter((m) => m.session_id === sid);
      return (limit ? list.slice(-limit) : list).map((m) => ({ ...m }));
    }
    return invoke<AiStoredMessageDto[]>("ai_list_messages", {
      sessionId: sessionId ?? null,
      limit: limit ?? null,
    });
  },
  async aiClearMessages(sessionId?: string): Promise<number> {
    if (!isTauri()) {
      const sid = sessionId || "default";
      const before = DEMO_AI_MESSAGES.length;
      DEMO_AI_MESSAGES = DEMO_AI_MESSAGES.filter((m) => m.session_id !== sid);
      DEMO_AI_SESSIONS = DEMO_AI_SESSIONS.map((s) =>
        s.id === sid ? { ...s, message_count: 0, last_preview: "" } : s,
      );
      return before - DEMO_AI_MESSAGES.length;
    }
    return invoke<number>("ai_clear_messages", {
      sessionId: sessionId ?? null,
    });
  },
  async aiChatSend(prompt: string, sessionId?: string): Promise<AiStoredMessageDto> {
    if (!isTauri()) {
      if (!DEMO_AI_CONFIG.enabled) {
        throw new Error("Trợ lý AI đang tắt. Hãy bật trong Cài đặt → Trợ lý AI.");
      }
      const sid = sessionId || "default";
      const now = Date.now();
      const userMsg: AiStoredMessageDto = {
        id: `demo-ai-u-${now}`,
        session_id: sid,
        role: "user",
        content: prompt,
        model: DEMO_AI_CONFIG.model,
        backend: DEMO_AI_CONFIG.backend,
        ts: now,
      };
      DEMO_AI_MESSAGES = [...DEMO_AI_MESSAGES, userMsg];
      touchDemoAiSessionOnMessage(userMsg);
      const backendNote =
        DEMO_AI_CONFIG.backend === "cloud_api" ? "khóa API đã cấu hình" : "ZeroClaw";
      const replyText = `[Chế độ demo] Đã nhận câu hỏi: "${prompt.trim().slice(0, 140)}".\n\n- Đây là phản hồi mô phỏng cục bộ khi chạy ngoài Tauri.\n- Khi chạy trên ứng dụng máy tính, Trợ lý AI sẽ xử lý trực tiếp bằng **${backendNote}**.`;
      for (const fn of DEMO_AI_STREAM_LISTENERS) {
        try {
          fn({ session_id: sid, delta: replyText });
        } catch {
          // Ignore demo listener error
        }
      }
      const modelMsg: AiStoredMessageDto = {
        id: `demo-ai-m-${now + 1}`,
        session_id: sid,
        role: "model",
        content: replyText,
        model: DEMO_AI_CONFIG.model,
        backend: DEMO_AI_CONFIG.backend,
        ts: now + 1,
      };
      DEMO_AI_MESSAGES = [...DEMO_AI_MESSAGES, modelMsg];
      touchDemoAiSessionOnMessage(modelMsg);
      return { ...modelMsg };
    }
    return invoke<AiStoredMessageDto>("ai_chat_send", {
      prompt,
      sessionId: sessionId ?? null,
    });
  },
  async aiChatCancel(sessionId?: string): Promise<boolean> {
    if (!isTauri()) {
      return false;
    }
    return invoke<boolean>("ai_chat_cancel", {
      sessionId: sessionId ?? null,
    });
  },
  async aiRunZeroClawCommand(
    command: string,
    sessionId?: string,
  ): Promise<AiCommandResultDto> {
    if (!isTauri()) {
      const norm = (command || "").trim().toLowerCase();
      const sid = (sessionId || "default").trim() || "default";
      switch (norm) {
        case "memory":
        case "memory_list":
        case "memory_stats": {
          const msgCount = DEMO_AI_MESSAGES.filter((m) => m.session_id === sid).length;
          return {
            command: norm,
            title: "Trí nhớ ZeroClaw",
            output: `Chế độ xem trước — phiên "${sid}" hiện có ${msgCount} tin nhắn cục bộ. Trí nhớ ZeroClaw thật chỉ hoạt động trên ứng dụng Teza máy tính.`,
            ok: true,
          };
        }
        case "memory_clear":
          return {
            command: "memory_clear",
            title: "Xóa trí nhớ ZeroClaw",
            output: `Đã xóa trí nhớ mô phỏng của phiên "${sid}" (chế độ xem trước).`,
            ok: true,
          };
        case "skills":
        case "skills_list":
          return {
            command: "skills",
            title: "Kỹ năng ZeroClaw",
            output:
              "Chưa có kỹ năng nào được cài trong chế độ xem trước. Mở Teza trên máy tính để xem danh sách kỹ năng ZeroClaw.",
            ok: true,
          };
        case "status":
          return {
            command: "status",
            title: "Trạng thái ZeroClaw",
            output: `Chế độ xem trước (ngoài Tauri) — công cụ an toàn: ${DEMO_AI_CONFIG.zeroclaw_safe_tools ? "Đang bật" : "Đang tắt"}.`,
            ok: true,
          };
        default:
          throw new Error(`Lệnh ZeroClaw không được hỗ trợ: ${command}`);
      }
    }
    return invoke<AiCommandResultDto>("ai_run_zeroclaw_command", {
      command,
      sessionId: sessionId ?? null,
    });
  },
  async aiSummarizeUnread(threadId: string): Promise<AiSummaryDto> {
    if (!isTauri()) {
      if (!DEMO_AI_CONFIG.enabled) {
        throw new Error("Trợ lý AI đang tắt. Hãy bật trong Cài đặt → Trợ lý AI.");
      }
      const list = (DEMO_MESSAGES[threadId] ?? []).slice(-DEMO_AI_CONFIG.max_summary_messages);
      const summaryText =
        list.length > 0
          ? `Tóm tắt nhanh ${list.length} tin nhắn gần đây trong hội thoại (chế độ demo).`
          : "Không có tin nhắn chưa đọc nào cần tóm tắt.";
      for (const fn of DEMO_AI_SUMMARY_LISTENERS) {
        try {
          fn({ thread_id: threadId, delta: summaryText });
        } catch {
          // Ignore demo listener error
        }
      }
      return {
        summary: summaryText,
        bullet_points: list
          .slice(-3)
          .map((m) => `${m.sender_name}: ${(m.text || m.attachment_name || "").slice(0, 80)}`),
        action_items: list.length > 0 ? ["Xem lại tin nhắn mới nhất và phản hồi khi thuận tiện."] : [],
        message_count: list.length,
        backend: DEMO_AI_CONFIG.backend,
        model: DEMO_AI_CONFIG.model,
      };
    }
    return invoke<AiSummaryDto>("ai_summarize_unread", { threadId });
  },
  async aiTranslateText(text: string, targetLang: string): Promise<string> {
    if (!isTauri()) {
      return `[AI Demo -> ${targetLang}] ${text}`;
    }
    return invoke<string>("ai_translate_text", { text, targetLang });
  },
  async onAiStreamDelta(handler: (ev: AiStreamDeltaEvent) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_STREAM_LISTENERS.add(handler);
      return () => {
        DEMO_AI_STREAM_LISTENERS.delete(handler);
      };
    }
    return listen<AiStreamDeltaEvent>("core://ai-stream-delta", (event) => {
      handler(event.payload);
    });
  },
  async onAiSummaryDelta(handler: (ev: AiSummaryDeltaEvent) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_SUMMARY_LISTENERS.add(handler);
      return () => {
        DEMO_AI_SUMMARY_LISTENERS.delete(handler);
      };
    }
    return listen<AiSummaryDeltaEvent>("core://ai-summary-delta", (event) => {
      handler(event.payload);
    });
  },
  async aiResolveToolApproval(
    approvalId: string,
    decision: AiApprovalDecision,
  ): Promise<boolean> {
    if (!isTauri()) {
      const idx = DEMO_AI_PENDING_APPROVALS.findIndex((a) => a.approval_id === approvalId);
      if (idx < 0) return false;
      const [removed] = DEMO_AI_PENDING_APPROVALS.splice(idx, 1);
      for (const fn of DEMO_AI_TOOL_APPROVAL_RESOLVED_LISTENERS) {
        try {
          fn({ approval_id: approvalId, session_id: removed.session_id, decision });
        } catch {
          // Ignore demo listener error
        }
      }
      return true;
    }
    return invoke<boolean>("ai_resolve_tool_approval", { approvalId, decision });
  },
  async aiEmergencyStop(): Promise<AiEmergencyStopResult> {
    if (!isTauri()) {
      const cleared = DEMO_AI_PENDING_APPROVALS;
      DEMO_AI_PENDING_APPROVALS = [];
      for (const req of cleared) {
        for (const fn of DEMO_AI_TOOL_APPROVAL_RESOLVED_LISTENERS) {
          try {
            fn({
              approval_id: req.approval_id,
              session_id: req.session_id,
              decision: "cancelled",
            });
          } catch {
            // Ignore demo listener error
          }
        }
      }
      return {
        cancelled_approvals: cleared.length,
        killed_processes: 0,
      };
    }
    return invoke<AiEmergencyStopResult>("ai_emergency_stop");
  },
  async aiListToolAuditLogs(limit?: number): Promise<AiToolAuditEntry[]> {
    if (!isTauri()) {
      return (limit && limit > 0
        ? DEMO_AI_TOOL_AUDIT_LOGS.slice(0, limit)
        : DEMO_AI_TOOL_AUDIT_LOGS
      ).map((row) => ({ ...row }));
    }
    return invoke<AiToolAuditEntry[]>("ai_list_tool_audit_logs", {
      limit: limit ?? null,
    });
  },
  async aiMcpStatus(): Promise<AiMcpStatusDto> {
    if (!isTauri()) {
      const tezaLevel = DEMO_AI_CONFIG.teza_access_level ?? "isolated";
      const sysLevel = DEMO_AI_CONFIG.system_access_level ?? "safe";
      const tezaCount =
        tezaLevel === "read_only" ? 10 : tezaLevel === "supervised" || tezaLevel === "autonomous" ? 21 : 0;
      const sysCount = sysLevel === "supervised" || sysLevel === "full" ? 5 : 0;
      const totalTools = tezaCount + sysCount;
      const running = Boolean(
        DEMO_AI_CONFIG.enabled && DEMO_AI_CONFIG.backend === "zeroclaw" && totalTools > 0,
      );
      return {
        running,
        exposed_tools_count: running ? totalTools : 0,
        pending_approvals_count: DEMO_AI_PENDING_APPROVALS.length,
        active_powershell_count: 0,
        workspace_dir: "LocalAppData/Teza/ai_workspace",
        forbidden_paths: [
          "C:\\Windows",
          "C:\\Windows\\System32",
          "C:\\Program Files",
          "~/.ssh",
          "~/.gnupg",
          "~/.aws",
          "Teza DB & DPAPI keys",
        ],
      };
    }
    return invoke<AiMcpStatusDto>("ai_mcp_status");
  },
  async aiPickAllowedRootDir(): Promise<string | null> {
    if (!isTauri()) {
      return null;
    }
    return invoke<string | null>("ai_pick_allowed_root_dir");
  },
  async onAiToolActivity(handler: (ev: AiToolActivityEvent) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_TOOL_ACTIVITY_LISTENERS.add(handler);
      return () => {
        DEMO_AI_TOOL_ACTIVITY_LISTENERS.delete(handler);
      };
    }
    return listen<AiToolActivityEvent>("core://ai-tool-activity", (event) => {
      handler(event.payload);
    });
  },
  async onAiToolApproval(handler: (ev: AiToolApprovalRequest) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_TOOL_APPROVAL_LISTENERS.add(handler);
      return () => {
        DEMO_AI_TOOL_APPROVAL_LISTENERS.delete(handler);
      };
    }
    return listen<AiToolApprovalRequest>("core://ai-tool-approval", (event) => {
      handler(event.payload);
    });
  },
  async onAiToolApprovalResolved(
    handler: (ev: AiToolApprovalResolvedEvent) => void,
  ): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_TOOL_APPROVAL_RESOLVED_LISTENERS.add(handler);
      return () => {
        DEMO_AI_TOOL_APPROVAL_RESOLVED_LISTENERS.delete(handler);
      };
    }
    return listen<AiToolApprovalResolvedEvent>("core://ai-tool-approval-resolved", (event) => {
      handler(event.payload);
    });
  },
  async onAiComposeDraft(handler: (ev: AiComposeDraftEvent) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_COMPOSE_DRAFT_LISTENERS.add(handler);
      return () => {
        DEMO_AI_COMPOSE_DRAFT_LISTENERS.delete(handler);
      };
    }
    return listen<AiComposeDraftEvent>("core://ai-compose-draft", (event) => {
      handler(event.payload);
    });
  },
  async onAiOpenThread(handler: (ev: AiOpenThreadEvent) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_AI_OPEN_THREAD_LISTENERS.add(handler);
      return () => {
        DEMO_AI_OPEN_THREAD_LISTENERS.delete(handler);
      };
    }
    return listen<AiOpenThreadEvent>("core://ai-open-thread", (event) => {
      handler(event.payload);
    });
  },

  // ==================== Item #2: Scheduled Messages ====================
  async scheduleList(statusFilter?: string, limit?: number): Promise<ScheduledMessageDto[]> {
    if (!isTauri()) {
      let items = DEMO_SCHEDULES;
      if (statusFilter && statusFilter !== "all") {
        const wanted = new Set(statusFilter.split(",").map((s) => s.trim()));
        items = items.filter((it) => wanted.has(it.status));
      }
      return (limit ? items.slice(0, limit) : items).map((it) => ({ ...it }));
    }
    return invoke<ScheduledMessageDto[]>("schedule_list", {
      statusFilter: statusFilter ?? null,
      limit: limit ?? null,
    });
  },
  async scheduleCreate(input: CreateScheduledMessageInput): Promise<ScheduledMessageDto> {
    const payload = {
      thread_id: input.thread_id,
      thread_title: input.thread_title,
      thread_kind: input.thread_kind,
      text: input.text,
      attachment_path: input.attachment_path ?? "",
      send_at_ts: input.send_at_ts,
      recurrence: input.recurrence,
      misfire_policy: input.misfire_policy,
      misfire_grace_secs: input.misfire_grace_secs ?? 600,
    };
    if (!isTauri()) {
      const now = Date.now();
      const created: ScheduledMessageDto = {
        id: `demo-sched-${now}-${Math.random().toString(36).slice(2, 6)}`,
        thread_id: payload.thread_id,
        thread_title: payload.thread_title || payload.thread_id,
        thread_kind: payload.thread_kind,
        text: payload.text,
        attachment_path: payload.attachment_path,
        send_at_ts: payload.send_at_ts,
        recurrence: payload.recurrence,
        misfire_policy: payload.misfire_policy,
        misfire_grace_secs: payload.misfire_grace_secs,
        status: "pending",
        retry_count: 0,
        last_error: "",
        created_at: now,
        updated_at: now,
        sent_at_ts: 0,
      };
      DEMO_SCHEDULES = [created, ...DEMO_SCHEDULES];
      emitDemoScheduleUpdated(created);
      return { ...created };
    }
    return invoke<ScheduledMessageDto>("schedule_create", { input: payload });
  },
  async scheduleUpdate(
    id: string,
    input: UpdateScheduledMessageInput,
  ): Promise<ScheduledMessageDto | null> {
    if (!isTauri()) {
      const idx = DEMO_SCHEDULES.findIndex((s) => s.id === id);
      if (idx < 0) return null;
      const updated: ScheduledMessageDto = {
        ...DEMO_SCHEDULES[idx],
        ...(input.thread_title !== undefined ? { thread_title: input.thread_title } : {}),
        ...(input.thread_kind !== undefined ? { thread_kind: input.thread_kind } : {}),
        ...(input.text !== undefined ? { text: input.text } : {}),
        ...(input.attachment_path !== undefined ? { attachment_path: input.attachment_path } : {}),
        ...(input.send_at_ts !== undefined ? { send_at_ts: input.send_at_ts } : {}),
        ...(input.recurrence !== undefined ? { recurrence: input.recurrence } : {}),
        ...(input.misfire_policy !== undefined ? { misfire_policy: input.misfire_policy } : {}),
        ...(input.misfire_grace_secs !== undefined
          ? { misfire_grace_secs: input.misfire_grace_secs }
          : {}),
        updated_at: Date.now(),
      };
      DEMO_SCHEDULES = DEMO_SCHEDULES.map((s) => (s.id === id ? updated : s));
      emitDemoScheduleUpdated(updated);
      return { ...updated };
    }
    return invoke<ScheduledMessageDto | null>("schedule_update", { id, input });
  },
  async scheduleCancel(id: string): Promise<boolean> {
    if (!isTauri()) {
      const target = DEMO_SCHEDULES.find((s) => s.id === id);
      if (!target) return false;
      const updated: ScheduledMessageDto = {
        ...target,
        status: "cancelled",
        updated_at: Date.now(),
      };
      DEMO_SCHEDULES = DEMO_SCHEDULES.map((s) => (s.id === id ? updated : s));
      emitDemoScheduleUpdated(updated);
      return true;
    }
    return invoke<boolean>("schedule_cancel", { id });
  },
  async scheduleDelete(id: string): Promise<boolean> {
    if (!isTauri()) {
      const before = DEMO_SCHEDULES.length;
      DEMO_SCHEDULES = DEMO_SCHEDULES.filter((s) => s.id !== id);
      if (DEMO_SCHEDULES.length !== before) {
        emitDemoScheduleUpdated(null);
        return true;
      }
      return false;
    }
    return invoke<boolean>("schedule_delete", { id });
  },
  async scheduleSendNow(id: string): Promise<ScheduledMessageDto | null> {
    if (!isTauri()) {
      const target = DEMO_SCHEDULES.find((s) => s.id === id);
      if (!target) return null;
      const now = Date.now();
      const updated: ScheduledMessageDto = {
        ...target,
        status: "sent",
        sent_at_ts: now,
        updated_at: now,
      };
      DEMO_SCHEDULES = DEMO_SCHEDULES.map((s) => (s.id === id ? updated : s));
      emitDemoScheduleUpdated(updated);
      return { ...updated };
    }
    return invoke<ScheduledMessageDto | null>("schedule_send_now", { id });
  },
  async onScheduleUpdated(
    handler: (ev: ScheduledMessageDto | null) => void,
  ): Promise<() => void> {
    if (!isTauri()) {
      DEMO_SCHEDULE_LISTENERS.add(handler);
      return () => {
        DEMO_SCHEDULE_LISTENERS.delete(handler);
      };
    }
    return listen<ScheduledMessageDto | null>("core://schedule-updated", (event) => {
      handler(event.payload);
    });
  },

  // ==================== Item #3: Local Automation API ====================
  async localApiGetConfig(): Promise<LocalApiConfigDto> {
    if (!isTauri()) {
      return { ...DEMO_LOCAL_API_CONFIG };
    }
    return invoke<LocalApiConfigDto>("local_api_get_config");
  },
  async localApiGetStatus(): Promise<LocalApiStatusDto> {
    if (!isTauri()) {
      return {
        enabled: DEMO_LOCAL_API_CONFIG.enabled,
        running: DEMO_LOCAL_API_CONFIG.enabled,
        bind_addr: `127.0.0.1:${DEMO_LOCAL_API_CONFIG.port}`,
        port: DEMO_LOCAL_API_CONFIG.port,
        active_clients: DEMO_API_CLIENTS.filter((c) => c.enabled).length,
      };
    }
    return invoke<LocalApiStatusDto>("local_api_get_status");
  },
  async localApiUpdateConfig(config: LocalApiConfigDto): Promise<LocalApiStatusDto> {
    if (!isTauri()) {
      DEMO_LOCAL_API_CONFIG = { ...config };
      return this.localApiGetStatus();
    }
    return invoke<LocalApiStatusDto>("local_api_update_config", { config });
  },
  async localApiKillSwitch(): Promise<LocalApiStatusDto> {
    if (!isTauri()) {
      DEMO_LOCAL_API_CONFIG = { ...DEMO_LOCAL_API_CONFIG, enabled: false };
      return this.localApiGetStatus();
    }
    return invoke<LocalApiStatusDto>("local_api_kill_switch");
  },
  async localApiListClients(): Promise<ApiClientDto[]> {
    if (!isTauri()) {
      return DEMO_API_CLIENTS.map((c) => ({ ...c }));
    }
    return invoke<ApiClientDto[]>("local_api_list_clients");
  },
  async localApiCreateClient(input: CreateApiClientInput): Promise<CreatedApiClientDto> {
    if (!isTauri()) {
      const now = Date.now();
      const rawToken = `teza_demo_${Math.random().toString(36).slice(2)}${now.toString(36)}`;
      const client: ApiClientDto = {
        id: `demo-client-${now}`,
        name: input.name,
        token_prefix: rawToken.slice(0, 12),
        scopes: [...input.scopes],
        allowed_thread_ids: [...input.allowed_thread_ids],
        rate_limit_per_min: input.rate_limit_per_min,
        enabled: true,
        created_at: now,
        last_used_at: 0,
      };
      DEMO_API_CLIENTS = [client, ...DEMO_API_CLIENTS];
      return { client: { ...client }, raw_token: rawToken };
    }
    return invoke<CreatedApiClientDto>("local_api_create_client", {
      name: input.name,
      scopes: input.scopes,
      allowedThreadIds: input.allowed_thread_ids,
      rateLimitPerMin: input.rate_limit_per_min,
    });
  },
  async localApiUpdateClient(
    id: string,
    input: UpdateApiClientInput,
  ): Promise<ApiClientDto | null> {
    if (!isTauri()) {
      const idx = DEMO_API_CLIENTS.findIndex((c) => c.id === id);
      if (idx < 0) return null;
      const updated: ApiClientDto = {
        ...DEMO_API_CLIENTS[idx],
        ...(typeof input.enabled === "boolean" ? { enabled: input.enabled } : {}),
        ...(input.scopes !== undefined ? { scopes: [...input.scopes] } : {}),
        ...(input.allowed_thread_ids !== undefined
          ? { allowed_thread_ids: [...input.allowed_thread_ids] }
          : {}),
        ...(typeof input.rate_limit_per_min === "number"
          ? { rate_limit_per_min: input.rate_limit_per_min }
          : {}),
      };
      DEMO_API_CLIENTS = DEMO_API_CLIENTS.map((c) => (c.id === id ? updated : c));
      return { ...updated };
    }
    return invoke<ApiClientDto | null>("local_api_update_client", {
      id,
      enabled: input.enabled ?? null,
      scopes: input.scopes ?? null,
      allowedThreadIds: input.allowed_thread_ids ?? null,
      rateLimitPerMin: input.rate_limit_per_min ?? null,
    });
  },
  async localApiDeleteClient(id: string): Promise<boolean> {
    if (!isTauri()) {
      const before = DEMO_API_CLIENTS.length;
      DEMO_API_CLIENTS = DEMO_API_CLIENTS.filter((c) => c.id !== id);
      return DEMO_API_CLIENTS.length !== before;
    }
    return invoke<boolean>("local_api_delete_client", { id });
  },
  async localApiListAuditLogs(limit?: number): Promise<ApiAuditLogDto[]> {
    if (!isTauri()) {
      return (limit ? DEMO_API_AUDIT_LOGS.slice(0, limit) : DEMO_API_AUDIT_LOGS).map((row) => ({
        ...row,
      }));
    }
    return invoke<ApiAuditLogDto[]>("local_api_list_audit_logs", {
      limit: limit ?? null,
    });
  },
  async localApiClearAuditLogs(): Promise<number> {
    if (!isTauri()) {
      const count = DEMO_API_AUDIT_LOGS.length;
      DEMO_API_AUDIT_LOGS = [];
      return count;
    }
    return invoke<number>("local_api_clear_audit_logs");
  },
  async onLocalApiAudit(handler: (ev: ApiAuditLogDto) => void): Promise<() => void> {
    if (!isTauri()) {
      DEMO_LOCAL_API_AUDIT_LISTENERS.add(handler);
      return () => {
        DEMO_LOCAL_API_AUDIT_LISTENERS.delete(handler);
      };
    }
    return listen<ApiAuditLogDto>("core://local-api-audit", (event) => {
      handler(event.payload);
    });
  },
};
