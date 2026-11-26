import type { ThreadKind } from "../types.ts";

// ==================== Item #1: Optional AI Assistant DTOs ====================

export type AiBackendKind = "gemini_api" | "gemini_cli";

export interface AiConfigDto {
  enabled: boolean;
  backend: AiBackendKind;
  gemini_model: string;
  cli_path: string;
  cli_model: string;
  system_prompt: string;
  summary_language: string;
  max_summary_messages: number;
  has_api_key: boolean;
  api_key_preview: string;
  has_translate_fallback_key?: boolean;
  translate_fallback_preview?: string;
  uses_translate_fallback_key?: boolean;
  has_effective_api_key?: boolean;
}

export interface AiConfigUpdateDto {
  enabled?: boolean;
  backend?: AiBackendKind;
  gemini_model?: string;
  api_key?: string;
  clear_api_key?: boolean;
  cli_path?: string;
  cli_model?: string;
  system_prompt?: string;
  summary_language?: string;
  max_summary_messages?: number;
}

export interface AiStoredMessageDto {
  id: string;
  session_id: string;
  role: "user" | "model";
  content: string;
  model: string;
  backend: string;
  ts: number;
}

export interface AiCliProbeDto {
  available: boolean;
  resolved_path: string | null;
  version: string | null;
  error: string | null;
}

export interface AiSummaryDto {
  summary: string;
  bullet_points: string[];
  action_items: string[];
  message_count: number;
  backend: string;
  model: string;
}

export const AI_GEMINI_MODELS = [
  {
    id: "gemini-2.5-flash",
    label: "Gemini 2.5 Flash",
    hint: "Nhanh · Cân bằng (Khuyên dùng)",
  },
  {
    id: "gemini-2.5-pro",
    label: "Gemini 2.5 Pro",
    hint: "Thông minh · Suy luận sâu",
  },
  {
    id: "gemini-2.0-flash",
    label: "Gemini 2.0 Flash",
    hint: "Nhẹ · Phản hồi tức thì",
  },
] as const;

export const AI_SUMMARY_LANGUAGES = [
  { code: "vi", label: "Tiếng Việt" },
  { code: "en", label: "English" },
  { code: "ko", label: "한국어" },
  { code: "ja", label: "日本語" },
  { code: "zh", label: "中文" },
] as const;

export const DEFAULT_AI_SYSTEM_PROMPT =
  "Bạn là Trợ lý AI tích hợp trong Teza. Hãy trả lời ngắn gọn, rõ ràng, lịch sự và hữu ích bằng ngôn ngữ của người dùng.";

// ==================== Item #2: Scheduled Messages DTOs ====================

export type ScheduleRecurrence = "once" | "daily" | "weekly" | "monthly";

export type ScheduleMisfirePolicy = "skip" | "send_on_resume" | "ask";

export type ScheduleStatus =
  | "pending"
  | "sending"
  | "sent"
  | "failed"
  | "cancelled"
  | "missed";

export interface ScheduledMessageDto {
  id: string;
  thread_id: string;
  thread_title: string;
  thread_kind: ThreadKind;
  text: string;
  attachment_path: string;
  send_at_ts: number;
  recurrence: ScheduleRecurrence;
  misfire_policy: ScheduleMisfirePolicy;
  misfire_grace_secs: number;
  status: ScheduleStatus;
  retry_count: number;
  last_error: string;
  created_at: number;
  updated_at: number;
  sent_at_ts: number;
}

export interface CreateScheduledMessageInput {
  thread_id: string;
  thread_title: string;
  thread_kind: ThreadKind;
  text: string;
  attachment_path?: string;
  send_at_ts: number;
  recurrence: ScheduleRecurrence;
  misfire_policy: ScheduleMisfirePolicy;
  misfire_grace_secs?: number;
}

export interface UpdateScheduledMessageInput {
  thread_title?: string;
  thread_kind?: ThreadKind;
  text?: string;
  attachment_path?: string;
  send_at_ts?: number;
  recurrence?: ScheduleRecurrence;
  misfire_policy?: ScheduleMisfirePolicy;
  misfire_grace_secs?: number;
}

