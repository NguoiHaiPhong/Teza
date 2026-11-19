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
}

export interface AccountProfile {
  uid: string;
  display_name: string;
  avatar: string;
  send2me_id?: string;
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
