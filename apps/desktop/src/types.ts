export type ThemeMode = "triforce" | "night-blue" | "dark" | "light";

export type TranslateProvider = "deepl";

export type TranslateDetectMode = "api_cache" | "local_smart_skip";

export interface TranslateConfigSummary {
  provider: TranslateProvider;
  detect_mode: TranslateDetectMode;
  target_lang: string;
  has_deepl_key: boolean;
  deepl_preview: string;
}

export interface TranslateConfigUpdate {
  provider: TranslateProvider;
  target_lang: string;
  detect_mode?: TranslateDetectMode;
  deepl_key?: string;
  clear_deepl_key?: boolean;
}

export interface TranslationResult {
  msg_id: string;
  translated_text: string;
  detected_source_lang: string | null;
  skipped?: boolean;
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
  is_buzz?: boolean;
  buzz_streak?: number;
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
  creator_id?: string;
  admin_ids?: string[];
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
  failedAttempts?: number;
  lockoutUntilMs?: number;
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
  account_name: string;
  avatar: string;
  cover: string;
  status: string;
  gender: number | null;
  sdob: string;
  phone_number: string;
  is_friend: boolean | null;
  is_blocked?: boolean | null;
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
  is_live?: boolean;
  send_status?: "sending" | "failed";
  send_error?: string;
}