// ==================== Item #3: Local Automation API DTOs ====================

export type ApiClientScope = "send" | "schedule" | "read_threads";

export interface LocalApiConfigDto {
  enabled: boolean;
  port: number;
  min_send_interval_ms: number;
}

export interface LocalApiStatusDto {
  enabled: boolean;
  running: boolean;
  bind_addr: string;
  port: number;
  active_clients: number;
  last_error?: string | null;
}

export interface ApiClientDto {
  id: string;
  name: string;
  token_prefix: string;
  scopes: string[];
  allowed_thread_ids: string[];
  rate_limit_per_min: number;
  enabled: boolean;
  created_at: number;
  last_used_at: number;
}

export interface CreatedApiClientDto {
  client: ApiClientDto;
  raw_token: string;
}

export interface ApiAuditLogDto {
  id: number;
  client_id: string;
  client_name: string;
  method: string;
  path: string;
  target_thread_id: string;
  status_code: number;
  outcome: string;
  detail: string;
  created_at: number;
}

export interface CreateApiClientInput {
  name: string;
  scopes: ApiClientScope[];
  allowed_thread_ids: string[];
  rate_limit_per_min: number;
}

export interface UpdateApiClientInput {
  enabled?: boolean;
  scopes?: ApiClientScope[];
  allowed_thread_ids?: string[];
  rate_limit_per_min?: number;
}

export const DEFAULT_LOCAL_API_PORT = 17820;
export const DEFAULT_LOCAL_API_MIN_INTERVAL_MS = 1500;
export const MIN_LOCAL_API_INTERVAL_MS = 200;
export const MAX_LOCAL_API_INTERVAL_MS = 60_000;
export const MAX_SCHEDULE_HORIZON_MS = 5 * 365 * 24 * 60 * 60 * 1000;

export type BadgeTone = "neutral" | "info" | "success" | "warn" | "danger";

// ==================== Pure Helper Functions ====================

export function normalizeEpochMs(ts: number): number {
  if (!Number.isFinite(ts) || ts <= 0) return 0;
  return ts < 1_000_000_000_000 ? ts * 1000 : ts;
}

