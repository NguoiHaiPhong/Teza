export type ThemeMode = "triforce" | "night-blue" | "dark" | "light";

export type TranslateProvider = "deepl" | "gemini" | "azure";

export interface TranslateConfigSummary {
  provider: TranslateProvider;
  target_lang: string;
  gemini_model: string;
  azure_region: string;
  has_deepl_key: boolean;
  has_gemini_key: boolean;
  has_azure_key: boolean;
  deepl_preview: string;
  gemini_preview: string;
  azure_preview: string;
}

export interface TranslateConfigUpdate {
  provider: TranslateProvider;
  target_lang: string;
  deepl_key?: string;
  gemini_key?: string;
  gemini_model?: string;
  azure_key?: string;
  azure_region?: string;
}

export interface TranslationResult {
  msg_id: string;
  translated_text: string;
  detected_source_lang: string | null;
}

export type FolderFilter = "all" | "unread" | "direct" | "group" | "cloud";

export type BuiltInRailFolderId = "unread" | "direct" | "group" | "cloud";

export type RailDisplayMode = "icon-text" | "icon-only" | "text-only";

export interface AddGroupMembersResultDto {
  added: string[];
  pending: string[];
  failed: string[];
  message: string;
}

export interface PollOptionDto {
  id: number;
  content: string;
  votes: number;
  voted: boolean;
}

export interface PollDto {
  poll_id: number;
  question: string;
  options: PollOptionDto[];
  allow_multi: boolean;
  allow_add_option: boolean;
  is_anonymous: boolean;
  expired_time: number;
  closed: boolean;
}

export interface ReminderPayload {
  title: string;
  startTime: number;
  repeat: number;
  reminderId: string;
  scheduleText?: string;
}

export type ThreadKind = "direct" | "group" | "cloud";

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

export interface ThreadDto {
  id: string;
  title: string;
  avatar: string;
  kind: ThreadKind;
  last_preview: string;
  last_ts: number;
  unread_count: number;
  is_pinned: boolean;
  is_muted: boolean;
  last_read_in_ts?: number;
  peer_read_ts?: number;
  muted_until_ts?: number;
  is_archived?: boolean;
}

export type NotifyPosition = "top-left" | "top-right" | "bottom-left" | "bottom-right";
export type NotifyPreviewMode = "full" | "sender_only" | "hidden";
export type NotifyBadgeMode = "unread_messages" | "unread_chats";

export interface NotificationPrefs {
  enabled: boolean;
  sound: boolean;
  buzz: boolean;
  notifyDirect: boolean;
  notifyGroup: boolean;
  mentionOverride: boolean;
  notifyWhenFocused: boolean;
  previewMode: NotifyPreviewMode;
  position: NotifyPosition;
  durationSec: number;
  badgeMode: NotifyBadgeMode;
  includeMutedInBadge: boolean;
  useCustomPopup: boolean;
}

export interface NotifyToastPayload {
  thread_id: string;
  title: string;
  body: string;
  avatar: string;
  theme: string;
  position: NotifyPosition;
  duration_ms: number;
}

export interface ThreadMemberDto {
  id: string;
  name: string;
  avatar: string;
  role: string;
}

export interface ThreadDetailsDto {
  thread_id: string;
  total_members: number | null;
  members: ThreadMemberDto[];
  media_items: MessageDto[];
  file_items: MessageDto[];
  link_items: MessageDto[];
}

export interface ChatFolderDto {
  id: string;
  name: string;
  icon: string;
  color: string;
  thread_ids: string[];
  excluded_thread_ids: string[];
  include_kinds: ThreadKind[];
  only_unread: boolean;
  exclude_muted: boolean;
  position: number;
  remote_label_id: string | null;
}

export interface PasscodeConfig {
  enabled: boolean;
  salt: string;
  hash: string;
  autoLockMinutes: number;
}

export interface MutualGroupDto {
  id: string;
  title: string;
  avatar: string;
  total_members: number | null;
}

export interface UserProfileDto {
  uid: string;
  display_name: string;
  zalo_name: string;
  avatar: string;
  cover: string;
  status: string;
  gender: number | null;
  sdob: string;
  phone_number: string;
  is_friend: boolean | null;
  is_self: boolean;
  mutual_groups?: MutualGroupDto[];
  local_message_count?: number;
  local_media_count?: number;
  last_interaction_ts?: number;
}

export interface QuoteRef {
  msg_id: string;
  sender_name: string;
  snippet: string;
}

export interface ReactionSummary {
  emoji: string;
  count: number;
  me: boolean;
}

export interface MessageDto {
  msg_id: string;
  thread_id: string;
  sender_id: string;
  sender_name: string;
  text: string;
  msg_type: string;
  ts: number;
  is_outgoing: boolean;
  is_recalled: boolean;
  is_pinned: boolean;
  quote: QuoteRef | null;
  reactions: ReactionSummary[];
  attachment_url: string | null;
  attachment_name: string | null;
  send_status?: "sending" | "failed";
  send_error?: string;
}

export interface AccountProfile {
  uid: string;
  display_name: string;
  avatar: string;
  send2me_id?: string;
}

export interface StickerItem {
  id: number;
  cate_id: number;
  sticker_type: number;
  url: string;
  category?: string;
}

export type RelayMode = "socks5" | "http";

export interface RelayConfigSummary {
  enabled: boolean;
  mode: RelayMode;
  host: string;
  port: number;
  username: string;
  has_password: boolean;
  password_preview: string;
  route_webview_media: boolean;
}

export interface RelayConfigUpdate {
  enabled: boolean;
  mode: RelayMode;
  host: string;
  port: number;
  username: string;
  password?: string;
  clear_password?: boolean;
  route_webview_media?: boolean;
}

export interface RelayTestResult {
  latency_ms: number;
}

export interface AppUpdateInfo {
  version: string;
  current_version: string;
  body: string | null;
  date: string | null;
}

export interface AttachmentMeta {
  path: string;
  file_name: string;
  size_bytes: number;
  is_image: boolean;
}

export type PendingAttachment =
  | {
      kind: "path";
      path: string;
      fileName: string;
      sizeBytes: number;
      isImage: boolean;
    }
  | {
      kind: "raw";
      file: File;
      fileName: string;
      sizeBytes: number;
      isImage: boolean;
      previewUrl: string | null;
    };

export interface MonitorInfoDto {
  index: number;
  label: string;
  left: number;
  top: number;
  width: number;
  height: number;
  is_primary: boolean;
}

export interface ScreenCaptureDto {
  data_url: string;
  width: number;
  height: number;
  monitors: MonitorInfoDto[];
  active_monitor_index: number | null;
}
