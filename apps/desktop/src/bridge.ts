import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AccountProfile,
  AddGroupMembersResultDto,
  AppUpdateInfo,
  AttachmentMeta,
  CallAudioTestResultDto,
  CallDevicesDto,
  CallStateDto,
  ChatFolderDto,
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
  MediaVaultConfigDto,
  MediaVaultStatsDto,
  MessageDto,
  NotifyToastPayload,
  PollDto,
  RelayConfigSummary,
  RelayConfigUpdate,
  RelayTestResult,
  SavedFileResultDto,
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
  UserProfileDto,
  WebAppAccountDto,
  WebAppAccountInput,
} from "./types";
import { filterDemoWebAppAccounts, upsertDemoWebAppAccount } from "./utils/webappAccounts.ts";

export const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const MAX_ATTACHMENT_BYTES = 100 * 1024 * 1024;

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
  async findUserByPhone(phone: string): Promise<UserProfileDto> {
    if (!isTauri()) {
      const cleaned = phone.trim().replace(/[\s\-().\t+]/g, "");
      if (!/^\d{9,15}$/.test(cleaned)) {
        throw new Error("Số điện thoại không hợp lệ");
      }
      const norm = cleaned.startsWith("0") ? `84${cleaned.slice(1)}` : cleaned;
      const uid = `phone-${norm}`;
      return {
        uid,
        display_name: `Liên hệ ${norm}`,
        account_name: `Liên hệ ${norm}`,
        avatar: "",
        cover: "",
        status: "Tìm kiếm qua số điện thoại (Chế độ xem trước)",
        gender: null,
        sdob: "",
        phone_number: norm,
        is_friend: DEMO_FRIENDS.has(uid),
        is_blocked: DEMO_BLOCKED.has(uid),
        is_self: false,
        mutual_groups: [],
        local_message_count: 0,
        local_media_count: 0,
        last_interaction_ts: 0,
      };
    }
    return invoke<UserProfileDto>("user_find_by_phone", { phone });
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
  async loadMessagesAround(threadId: string, targetTs: number): Promise<MessagesPage> {
    if (!isTauri()) {
      return { messages: DEMO_MESSAGES[threadId] ?? [], has_more: false };
    }
    return invoke<MessagesPage>("messages_load_around", { threadId, targetTs });
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
    return invoke<MessageDto>("message_send_raw_attachment", bytes, {
      headers: {
        "x-thread-id": encodeURIComponent(threadId),
        "x-file-name": encodeURIComponent(fileName),
        "x-caption": encodeURIComponent(caption),
      },
    });
  },
  async searchGlobal(query: string): Promise<MessageDto[]> {
    if (!isTauri()) {
      const q = query.trim().toLowerCase();
      if (!q) return [];
      return Object.values(DEMO_MESSAGES)
        .flat()
        .filter((m) => m.text.toLowerCase().includes(q));
    }
    return invoke<MessageDto[]>("search_global", { query, limit: 30 });
  },
  async getTranslateConfig(): Promise<TranslateConfigSummary> {
    if (!isTauri()) {
      return {
        provider: "deepl",
        detect_mode: "api_cache",
        target_lang: "VI",
        gemini_model: "gemini-2.0-flash",
        azure_region: "eastasia",
        has_deepl_key: false,
        has_gemini_key: false,
        has_azure_key: false,
        deepl_preview: "",
        gemini_preview: "",
        azure_preview: "",
      };
    }
    return invoke<TranslateConfigSummary>("translate_get_config");
  },
  async saveTranslateConfig(update: TranslateConfigUpdate): Promise<TranslateConfigSummary> {
    if (!isTauri()) {
      return {
        provider: update.provider,
        detect_mode: update.detect_mode ?? "api_cache",
        target_lang: update.target_lang,
        gemini_model: update.gemini_model || "gemini-2.0-flash",
        azure_region: update.azure_region || "eastasia",
        has_deepl_key: !!update.deepl_key,
        has_gemini_key: !!update.gemini_key,
        has_azure_key: !!update.azure_key,
        deepl_preview: update.deepl_key ? "****" : "",
        gemini_preview: update.gemini_key ? "****" : "",
        azure_preview: update.azure_key ? "****" : "",
      };
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
      a.download = suggestedName || "download";
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
      const list = threadId ? DEMO_MESSAGES[threadId] ?? [] : [];
      const found = list.find((m) => m.msg_id === msgId);
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
      const list = threadId ? DEMO_MESSAGES[threadId] ?? [] : [];
      const found = list.find((m) => m.msg_id === msgId);
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
};