export function toLocalDatetimeInputValue(ms: number): string {
  const safe = normalizeEpochMs(ms);
  const d = new Date(safe > 0 ? safe : Date.now() + 10 * 60_000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function formatLocalDateTime(ts: number, includeSeconds = false): string {
  const safe = normalizeEpochMs(ts);
  if (safe <= 0) return "—";
  try {
    return new Date(safe).toLocaleString("vi-VN", {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      ...(includeSeconds ? { second: "2-digit" } : {}),
    });
  } catch {
    return new Date(safe).toISOString();
  }
}

export function formatThreadKindLabel(kind: ThreadKind | string): string {
  switch (kind) {
    case "direct":
      return "Cá nhân";
    case "group":
      return "Nhóm";
    case "cloud":
      return "Cloud";
    default:
      return "Cá nhân";
  }
}

export function formatScheduleRecurrence(rec: ScheduleRecurrence | string): string {
  switch (rec) {
    case "once":
      return "Một lần";
    case "daily":
      return "Hằng ngày";
    case "weekly":
      return "Hằng tuần";
    case "monthly":
      return "Hằng tháng";
    default:
      return "Một lần";
  }
}

export function formatScheduleMisfirePolicy(policy: ScheduleMisfirePolicy | string): string {
  switch (policy) {
    case "send_on_resume":
      return "Gửi ngay khi mở lại";
    case "skip":
      return "Bỏ qua nếu trễ";
    case "ask":
      return "Hỏi xác nhận";
    default:
      return "Gửi ngay khi mở lại";
  }
}

export function formatScheduleStatus(status: ScheduleStatus | string): string {
  switch (status) {
    case "pending":
      return "Chờ gửi";
    case "sending":
      return "Đang gửi";
    case "sent":
      return "Đã gửi";
    case "failed":
      return "Gửi lỗi";
    case "cancelled":
      return "Đã hủy";
    case "missed":
      return "Bỏ lỡ";
    default:
      return status ? String(status) : "Không rõ";
  }
}

export function getScheduleStatusTone(status: ScheduleStatus | string): BadgeTone {
  switch (status) {
    case "pending":
      return "info";
    case "sending":
      return "warn";
    case "sent":
      return "success";
    case "failed":
      return "danger";
    case "missed":
      return "warn";
    case "cancelled":
    default:
      return "neutral";
  }
}

export function validateScheduleTimestamp(
  sendAtMs: number,
  nowMs: number = Date.now(),
): { valid: boolean; error: string | null } {
  const safeNow = Number.isFinite(nowMs) && nowMs > 0 ? nowMs : Date.now();
  if (!Number.isFinite(sendAtMs) || sendAtMs <= 0) {
    return { valid: false, error: "Thời gian gửi không hợp lệ." };
  }
  if (sendAtMs <= safeNow) {
    return { valid: false, error: "Thời gian hẹn gửi phải nằm trong tương lai." };
  }
  if (sendAtMs - safeNow > MAX_SCHEDULE_HORIZON_MS) {
    return {
      valid: false,
      error: "Thời gian hẹn gửi vượt quá giới hạn tối đa (5 năm).",
    };
  }
  return { valid: true, error: null };
}

export function parseAllowedThreadIdsInput(raw: string): string[] {
  if (!raw || typeof raw !== "string") return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of raw.split(/[\s,;]+/)) {
    const clean = part.trim();
    if (!clean || seen.has(clean)) continue;
    seen.add(clean);
    out.push(clean);
  }
  return out;
}

export function clampSummaryMessageCount(value: number): number {
  if (!Number.isFinite(value)) return 40;
  return Math.min(120, Math.max(5, Math.round(value)));
}

export function validateLocalApiPort(port: number): { valid: boolean; error: string | null } {
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    return {
      valid: false,
      error: "Cổng Local API phải là số nguyên từ 1024 đến 65535.",
    };
  }
  return { valid: true, error: null };
}

export function validateLocalApiMinIntervalMs(
  intervalMs: number,
): { valid: boolean; error: string | null } {
  if (
    !Number.isFinite(intervalMs) ||
    intervalMs < MIN_LOCAL_API_INTERVAL_MS ||
    intervalMs > MAX_LOCAL_API_INTERVAL_MS
  ) {
    return {
      valid: false,
      error: `Khoảng cách gửi tối thiểu phải nằm trong khoảng ${MIN_LOCAL_API_INTERVAL_MS} đến ${MAX_LOCAL_API_INTERVAL_MS} ms.`,
    };
  }
  return { valid: true, error: null };
}

export function sanitizeLocalApiPort(port: number): number {
  return Number.isInteger(port) && port >= 1024 && port <= 65535
    ? port
    : DEFAULT_LOCAL_API_PORT;
}

