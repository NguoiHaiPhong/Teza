import { invoke } from "@tauri-apps/api/core";
import type {
  AccountProfile,
  AddGroupMembersResultDto,
  AppUpdateInfo,
  AttachmentMeta,
  ChatFolderDto,
  FolderFilter,
  MessageDto,
  NotifyToastPayload,
  PollDto,
  RelayConfigSummary,
  RelayConfigUpdate,
  RelayTestResult,
  StickerItem,
  ThreadDetailsDto,
  ThreadDto,
  TranslateConfigSummary,
  TranslateConfigUpdate,
  TranslateProvider,
  TranslationResult,
  UserProfileDto,
} from "./types";

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
  async getThreadDetails(threadId: string): Promise<ThreadDetailsDto> {
    if (!isTauri()) {
      const msgs = DEMO_MESSAGES[threadId] ?? [];
      return {
        thread_id: threadId,
        total_members: null,
        members: [],
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
      return {
        uid: userId,
        display_name: t?.title ?? `Người dùng ${userId}`,
        zalo_name: t?.title ?? `Người dùng ${userId}`,
        avatar: t?.avatar ?? "",
        cover: "",
        status: "Đang hoạt động trên Teza (Chế độ xem trước)",
        gender: null,
        sdob: "",
        phone_number: "",
        is_friend: t ? true : false,
        is_self: userId === "cloud-me" || userId === "me",
        mutual_groups: DEMO_THREADS.filter((g) => g.kind === "group").map((g) => ({
          id: g.id,
          title: g.title,
          avatar: g.avatar,
          total_members: 12,
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
      return {
        uid: `phone-${norm}`,
        display_name: `Liên hệ ${norm}`,
        zalo_name: `Liên hệ ${norm}`,
        avatar: "",
        cover: "",
        status: "Tìm kiếm qua số điện thoại (Chế độ xem trước)",
        gender: null,
        sdob: "",
        phone_number: norm,
        is_friend: false,
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
    });
  },
  async saveRemoteFile(url: string, suggestedName: string): Promise<string | null> {
    if (!isTauri()) {
      const a = document.createElement("a");
      a.href = url;
      a.download = suggestedName || "download";
      a.target = "_blank";
      a.rel = "noreferrer";
      document.body.appendChild(a);
      a.click();
      a.remove();
      return null;
    }
    return invoke<string | null>("save_remote_file", { url, suggestedName });
  },
  async fetchImageDataUrl(url: string): Promise<string> {
    if (!isTauri() || url.startsWith("data:image/")) {
      return url;
    }
    return invoke<string>("fetch_image_data_url", { url });
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
};
