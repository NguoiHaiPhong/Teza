import type { UiLanguage } from "../i18n/types.ts";
import {
  normalizeEpochMs,
  type AiConfigDto,
  type AiConfigUpdateDto,
  type BadgeTone,
} from "./automationHelpers.ts";

// ==================== Shared MCP & 2-Axis Permission Types ====================

export type TezaAccessLevel = "isolated" | "read_only" | "supervised" | "autonomous";
export type SystemAccessLevel = "safe" | "supervised" | "full";

export interface AiToolActivityEvent {
  session_id: string;
  tool_call_id: string;
  tool_name: string;
  status: "pending" | "completed" | "failed";
  summary?: string;
}

export interface AiToolApprovalRequest {
  approval_id: string;
  session_id: string;
  tool_name: string;
  category: "teza" | "system";
  risk: "medium" | "high";
  title: string;
  summary: string;
  details: Record<string, unknown>;
  created_ts: number;
  timeout_secs: number;
}

export type AiApprovalDecision = "allow_once" | "allow_session" | "deny";

export interface AiToolAuditEntry {
  id: string;
  ts: number;
  session_id: string;
  tool_name: string;
  category: "teza" | "system";
  decision: "auto" | "allow_once" | "allow_session" | "deny" | "timeout" | "blocked";
  ok: boolean;
  summary: string;
  error?: string | null;
}

export interface AiMcpStatusDto {
  running: boolean;
  exposed_tools_count: number;
  pending_approvals_count: number;
  active_powershell_count: number;
  workspace_dir: string;
  forbidden_paths: string[];
}

export interface AiConfigPermissionsFields {
  teza_access_level?: TezaAccessLevel;
  system_access_level?: SystemAccessLevel;
  zeroclaw_allowed_roots?: string[];
  zeroclaw_allowed_thread_ids?: string[];
}

export type AiConfigWithPermissions = AiConfigDto & AiConfigPermissionsFields;
export type AiConfigUpdateWithPermissions = AiConfigUpdateDto & AiConfigPermissionsFields;

export type AccessBadgeTone = "neutral" | "info" | "warn" | "danger";

export interface AccessBadgeMeta {
  tone: AccessBadgeTone;
  tezaTone: AccessBadgeTone;
  systemTone: AccessBadgeTone;
  isHighRisk: boolean;
  isElevated: boolean;
}

export type ApprovalDetailView =
  | {
      kind: "powershell";
      command: string;
      cwd?: string;
      timeoutSecs?: number;
    }
  | {
      kind: "write_file";
      path: string;
      mode?: string;
      contentPreview?: string;
      truncated?: boolean;
    }
  | {
      kind: "open_path";
      path: string;
    }
  | {
      kind: "message";
      recipient?: string;
      text?: string;
      filePath?: string;
      scheduleTime?: string;
      recurrence?: string;
      replyTo?: string;
    }
  | {
      kind: "generic";
      entries: { key: string; value: string }[];
    };

// ==================== Constants ====================

export const DEFAULT_TEZA_ACCESS_LEVEL: TezaAccessLevel = "isolated";
export const DEFAULT_SYSTEM_ACCESS_LEVEL: SystemAccessLevel = "safe";
export const MAX_ALLOWED_ROOTS = 32;
export const MAX_ALLOWED_ROOT_LENGTH = 512;
export const MAX_ALLOWED_THREAD_IDS = 200;

export const DEFAULT_FORBIDDEN_PATHS_SUMMARY: readonly string[] = [
  "C:\\Windows",
  "C:\\Windows\\System32",
  "C:\\Program Files",
  "~/.ssh",
  "~/.gnupg",
  "~/.aws",
  "Teza DB & DPAPI keys",
];

// ==================== Normalization & Risk Helpers ====================

export function normalizeTezaAccessLevel(raw: unknown): TezaAccessLevel {
  switch (raw) {
    case "read_only":
    case "supervised":
    case "autonomous":
    case "isolated":
      return raw;
    default:
      return DEFAULT_TEZA_ACCESS_LEVEL;
  }
}