function escapeSingleQuotedShell(value: string): string {
  return value.replace(/'/g, "'\\''");
}

function escapePowerShellSingleQuoted(value: string): string {
  return value.replace(/'/g, "''");
}

export function buildLocalApiCurlExample(
  port: number,
  tokenPlaceholder = "teza_YOUR_TOKEN",
  threadId = "123456789",
): string {
  const safePort = sanitizeLocalApiPort(port);
  const safeToken = (tokenPlaceholder.trim() || "teza_YOUR_TOKEN").replace(/[\r\n]/g, "");
  const safeThreadId = (threadId.trim() || "123456789").replace(/[\r\n]/g, "");
  const payload = JSON.stringify({
    thread_id: safeThreadId,
    text: "Xin chào từ Teza Local API!",
  });
  return [
    `curl -X POST http://127.0.0.1:${safePort}/v1/messages/send \\`,
    `  -H 'Authorization: Bearer ${escapeSingleQuotedShell(safeToken)}' \\`,
    `  -H 'Content-Type: application/json' \\`,
    `  -H 'Idempotency-Key: teza-job-001' \\`,
    `  -d '${escapeSingleQuotedShell(payload)}'`,
  ].join("\n");
}

export function buildLocalApiPowerShellExample(
  port: number,
  tokenPlaceholder = "teza_YOUR_TOKEN",
  threadId = "123456789",
): string {
  const safePort = sanitizeLocalApiPort(port);
  const safeToken = (tokenPlaceholder.trim() || "teza_YOUR_TOKEN").replace(/[\r\n]/g, "");
  const safeThreadId = (threadId.trim() || "123456789").replace(/[\r\n]/g, "");
  return [
    `$headers = @{`,
    `  Authorization = 'Bearer ${escapePowerShellSingleQuoted(safeToken)}'`,
    `  'Idempotency-Key' = 'teza-job-001'`,
    `}`,
    `$body = @{`,
    `  thread_id = '${escapePowerShellSingleQuoted(safeThreadId)}'`,
    `  text = 'Xin chào từ Teza Local API!'`,
    `} | ConvertTo-Json -Compress`,
    `Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:${safePort}/v1/messages/send' -Headers $headers -ContentType 'application/json; charset=utf-8' -Body $body`,
  ].join("\n");
}

export function buildLocalApiPythonExample(
  port: number,
  tokenPlaceholder = "teza_YOUR_TOKEN",
  threadId = "123456789",
): string {
  const safePort = sanitizeLocalApiPort(port);
  const safeToken = (tokenPlaceholder.trim() || "teza_YOUR_TOKEN").replace(/[\r\n]/g, "");
  const safeThreadId = (threadId.trim() || "123456789").replace(/[\r\n]/g, "");
  return [
    `import json`,
    `import urllib.request`,
    ``,
    `url = "http://127.0.0.1:${safePort}/v1/messages/send"`,
    `payload = json.dumps({`,
    `    "thread_id": ${JSON.stringify(safeThreadId)},`,
    `    "text": "Xin chào từ Teza Local API!",`,
    `}).encode("utf-8")`,
    `req = urllib.request.Request(`,
    `    url,`,
    `    data=payload,`,
    `    method="POST",`,
    `    headers={`,
    `        "Authorization": f"Bearer {${JSON.stringify(safeToken)}}",`,
    `        "Content-Type": "application/json",`,
    `        "Idempotency-Key": "teza-job-001",`,
    `    },`,
    `)`,
    `with urllib.request.urlopen(req, timeout=10) as resp:`,
    `    print(resp.status, resp.read().decode("utf-8"))`,
  ].join("\n");
}

export function formatAuditOutcome(statusCode: number, outcome: string): string {
  const norm = (outcome || "").trim().toLowerCase();
  if (norm === "idempotent_replay" || norm === "idempotency_replay" || norm === "replayed") {
    return "Trả về kết quả đã lưu (Idempotent)";
  }
  if (norm === "forbidden_scope" || norm === "missing_scope") {
    return "Thiếu quyền (Scope)";
  }
  if (norm === "forbidden_thread" || norm === "thread_not_allowed") {
    return "Hội thoại ngoài danh sách cho phép";
  }
  if (norm === "forbidden_origin" || norm === "csrf_blocked") {
    return "Bị chặn (CSRF Origin)";
  }
  if (norm === "forbidden_host" || norm === "dns_rebinding") {
    return "Bị chặn (Host không hợp lệ)";
  }
  if (norm === "rate_limited" || statusCode === 429) {
    return "Vượt giới hạn tần suất";
  }
  if (norm === "unauthorized" || norm === "invalid_token" || statusCode === 401) {
    return "Token không hợp lệ hoặc bị khóa";
  }
  if (statusCode === 403) {
    return "Bị từ chối quyền truy cập";
  }
  if (statusCode === 400) {
    return "Dữ liệu yêu cầu không hợp lệ";
  }
  if (statusCode === 404) {
    return "Không tìm thấy endpoint";
  }
  if (statusCode >= 200 && statusCode < 300) {
    return "Thành công";
  }
  if (statusCode >= 500) {
    return "Lỗi xử lý cục bộ";
  }
  return outcome?.trim() || `HTTP ${statusCode}`;
}

export function getAuditStatusTone(statusCode: number): BadgeTone {
  if (statusCode >= 200 && statusCode < 300) return "success";
  if (statusCode === 429) return "warn";
  if (statusCode >= 400) return "danger";
  return "neutral";
}

// ==================== Safe Markdown Parser (No HTML Injection) ====================

export type InlineToken =
  | { kind: "text"; value: string }
  | { kind: "code"; value: string }
  | { kind: "bold"; value: string }
  | { kind: "italic"; value: string };

export type MarkdownBlock =
  | { kind: "heading"; level: 1 | 2 | 3; text: string }
  | { kind: "paragraph"; text: string }
  | { kind: "code"; lang: string; code: string }
  | { kind: "ul"; items: string[] }
  | { kind: "ol"; items: string[] };

export function parseInlineMarkdown(text: string): InlineToken[] {
  if (!text) return [];
  const tokens: InlineToken[] = [];
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\*[^*\n]+\*)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = pattern.exec(text)) !== null) {
    if (match.index > lastIndex) {
      tokens.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const seg = match[0];
    if (seg.startsWith("`") && seg.endsWith("`") && seg.length >= 2) {
      tokens.push({ kind: "code", value: seg.slice(1, -1) });
    } else if (seg.startsWith("**") && seg.endsWith("**") && seg.length >= 4) {
      tokens.push({ kind: "bold", value: seg.slice(2, -2) });
    } else if (seg.startsWith("*") && seg.endsWith("*") && seg.length >= 2) {
      tokens.push({ kind: "italic", value: seg.slice(1, -1) });
    } else {
      tokens.push({ kind: "text", value: seg });
    }
    lastIndex = match.index + seg.length;
  }

  if (lastIndex < text.length) {
    tokens.push({ kind: "text", value: text.slice(lastIndex) });
  }
  return tokens;
}