export interface MessagesAroundResult {
  messages: MessageDto[];
  has_more?: boolean;
  has_newer: boolean;
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
      id?: string;
      kind: "path";
      path: string;
      fileName: string;
      sizeBytes: number;
      isImage: boolean;
    }
  | {
      id?: string;
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

export type DownloadStage = "downloading" | "saving" | "done" | "cancelled" | "error";

export interface DownloadProgressDto {
  id: string;
  file_name: string;
  stage: DownloadStage;
  downloaded_bytes: number;
  total_bytes: number | null;
  saved_path: string | null;
  can_open_directly: boolean;
  error: string | null;
}

export interface SavedFileResultDto {
  path: string;
  file_name: string;
  can_open_directly: boolean;
}

export type CallPhase =
  | "idle"
  | "dialing"
  | "ringing_outgoing"
  | "incoming_ringing"
  | "connecting"
  | "connected";

export type CallType = "DirectVoice" | "DirectVideo";

export type CallEndReason =
  | "local_hangup"
  | "local_declined"
  | "local_cancelled"
  | "remote_ended"
  | "remote_cancelled"
  | "answered_on_other_device"
  | "ended_on_other_device"
  | "ring_timeout"
  | { remote_rejected: { code: number; message: string } }
  | { error: string };

export interface CallSnapshotDto {
  call_id: number;
  partner_id: string;
  partner_name: string;
  partner_avatar: string;
  call_type: CallType;
  is_caller: boolean;
  phase: CallPhase;
  mic_muted: boolean;
  peer_mic_muted: boolean;
  cam_enabled: boolean;
  peer_cam_enabled: boolean;
  duration_secs: number;
}

export interface CallStateDto {
  available: boolean;
  unavailable_reason: string | null;
  phase: CallPhase;
  active: CallSnapshotDto | null;
}

export type CallEvent =
  | { type: "state_changed"; phase: CallPhase; call_id: number | null }
  | {
      type: "incoming_call";
      call_id: number;
      caller_id: string;
      caller_name: string;
      call_type: CallType;
    }
  | { type: "remote_ringing"; call_id: number }
  | { type: "connected"; call_id: number; partner_id: string; rtp_ip: string }
  | { type: "dismissed_by_other_device"; call_id: number; answered_elsewhere: boolean }
  | {
      type: "call_ended";
      call_id: number;
      partner_id: string;
      duration_secs: number;
      reason: CallEndReason;
    }
  | { type: "media_state_changed"; call_id: number; mic_muted: boolean }
  | { type: "peer_media_changed"; call_id: number; mic_muted: boolean }
  | { type: "camera_state_changed"; call_id: number; cam_enabled: boolean }
  | { type: "peer_camera_changed"; call_id: number; cam_enabled: boolean }
  | { type: "media_warning"; call_id: number; message: string }
  | {
      type: "signaling_warning";
      call_id: number;
      command: number;
      code: number | null;
      message: string;
    };

export interface CallEventEnvelopeDto {
  event: CallEvent;
  state: CallStateDto;
  partner_name: string | null;
  partner_avatar: string | null;
}

export interface CallTelemetryDto {
  call_id: number;
  duration_secs: number;
  mic_muted: boolean;
  has_input_device: boolean;
  mic_level_permille: number;
  remote_level_permille: number;
  remote_speaking: boolean;
  tx_packets: number;
  rx_packets: number;
  active_input_device: string | null;
  active_output_device: string | null;
  input_is_virtual: boolean;
  output_is_virtual: boolean;
  tx_errors: number;
  video_cam_enabled?: boolean;
  video_peer_active?: boolean;
  video_synced?: boolean;
  video_tx_frames?: number;
  video_rx_frames?: number;
  video_rx_dropped_incomplete?: number;
}

export interface CallVideoFrameDto {
  call_id: number;
  ssrc: number;
  sequence_number: number;
  timestamp: number;
  is_keyframe: boolean;
  just_synced: boolean;
  data_b64: string;
}

export interface CallVideoKeyframeRequestDto {
  call_id: number;
}

export interface AudioDeviceInfoDto {
  name: string;
  is_default: boolean;
  is_virtual?: boolean;
}

export interface CallDevicesDto {
  inputs: AudioDeviceInfoDto[];
  outputs: AudioDeviceInfoDto[];
  selected_input: string | null;
  selected_output: string | null;
}

export interface CallAudioTestResultDto {
  output_device: string | null;
  output_is_virtual: boolean;
  input_device: string | null;
  input_is_virtual: boolean;
  has_input_device: boolean;
  tx_packets: number;
  rx_packets: number;
}

export interface CallCardPayload {
  callType: "voice" | "video";
  outcome: "completed" | "missed" | "cancelled" | "declined";
  durationSecs: number;
}

export interface WebAppAccountDto {
  id: string;
  app_id: string;
  origin: string;
  label: string;
  username: string;
  has_password: boolean;
  auto_fill: boolean;
  updated_at_ms: number;
}

export interface WebAppAccountInput {
  id?: string | null;
  app_id: string;
  url: string;
  label: string;
  username: string;
  password?: string | null;
  clear_password?: boolean;
  auto_fill: boolean;
}

export interface MediaVaultConfigDto {
  custom_vault_dir: string | null;
  active_vault_dir: string;
  quota_mb: number;
  encrypt_at_rest: boolean;
  auto_cache_images: boolean;
  auto_cache_voice: boolean;
  auto_cache_video_files: boolean;
}

export interface MediaVaultStatsDto {
  total_bytes: number;
  object_count: number;
  pinned_bytes: number;
  pinned_count: number;
  image_bytes: number;
  video_bytes: number;
  voice_bytes: number;
  file_bytes: number;
  thumb_bytes: number;
  cloud_synced_count: number;
  cloud_total_bytes: number;
}

export interface GDriveConfigDto {
  client_id: string;
  has_client_secret: boolean;
  has_builtin_oauth: boolean;
  token_broker_url: string;
  has_builtin_token_broker: boolean;
  oauth_mode: string;
  sync_enabled: boolean;
  sync_media: boolean;
  sync_history: boolean;
  auto_sync_interval_secs: number;
}

export interface GDriveConfigPatch {
  clientId?: string | null;
  clientSecret?: string | null;
  tokenBrokerUrl?: string | null;
  resetCustomOAuth?: boolean | null;
  syncEnabled?: boolean | null;
  syncMedia?: boolean | null;
  syncHistory?: boolean | null;
}

export interface GDriveStatusDto {
  relay_blocked: boolean;
  oauth_configured: boolean;
  connected: boolean;
  account_email: string | null;
  account_name: string | null;
  root_folder_id?: string | null;
  vault_unlocked: boolean;
  has_remote_manifest: boolean;
  sync_enabled: boolean;
  sync_media: boolean;
  sync_history: boolean;
  syncing: boolean;
  phase: string;
  progress_done: number;
  progress_total: number;
  pending_media_count: number;
  pending_media_bytes: number;
  last_sync_ts: number | null;
  last_history_sync_ts: number | null;
  last_error: string | null;
}

export interface GDriveQuotaDto {
  drive_limit_bytes: number | null;
  drive_usage_bytes: number;
  drive_usage_in_drive_bytes: number;
  drive_usage_in_trash_bytes: number;
}

export interface GDriveThreadUsageDto {
  thread_id: string;
  thread_name: string;
  total_bytes: number;
  object_count: number;
}

export interface GDriveCloudItemDto {
  sha256: string;
  cloud_file_id: string;
  kind: string;
  mime: string;
  size_bytes: number;
  original_name: string | null;
  thread_id: string | null;
  thread_name: string | null;
  msg_id: string | null;
  cloud_synced_ts: number;
  local_state: number;
}

export interface GDriveStorageOverviewDto {
  quota: GDriveQuotaDto | null;
  teza_cloud_total_bytes: number;
  teza_cloud_object_count: number;
  images_bytes: number;
  videos_bytes: number;
  voice_bytes: number;
  files_bytes: number;
  history_bytes: number;
  history_file_count: number;
  by_thread: GDriveThreadUsageDto[];
  largest_items: GDriveCloudItemDto[];
}

export interface GDrivePruneResultDto {
  deleted_count: number;
  freed_bytes: number;
  failed_count: number;
}

export interface FriendRequestItemDto {
  user_id: string;
  display_name: string;
  account_name: string;
  avatar: string;
  message: string;
  created_ts: number;
  direction: "received" | "sent" | string;
  is_seen: boolean;
}

export interface FriendRequestsListDto {
  received: FriendRequestItemDto[];
  sent: FriendRequestItemDto[];
}

export interface FriendActionResultDto {
  ok: boolean;
  user_id: string;
  action: string;
  already_friends: boolean;
  auto_accepted: boolean;
  message: string;
}

export interface LeaveGroupResultDto {
  group_id: string;
  left: boolean;
  member_error: string[];
  message: string;
}

export interface KickGroupMembersResultDto {
  group_id: string;
  removed: string[];
  failed: string[];
  message: string;
}

export interface SocialWsEventDto {
  kind: string;
  thread_id: string | null;
  user_id: string | null;
  self_removed: boolean;
}

export type {
  AiBackendKind,
  AiCliProbeDto,
  AiConfigDto,
  AiConfigUpdateDto,
  AiInstallZeroClawResponseDto,
  AiStoredMessageDto,
  AiSummaryDto,
  ApiAuditLogDto,
  ApiClientDto,
  ApiClientScope,
  CreateApiClientInput,
  CreateScheduledMessageInput,
  CreatedApiClientDto,
  LocalApiConfigDto,
  LocalApiStatusDto,
  ScheduleMisfirePolicy,
  ScheduleRecurrence,
  ScheduleStatus,
  ScheduledMessageDto,
  UpdateApiClientInput,
  UpdateScheduledMessageInput,
  ZeroClawInstallProgressDto,
  ZeroClawInstallResultDto,
} from "./utils/automationHelpers";

export type {
  AiApprovalDecision,
  AiMcpStatusDto,
  AiToolActivityEvent,
  AiToolApprovalRequest,
  AiToolAuditEntry,
  SystemAccessLevel,
  TezaAccessLevel,
} from "./utils/aiPermissions";

export interface AiStreamDeltaEvent {
  session_id: string;
  delta: string;
}

export interface AiSummaryDeltaEvent {
  thread_id: string;
  delta: string;
}

export interface AiToolApprovalResolvedEvent {
  approval_id: string;
  session_id?: string;
  decision?: "allow_once" | "allow_session" | "deny" | "timeout" | "cancelled" | "blocked" | string;
}

export interface AiComposeDraftEvent {
  thread_id: string;
  text: string;
  open_thread?: boolean;
}

export interface AiOpenThreadEvent {
  thread_id: string;
}

export interface AiEmergencyStopResult {
  cancelled_approvals: number;
  killed_processes: number;
}