export function normalizeSystemAccessLevel(raw: unknown): SystemAccessLevel {
  switch (raw) {
    case "supervised":
    case "full":
    case "safe":
      return raw;
    default:
      return DEFAULT_SYSTEM_ACCESS_LEVEL;
  }
}

/**
 * Returns `true` when ZeroClaw can read/act on Teza data while having Full Windows
 * privileges (Prompt Injection escalation surface) OR when Teza access is Autonomous (T3).
 */
export function isHighRiskCombination(
  tezaLevel: TezaAccessLevel,
  systemLevel: SystemAccessLevel,
): boolean {
  return (tezaLevel !== "isolated" && systemLevel === "full") || tezaLevel === "autonomous";
}

export function getAccessBadgeTone(
  tezaLevel: TezaAccessLevel,
  systemLevel: SystemAccessLevel,
): AccessBadgeMeta {
  const tezaTone: AccessBadgeTone =
    tezaLevel === "autonomous"
      ? "danger"
      : tezaLevel === "supervised"
        ? "warn"
        : tezaLevel === "read_only"
          ? "info"
          : "neutral";

  const systemTone: AccessBadgeTone =
    systemLevel === "full" ? "danger" : systemLevel === "supervised" ? "warn" : "neutral";

  const isHighRisk = isHighRiskCombination(tezaLevel, systemLevel);
  const isElevated = tezaLevel !== "isolated" || systemLevel !== "safe";

  let tone: AccessBadgeTone = "neutral";
  if (isHighRisk || systemLevel === "full") {
    tone = "danger";
  } else if (tezaLevel === "supervised" || systemLevel === "supervised") {
    tone = "warn";
  } else if (tezaLevel === "read_only") {
    tone = "info";
  }

  return {
    tone,
    tezaTone,
    systemTone,
    isHighRisk,
    isElevated,
  };
}