export function parseSimpleMarkdownBlocks(raw: string): MarkdownBlock[] {
  if (!raw) return [];
  const lines = raw.replace(/\r\n/g, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    // Fenced code block
    const fenceMatch = line.match(/^\s*```([A-Za-z0-9_+-]*)\s*$/);
    if (fenceMatch) {
      const lang = fenceMatch[1] || "";
      const codeLines: string[] = [];
      i += 1;
      while (i < lines.length && !/^\s*```\s*$/.test(lines[i])) {
        codeLines.push(lines[i]);
        i += 1;
      }
      if (i < lines.length) i += 1; // skip closing fence
      blocks.push({ kind: "code", lang, code: codeLines.join("\n") });
      continue;
    }

    if (!line.trim()) {
      i += 1;
      continue;
    }

    // Headings (#, ##, ###)
    const headingMatch = line.match(/^\s*(#{1,3})\s+(.+)$/);
    if (headingMatch) {
      const level = headingMatch[1].length as 1 | 2 | 3;
      blocks.push({ kind: "heading", level, text: headingMatch[2].trim() });
      i += 1;
      continue;
    }

    // Unordered list (- , * , • )
    if (/^\s*[-*•]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*•]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*•]\s+/, "").trim());
        i += 1;
      }
      blocks.push({ kind: "ul", items });
      continue;
    }

    // Ordered list (1. , 2. )
    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+[.)]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+[.)]\s+/, "").trim());
        i += 1;
      }
      blocks.push({ kind: "ol", items });
      continue;
    }

    // Paragraph (collect consecutive plain lines)
    const paraLines: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !/^\s*```/.test(lines[i]) &&
      !/^\s*#{1,3}\s+/.test(lines[i]) &&
      !/^\s*[-*•]\s+/.test(lines[i]) &&
      !/^\s*\d+[.)]\s+/.test(lines[i])
    ) {
      paraLines.push(lines[i]);
      i += 1;
    }
    blocks.push({ kind: "paragraph", text: paraLines.join("\n") });
  }

  return blocks;
}