function cleanSingleAllowedRoot(raw: string): { display: string; dedupeKey: string } | null {
  if (typeof raw !== "string") return null;
  let trimmed = raw.trim();
  if (!trimmed) return null;
  // Strip wrapping double quotes commonly copied via Windows "Copy as path".
  if (trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    trimmed = trimmed.slice(1, -1).trim();
  }
  if (!trimmed || trimmed.length > MAX_ALLOWED_ROOT_LENGTH) return null;
  if (/[\x00-\x1f\x7f]/.test(trimmed)) return null;

  // Preserve Windows drive roots such as "C:\" while stripping trailing separators elsewhere.
  const driveRootMatch = /^([A-Za-z]:)[\\/]+$/.exec(trimmed);
  if (driveRootMatch) {
    const display = `${driveRootMatch[1]}\\`;
    return {
      display,
      dedupeKey: display.toLowerCase(),
    };
  }

  // Handle bare drive letter with colon ("C:") -> normalize key with trailing slash? Keep exact trimmed
  // except strip redundant trailing separators on nested paths ("C:\Users\Docs\" -> "C:\Users\Docs").
  let display = trimmed.replace(/[\\/]+$/, "");
  if (!display) return null;
  if (/^[A-Za-z]:$/.test(display)) {
    display = `${display}\\`;
  }

  const dedupeKey = display.replace(/\//g, "\\").toLowerCase();
  return { display, dedupeKey };
}

/**
 * Trims, deduplicates Windows paths case-insensitively (ignoring slash style and trailing slashes),
 * filters invalid/oversized entries, and caps at 32 roots.
 */
export function normalizeAllowedRoots(roots: readonly string[] | null | undefined): string[] {
  if (!Array.isArray(roots)) return [];
  const seen = new Set<string>();
  const out: string[] = [];

  for (const item of roots) {
    if (out.length >= MAX_ALLOWED_ROOTS) break;
    const cleaned = cleanSingleAllowedRoot(item);
    if (!cleaned) continue;
    if (seen.has(cleaned.dedupeKey)) continue;
    seen.add(cleaned.dedupeKey);
    out.push(cleaned.display);
  }
  return out;
}

export function normalizeAllowedThreadIds(ids: readonly string[] | null | undefined): string[] {
  if (!Array.isArray(ids)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of ids) {
    if (out.length >= MAX_ALLOWED_THREAD_IDS) break;
    if (typeof raw !== "string") continue;
    const clean = raw.trim();
    if (!clean || seen.has(clean)) continue;
    seen.add(clean);
    out.push(clean);
  }
  return out;
}

// ==================== Tool Naming & Display Helpers ====================

export function stripToolPrefix(toolName: string | null | undefined): string {
  if (typeof toolName !== "string") return "";
  return toolName.trim().replace(/^(?:mcp__)?teza__/i, "");
}

const TOOL_LABELS: Readonly<Record<string, Record<UiLanguage, string>>> = {
  // Teza Read & Draft (T1+)
  get_account_info: {
    vi: "Xem thông tin tài khoản",
    en: "Get account info",
    ko: "계정 정보 조회",
  },
  list_threads: {
    vi: "Liệt kê hội thoại",
    en: "List conversations",
    ko: "대화 목록 조회",
  },
  get_thread_details: {
    vi: "Xem chi tiết hội thoại",
    en: "Get conversation details",
    ko: "대화 상세 정보 조회",
  },
  get_messages: {
    vi: "Đọc lịch sử tin nhắn",
    en: "Read message history",
    ko: "메시지 기록 읽기",
  },
  search_messages: {
    vi: "Tìm kiếm tin nhắn",
    en: "Search messages",
    ko: "메시지 검색",
  },
  list_friends: {
    vi: "Xem danh sách bạn bè",
    en: "List friends",
    ko: "친구 목록 조회",
  },
  list_folders: {
    vi: "Xem thư mục hội thoại",
    en: "List chat folders",
    ko: "대화 폴더 목록 조회",
  },
  list_schedules: {
    vi: "Xem tin nhắn hẹn giờ",
    en: "List scheduled messages",
    ko: "예약 메시지 목록 조회",
  },
  export_attachment: {
    vi: "Xuất tệp đính kèm",
    en: "Export attachment",
    ko: "첨부 파일 내보내기",
  },
  draft_message: {
    vi: "Soạn bản nháp tin nhắn",
    en: "Draft message",
    ko: "메시지 초안 작성",
  },

  // Teza Action & Control (T2+)
  send_message: {
    vi: "Gửi tin nhắn",
    en: "Send message",
    ko: "메시지 보내기",
  },
  send_attachment: {
    vi: "Gửi tệp đính kèm",
    en: "Send attachment",
    ko: "첨부 파일 보내기",
  },
  react_message: {
    vi: "Thả cảm xúc tin nhắn",
    en: "React to message",
    ko: "메시지 반응 남기기",
  },
  recall_or_delete_message: {
    vi: "Thu hồi / xóa tin nhắn",
    en: "Recall or delete message",
    ko: "메시지 회수/삭제",
  },
  manage_thread_state: {
    vi: "Cập nhật trạng thái hội thoại",
    en: "Update conversation state",
    ko: "대화 상태 변경",
  },
  pin_chat_message: {
    vi: "Ghim / bỏ ghim tin nhắn",
    en: "Pin or unpin message",
    ko: "메시지 고정/해제",
  },
  create_schedule: {
    vi: "Tạo tin nhắn hẹn giờ",
    en: "Schedule message",
    ko: "메시지 예약 생성",
  },
  manage_schedule: {
    vi: "Quản lý lịch hẹn gửi",
    en: "Manage scheduled message",
    ko: "예약 메시지 관리",
  },
  create_poll: {
    vi: "Tạo bình chọn nhóm",
    en: "Create group poll",
    ko: "그룹 투표 만들기",
  },
  create_reminder: {
    vi: "Tạo nhắc hẹn",
    en: "Create reminder",
    ko: "알림 만들기",
  },
  manage_social: {
    vi: "Quản lý kết bạn & nhóm",
    en: "Manage friends & group",
    ko: "친구 및 그룹 관리",
  },
  leave_group: {
    vi: "Rời nhóm trò chuyện",
    en: "Leave group chat",
    ko: "그룹 채팅 나가기",
  },
  kick_group_member: {
    vi: "Mời thành viên ra khỏi nhóm",
    en: "Remove group member",
    ko: "그룹 멤버 내보내기",
  },
  kick_member: {
    vi: "Mời thành viên ra khỏi nhóm",
    en: "Remove group member",
    ko: "그룹 멤버 내보내기",
  },

  // Windows & Office MCP Tools (S1+)
  win_list_dir: {
    vi: "Liệt kê thư mục Windows",
    en: "List Windows folder",
    ko: "Windows 폴더 목록 보기",
  },
  win_read_file: {
    vi: "Đọc tệp Windows",
    en: "Read Windows file",
    ko: "Windows 파일 읽기",
  },
  win_write_file: {
    vi: "Ghi tệp Windows",
    en: "Write Windows file",
    ko: "Windows 파일 쓰기",
  },
  win_run_powershell: {
    vi: "Chạy lệnh PowerShell",
    en: "Run PowerShell command",
    ko: "PowerShell 명령 실행",
  },
  win_open_path: {
    vi: "Mở tệp / thư mục Windows",
    en: "Open Windows path",
    ko: "Windows 경로 열기",
  },

  // ZeroClaw built-in safe & native tools
  web_search_tool: {
    vi: "Tìm kiếm web",
    en: "Web search",
    ko: "웹 검색",
  },
  web_search: {
    vi: "Tìm kiếm web",
    en: "Web search",
    ko: "웹 검색",
  },
  web_fetch: {
    vi: "Đọc trang web",
    en: "Fetch web page",
    ko: "웹 페이지 읽기",
  },
  calculator: {
    vi: "Tính toán",
    en: "Calculator",
    ko: "계산기",
  },
  weather: {
    vi: "Xem thời tiết",
    en: "Check weather",
    ko: "날씨 확인",
  },
  memory_recall: {
    vi: "Tra cứu trí nhớ AI",
    en: "Recall AI memory",
    ko: "AI 메모리 조회",
  },
  memory_store: {
    vi: "Lưu vào trí nhớ AI",
    en: "Save to AI memory",
    ko: "AI 메모리에 저장",
  },
  memory_forget: {
    vi: "Xóa mục trí nhớ AI",
    en: "Forget AI memory",
    ko: "AI 메모리 삭제",
  },
  shell: {
    vi: "Chạy lệnh hệ thống",
    en: "Run shell command",
    ko: "셸 명령 실행",
  },
  file_read: {
    vi: "Đọc tệp cục bộ",
    en: "Read local file",
    ko: "로컬 파일 읽기",
  },
  file_write: {
    vi: "Ghi tệp cục bộ",
    en: "Write local file",
    ko: "로컬 파일 쓰기",
  },
};

export function formatToolDisplayName(toolName: string, lang: UiLanguage = "vi"): string {
  const base = stripToolPrefix(toolName);
  if (!base) {
    return lang === "en" ? "AI Tool" : lang === "ko" ? "AI 도구" : "Công cụ AI";
  }
  const normalizedKey = base.toLowerCase();
  const entry = TOOL_LABELS[normalizedKey];
  if (entry) {
    return entry[lang] ?? entry.vi;
  }
  const words = base
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!words) return base;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function formatTezaAccessShortLabel(
  level: TezaAccessLevel,
  lang: UiLanguage = "vi",
): string {
  const table: Record<TezaAccessLevel, Record<UiLanguage, string>> = {
    isolated: { vi: "T0 Cô lập", en: "T0 Isolated", ko: "T0 격리" },
    read_only: { vi: "T1 Đọc & Nháp", en: "T1 Read & Draft", ko: "T1 읽기/초안" },
    supervised: { vi: "T2 Có duyệt", en: "T2 Supervised", ko: "T2 승인 필요" },
    autonomous: { vi: "T3 Tự động", en: "T3 Autonomous", ko: "T3 자동 실행" },
  };
  return table[level]?.[lang] ?? table.isolated.vi;
}

export function formatSystemAccessShortLabel(
  level: SystemAccessLevel,
  lang: UiLanguage = "vi",
): string {
  const table: Record<SystemAccessLevel, Record<UiLanguage, string>> = {
    safe: { vi: "S0 An toàn", en: "S0 Safe", ko: "S0 안전" },
    supervised: { vi: "S1 Có duyệt", en: "S1 Supervised", ko: "S1 승인 필요" },
    full: { vi: "S2 Toàn quyền", en: "S2 Full", ko: "S2 전체 권한" },
  };
  return table[level]?.[lang] ?? table.safe.vi;
}

export function formatAuditDecisionLabel(
  decision: AiToolAuditEntry["decision"],
  lang: UiLanguage = "vi",
): string {
  const table: Record<AiToolAuditEntry["decision"], Record<UiLanguage, string>> = {
    auto: { vi: "Tự động", en: "Auto", ko: "자동" },
    allow_once: { vi: "Cho phép 1 lần", en: "Allowed once", ko: "1회 허용" },
    allow_session: { vi: "Cho phép phiên", en: "Allowed (session)", ko: "세션 허용" },
    deny: { vi: "Đã từ chối", en: "Denied", ko: "거부됨" },
    timeout: { vi: "Hết hạn", en: "Timed out", ko: "시간 초과" },
    blocked: { vi: "Bị chặn", en: "Blocked", ko: "차단됨" },
  };
  return table[decision]?.[lang] ?? String(decision);
}

export function getAuditDecisionTone(
  decision: AiToolAuditEntry["decision"],
  ok: boolean,
): BadgeTone {
  if (decision === "deny" || decision === "blocked" || decision === "timeout") {
    return "danger";
  }
  if (!ok) return "warn";
  if (decision === "auto") return "info";
  return "success";
}

// ==================== Approval & Activity Pure Helpers ====================

const ALWAYS_CONFIRM_DESTRUCTIVE_TOOLS = new Set([
  "leave_group",
  "kick_group_member",
  "kick_member",
]);

/**
 * Returns `false` for high-risk or permanently destructive actions where
 * "Always allow this type in session" must stay hidden.
 */
export function isAllowSessionPermitted(
  req: Pick<AiToolApprovalRequest, "risk" | "tool_name">,
): boolean {
  if (req.risk === "high") return false;
  const base = stripToolPrefix(req.tool_name).toLowerCase();
  if (ALWAYS_CONFIRM_DESTRUCTIVE_TOOLS.has(base)) return false;
  return true;
}

export function computeApprovalRemainingSecs(
  createdTs: number,
  timeoutSecs: number,
  nowMs: number = Date.now(),
): number {
  const createdMs = normalizeEpochMs(createdTs);
  const safeTimeout = Number.isFinite(timeoutSecs) && timeoutSecs > 0 ? timeoutSecs : 0;
  if (createdMs <= 0 || safeTimeout <= 0) return 0;
  const deadlineMs = createdMs + safeTimeout * 1000;
  const remainingMs = deadlineMs - nowMs;
  if (remainingMs <= 0) return 0;
  return Math.ceil(remainingMs / 1000);
}

export function shouldShowEmergencyStop(opts: {
  isStreaming: boolean;
  pendingApprovalsCount: number;
  tezaAccessLevel: TezaAccessLevel;
  systemAccessLevel: SystemAccessLevel;
}): boolean {
  return (
    opts.isStreaming ||
    opts.pendingApprovalsCount > 0 ||
    opts.tezaAccessLevel !== "isolated" ||
    opts.systemAccessLevel !== "safe"
  );
}

/**
 * Deduplicates tool activity events by `tool_call_id`, preserving first-seen order
 * while updating status and summary from newer events.
 */
export function mergeToolActivities(
  activities: readonly AiToolActivityEvent[] | null | undefined,
): AiToolActivityEvent[] {
  if (!Array.isArray(activities) || activities.length === 0) return [];
  const indexById = new Map<string, number>();
  const merged: AiToolActivityEvent[] = [];

  for (const ev of activities) {
    if (!ev || !ev.tool_call_id) continue;
    const existingIdx = indexById.get(ev.tool_call_id);
    if (existingIdx === undefined) {
      indexById.set(ev.tool_call_id, merged.length);
      merged.push({ ...ev });
    } else {
      const prev = merged[existingIdx];
      merged[existingIdx] = {
        ...prev,
        ...ev,
        tool_name: ev.tool_name || prev.tool_name,
        summary: ev.summary !== undefined && ev.summary !== "" ? ev.summary : prev.summary,
      };
    }
  }
  return merged;
}

function readStringField(
  details: Record<string, unknown>,
  keys: readonly string[],
): string | undefined {
  for (const k of keys) {
    const val = details[k];
    if (typeof val === "string" && val.trim().length > 0) {
      return val;
    }
    if (typeof val === "number" && Number.isFinite(val)) {
      return String(val);
    }
  }
  return undefined;
}

function formatDetailValue(val: unknown): string {
  if (val === null || val === undefined) return "";
  if (typeof val === "string") return val.length > 400 ? `${val.slice(0, 397)}...` : val;
  if (typeof val === "number" || typeof val === "boolean") return String(val);
  try {
    const serialized = JSON.stringify(val);
    return serialized.length > 400 ? `${serialized.slice(0, 397)}...` : serialized;
  } catch {
    return String(val);
  }
}

export function extractApprovalDetailView(
  req: Pick<AiToolApprovalRequest, "tool_name" | "details">,
): ApprovalDetailView {
  const details =
    req.details && typeof req.details === "object" && !Array.isArray(req.details)
      ? req.details
      : {};
  const baseTool = stripToolPrefix(req.tool_name).toLowerCase();

  if (baseTool === "win_run_powershell" || baseTool === "shell") {
    const command = readStringField(details, ["command", "script", "ps_command", "code"]) ?? "";
    const cwd = readStringField(details, ["cwd", "working_dir", "workdir", "dir"]);
    const rawTimeout = details.timeout_secs ?? details.timeout;
    const timeoutSecs =
      typeof rawTimeout === "number" && Number.isFinite(rawTimeout) && rawTimeout > 0
        ? Math.round(rawTimeout)
        : undefined;
    return {
      kind: "powershell",
      command,
      cwd,
      timeoutSecs,
    };
  }

  if (baseTool === "win_write_file" || baseTool === "file_write") {
    const path = readStringField(details, ["path", "file_path", "target_path"]) ?? "";
    const rawContent = readStringField(details, ["content", "content_preview", "preview", "text"]);
    const maxPreviewLen = 1200;
    const truncated = Boolean(rawContent && rawContent.length > maxPreviewLen);
    const contentPreview = rawContent
      ? truncated
        ? `${rawContent.slice(0, maxPreviewLen)}…`
        : rawContent
      : undefined;
    let mode = readStringField(details, ["mode"]);
    if (!mode && typeof details.append === "boolean") {
      mode = details.append ? "append" : "overwrite";
    }
    return {
      kind: "write_file",
      path,
      mode,
      contentPreview,
      truncated,
    };
  }

  if (baseTool === "win_open_path") {
    const path = readStringField(details, ["path", "file_path", "target_path"]) ?? "";
    return {
      kind: "open_path",
      path,
    };
  }

  if (
    baseTool === "send_message" ||
    baseTool === "send_attachment" ||
    baseTool === "create_schedule"
  ) {
    const recipient = readStringField(details, [
      "recipient_name",
      "thread_title",
      "recipient",
      "target",
      "to",
      "thread_id",
    ]);
    const text = readStringField(details, ["text", "message", "caption", "content", "body"]);
    const filePath = readStringField(details, ["file_path", "attachment_path", "path"]);
    const scheduleTime = readStringField(details, [
      "send_at_local",
      "send_at",
      "schedule_time",
      "scheduled_at",
      "send_at_ts",
    ]);
    const recurrence = readStringField(details, ["recurrence"]);
    const replyTo = readStringField(details, ["reply_to_msg_id", "reply_to"]);

    if (recipient || text || filePath || scheduleTime) {
      return {
        kind: "message",
        recipient,
        text,
        filePath,
        scheduleTime,
        recurrence,
        replyTo,
      };
    }
  }

  const entries: { key: string; value: string }[] = [];
  for (const [key, val] of Object.entries(details)) {
    const formatted = formatDetailValue(val).trim();
    if (!formatted) continue;
    entries.push({ key, value: formatted });
  }
  return { kind: "generic", entries };
}
