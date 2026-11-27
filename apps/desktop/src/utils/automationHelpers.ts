import type { ThreadKind } from "../types.ts";

// ==================== Item #1: Optional AI Assistant DTOs ====================

export type AiBackendKind = "zeroclaw" | "cloud_api";

export const DEFAULT_ZEROCLAW_PATH = "zeroclaw";
export const DEFAULT_ZEROCLAW_MODEL = "default";
export const DEFAULT_CLOUD_API_BASE_URL = "https://api.groq.com/openai/v1";
export const DEFAULT_CLOUD_API_MODEL = "llama-3.3-70b-versatile";

export type AiCloudProviderId = "groq" | "openrouter" | "xai" | "custom";

export interface AiCloudModelPreset {
  id: string;
  label: string;
  isFree?: boolean;
}

export interface AiCloudProviderPreset {
  id: Exclude<AiCloudProviderId, "custom">;
  name: string;
  baseUrl: string;
  defaultModel: string;
  keyPlaceholder: string;
  hint: string;
  recommendedModels: readonly AiCloudModelPreset[];
}

export const AI_CLOUD_PROVIDERS: readonly AiCloudProviderPreset[] = [
  {
    id: "groq",
    name: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    defaultModel: "llama-3.3-70b-versatile",
    keyPlaceholder: "gsk_...",
    hint: "Tốc độ phản hồi rất nhanh, có gói dùng miễn phí tại console.groq.com.",
    recommendedModels: [
      { id: "llama-3.3-70b-versatile", label: "Llama 3.3 70B (Khuyên dùng)", isFree: true },
      { id: "llama-3.1-8b-instant", label: "Llama 3.1 8B (Nhanh)", isFree: true },
      { id: "qwen/qwen3-32b", label: "Qwen3 32B", isFree: true },
      { id: "meta-llama/llama-4-scout-17b-16e-instruct", label: "Llama 4 Scout", isFree: true },
    ],
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    defaultModel: "meta-llama/llama-3.3-70b-instruct:free",
    keyPlaceholder: "sk-or-v1-...",
    hint: "Nhiều mẫu AI miễn phí và trả phí trên cùng một khóa tại openrouter.ai.",
    recommendedModels: [
      {
        id: "meta-llama/llama-3.3-70b-instruct:free",
        label: "Llama 3.3 70B",
        isFree: true,
      },
      {
        id: "deepseek/deepseek-r1-0528:free",
        label: "DeepSeek R1",
        isFree: true,
      },
      {
        id: "qwen/qwen3-235b-a22b:free",
        label: "Qwen3 235B",
        isFree: true,
      },
      {
        id: "mistralai/mistral-small-3.1-24b-instruct:free",
        label: "Mistral Small 3.1",
        isFree: true,
      },
    ],
  },
  {
    id: "xai",
    name: "xAI Grok",
    baseUrl: "https://api.x.ai/v1",
    defaultModel: "grok-3-mini",
    keyPlaceholder: "xai-...",
    hint: "Dùng trực tiếp các mẫu Grok chính thức từ console.x.ai.",
    recommendedModels: [
      { id: "grok-3-mini", label: "Grok 3 Mini (Khuyên dùng)" },
      { id: "grok-3", label: "Grok 3" },
      { id: "grok-3-fast", label: "Grok 3 Fast" },
    ],
  },
] as const;

export interface AiModelInfoDto {
  id: string;
  is_free: boolean;
  owned_by?: string | null;
}

export interface AiConfigDto {
  enabled: boolean;
  backend: AiBackendKind;
  model: string;
  api_base_url: string;
  has_api_key: boolean;
  api_key_preview: string;
  zeroclaw_path: string;
  zeroclaw_gateway_url: string;
  has_zeroclaw_token: boolean;
  zeroclaw_token_preview: string;
  zeroclaw_safe_tools?: boolean;
  system_prompt: string;
  summary_language: string;
  max_summary_messages: number;
}

export interface AiConfigUpdateDto {
  enabled?: boolean;
  backend?: AiBackendKind;
  model?: string;
  api_base_url?: string;
  api_key?: string;
  clear_api_key?: boolean;
  zeroclaw_path?: string;
  zeroclaw_gateway_url?: string;
  zeroclaw_token?: string;
  clear_zeroclaw_token?: boolean;
  zeroclaw_safe_tools?: boolean;
  system_prompt?: string;
  summary_language?: string;
  max_summary_messages?: number;
}

export interface ZeroClawInstallProgressDto {
  stage: "checking" | "downloading" | "verifying" | "extracting" | "ready" | string;
  downloaded_bytes: number;
  total_bytes: number | null;
  tag: string | null;
}

export interface ZeroClawInstallResultDto {
  installed_path: string;
  tag: string;
  version: string | null;
  sha256_verified: boolean;
}

export interface AiInstallZeroClawResponseDto {
  install: ZeroClawInstallResultDto;
  config: AiConfigDto;
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
  mode?: string | null;
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

export interface AiSessionDto {
  id: string;
  title: string;
  created_ts: number;
  updated_ts: number;
  message_count: number;
  last_preview: string;
}

export interface AiCommandResultDto {
  command: string;
  title: string;
  output: string;
  ok: boolean;
}

export type AiSlashCommandAction =
  | { kind: "new" }
  | { kind: "clear" }
  | { kind: "model" }
  | { kind: "help" }
  | { kind: "zeroclaw"; command: "memory" | "memory_clear" | "skills" | "status" };

export interface AiSlashCommandDef {
  cmd: string;
  label: string;
  description: string;
  zeroclawOnly?: boolean;
  action: AiSlashCommandAction;
}

export const AI_SLASH_COMMANDS: readonly AiSlashCommandDef[] = [
  {
    cmd: "/new",
    label: "Phiên mới",
    description: "Tạo cuộc trò chuyện AI mới",
    action: { kind: "new" },
  },
  {
    cmd: "/clear",
    label: "Xóa phiên",
    description: "Xóa tin nhắn và bộ nhớ phiên hiện tại",
    action: { kind: "clear" },
  },
  {
    cmd: "/model",
    label: "Chọn mẫu AI",
    description: "Mở nhanh bảng chọn mẫu AI",
    action: { kind: "model" },
  },
  {
    cmd: "/memory",
    label: "Trí nhớ",
    description: "Xem trí nhớ ZeroClaw đang lưu cho phiên này",
    zeroclawOnly: true,
    action: { kind: "zeroclaw", command: "memory" },
  },
  {
    cmd: "/memory clear",
    label: "Xóa trí nhớ",
    description: "Xóa trí nhớ ZeroClaw của phiên này",
    zeroclawOnly: true,
    action: { kind: "zeroclaw", command: "memory_clear" },
  },
  {
    cmd: "/skills",
    label: "Kỹ năng",
    description: "Xem danh sách kỹ năng ZeroClaw đã cài",
    zeroclawOnly: true,
    action: { kind: "zeroclaw", command: "skills" },
  },
  {
    cmd: "/status",
    label: "Trạng thái",
    description: "Xem trạng thái và cấu hình ZeroClaw",
    zeroclawOnly: true,
    action: { kind: "zeroclaw", command: "status" },
  },
  {
    cmd: "/help",
    label: "Trợ giúp",
    description: "Xem danh sách lệnh nhanh",
    action: { kind: "help" },
  },
];

export function parseAiSlashCommand(input: string): AiSlashCommandAction | null {
  if (!input || typeof input !== "string") return null;
  if (input.includes("\n") || input.includes("\r")) return null;
  const trimmed = input.trim();
  if (!trimmed.startsWith("/")) return null;
  const normalized = trimmed.replace(/\s+/g, " ").toLowerCase();

  switch (normalized) {
    case "/new":
      return { kind: "new" };
    case "/clear":
    case "/reset":
      return { kind: "clear" };
    case "/model":
      return { kind: "model" };
    case "/help":
      return { kind: "help" };
    case "/memory":
    case "/memory list":
    case "/memory stats":
      return { kind: "zeroclaw", command: "memory" };
    case "/memory clear":
      return { kind: "zeroclaw", command: "memory_clear" };
    case "/skills":
      return { kind: "zeroclaw", command: "skills" };
    case "/status":
      return { kind: "zeroclaw", command: "status" };
    default:
      return null;
  }
}

export function filterAiSlashCommands(
  draft: string,
  isZeroClaw = true,
): AiSlashCommandDef[] {
  if (!draft || typeof draft !== "string") return [];
  if (draft.includes("\n") || draft.includes("\r")) return [];
  if (!draft.startsWith("/")) return [];
  const query = draft.replace(/[ \t]+/g, " ").toLowerCase();

  return AI_SLASH_COMMANDS.filter((item) => {
    if (!isZeroClaw && item.zeroclawOnly) return false;
    return item.cmd.toLowerCase().startsWith(query);
  });
}

export function formatAiHelpMarkdown(isZeroClaw: boolean): string {
  const generalCommands = AI_SLASH_COMMANDS.filter((item) => !item.zeroclawOnly);
  const lines: string[] = [
    "### Lệnh nhanh Teza AI",
    "",
    ...generalCommands.map((item) => `- \`${item.cmd}\` — ${item.description}`),
  ];

  if (isZeroClaw) {
    const zcCommands = AI_SLASH_COMMANDS.filter((item) => item.zeroclawOnly);
    lines.push(
      "",
      "### Lệnh riêng cho ZeroClaw",
      "",
      ...zcCommands.map((item) => `- \`${item.cmd}\` — ${item.description}`),
    );
  } else {
    lines.push(
      "",
      "*Chuyển sang chế độ ZeroClaw trong phần chọn mẫu AI để dùng thêm các lệnh xem trí nhớ, kỹ năng và trạng thái.*",
    );
  }

  return lines.join("\n");
}

export function isValidAiModelName(raw: string): boolean {
  const clean = raw.trim();
  if (!clean || clean.length > 128) return false;
  if (clean.includes("..") || clean.startsWith("/") || clean.endsWith("/")) return false;
  return /^[A-Za-z0-9._:/-]+$/.test(clean);
}

export function formatInstallSizeMb(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 MB";
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function detectAiCloudProvider(baseUrl: string): AiCloudProviderId {
  const clean = (baseUrl || "").trim().toLowerCase().replace(/\/+$/, "");
  if (!clean) return "groq";
  try {
    const parsed = new URL(clean);
    const host = parsed.hostname.toLowerCase();
    if (host === "api.groq.com" || host.endsWith(".groq.com")) return "groq";
    if (host === "openrouter.ai" || host.endsWith(".openrouter.ai")) return "openrouter";
    if (host === "api.x.ai" || host === "x.ai") return "xai";
    return "custom";
  } catch {
    if (clean.includes("groq.com")) return "groq";
    if (clean.includes("openrouter.ai")) return "openrouter";
    if (clean.includes("api.x.ai")) return "xai";
    return "custom";
  }
}

export function getAiCloudProviderPreset(id: AiCloudProviderId): AiCloudProviderPreset | null {
  return AI_CLOUD_PROVIDERS.find((p) => p.id === id) ?? null;
}

export function getAiBackendBadgeLabel(
  config: Pick<AiConfigDto, "backend" | "api_base_url"> | null | undefined,
): string {
  if (!config || config.backend !== "cloud_api") {
    return "ZeroClaw";
  }
  const provider = detectAiCloudProvider(config.api_base_url);
  const preset = getAiCloudProviderPreset(provider);
  return preset ? preset.name : "Khóa API";
}

export function formatShortAiModelLabel(modelId: string | null | undefined): string {
  const clean = (modelId || "").trim();
  if (!clean || clean === DEFAULT_ZEROCLAW_MODEL) return "";
  const afterSlash = clean.includes("/") ? clean.slice(clean.lastIndexOf("/") + 1) : clean;
  return afterSlash.length > 28 ? `${afterSlash.slice(0, 27)}…` : afterSlash;
}

export function resolveQuickSwitchModelBackend(
  currentBackend: AiBackendKind | null | undefined,
  relayEnabled = false,
): AiBackendKind {
  return currentBackend === "zeroclaw" && !relayEnabled ? "zeroclaw" : "cloud_api";
}

export function needsZeroClawApiKeyLink(
  config:
    | Pick<AiConfigDto, "enabled" | "backend" | "has_api_key" | "zeroclaw_gateway_url">
    | null
    | undefined,
  relayEnabled = false,
): boolean {
  if (!config || !config.enabled || config.backend !== "zeroclaw") return false;
  if (relayEnabled) return false;
  if (config.has_api_key) return false;
  if ((config.zeroclaw_gateway_url || "").trim().length > 0) return false;
  return true;
}

export function isValidHttpsApiBaseUrl(raw: string): boolean {
  const trimmed = (raw || "").trim();
  if (!trimmed || trimmed.length > 256) return false;
  if (/[\x00-\x1f\x7f]/.test(trimmed)) return false;
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "https:") return false;
    if (parsed.username || parsed.password) return false;
    if (parsed.search || parsed.hash) return false;
    if (!parsed.hostname) return false;
    return true;
  } catch {
    return false;
  }
}

export function isPublicCloudModelsProvider(baseUrl: string): boolean {
  return detectAiCloudProvider(baseUrl) === "openrouter";
}

export function isSameCloudApiHost(urlA: string, urlB: string): boolean {
  try {
    const a = new URL((urlA || "").trim());
    const b = new URL((urlB || "").trim());
    return a.hostname.toLowerCase() === b.hostname.toLowerCase();
  } catch {
    return false;
  }
}

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
export const MIN_LOCAL_API_RATE_LIMIT = 1;
export const MAX_LOCAL_API_RATE_LIMIT = 600;
export const DEFAULT_LOCAL_API_AUDIT_LIMIT = 100;
export const MAX_LOCAL_API_AUDIT_LIMIT = 500;
export const LOCAL_API_AUDIT_LIMIT_STEPS = [100, 250, 500] as const;
export const MAX_SCHEDULE_HORIZON_MS = 5 * 365 * 24 * 60 * 60 * 1000;

export type BadgeTone = "neutral" | "info" | "success" | "warn" | "danger";

// ==================== Pure Helper Functions ====================

export function normalizeEpochMs(ts: number): number {
  if (!Number.isFinite(ts) || ts <= 0) return 0;
  return ts < 1_000_000_000_000 ? ts * 1000 : ts;
}

export function extractAttachmentBasename(filePath: string | null | undefined): string {
  const clean = (filePath || "").trim().replace(/[\\/]+$/, "");
  if (!clean) return "";
  const lastSep = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  return lastSep >= 0 ? clean.slice(lastSep + 1) : clean;
}

export function isImageAttachmentPath(filePath: string | null | undefined): boolean {
  const base = extractAttachmentBasename(filePath);
  return /\.(png|jpe?g|webp|gif|bmp)$/i.test(base);
}

/**
 * Formats an epoch timestamp for `<input type="datetime-local">` (`YYYY-MM-DDTHH:mm`).
 *
 * When `nowMsForFutureFloor` is provided and `ms` is in the future relative to it,
 * truncating seconds could turn a schedule due within <60s into a past timestamp;
 * in that case we round up to the next whole local minute so it stays valid.
 */
export function toLocalDatetimeInputValue(ms: number, nowMsForFutureFloor?: number): string {
  const safe = normalizeEpochMs(ms);
  const safeNow =
    nowMsForFutureFloor !== undefined && Number.isFinite(nowMsForFutureFloor)
      ? normalizeEpochMs(nowMsForFutureFloor)
      : null;
  const fallbackBase = safeNow && safeNow > 0 ? safeNow : Date.now();
  let d = new Date(safe > 0 ? safe : fallbackBase + 10 * 60_000);
  if (safeNow !== null && safeNow > 0 && safe > safeNow) {
    const truncatedMs = new Date(
      d.getFullYear(),
      d.getMonth(),
      d.getDate(),
      d.getHours(),
      d.getMinutes(),
      0,
      0,
    ).getTime();
    if (truncatedMs <= safeNow) {
      d = new Date(
        d.getFullYear(),
        d.getMonth(),
        d.getDate(),
        d.getHours(),
        d.getMinutes() + 1,
        0,
        0,
      );
    }
  }
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

export function validateLocalApiRateLimit(
  rateLimit: number,
): { valid: boolean; error: string | null } {
  if (
    !Number.isInteger(rateLimit) ||
    rateLimit < MIN_LOCAL_API_RATE_LIMIT ||
    rateLimit > MAX_LOCAL_API_RATE_LIMIT
  ) {
    return {
      valid: false,
      error: `Giới hạn tần suất (req/phút) phải là số nguyên từ ${MIN_LOCAL_API_RATE_LIMIT} đến ${MAX_LOCAL_API_RATE_LIMIT}.`,
    };
  }
  return { valid: true, error: null };
}

export function sanitizeLocalApiPort(port: number): number {
  return Number.isInteger(port) && port >= 1024 && port <= 65535
    ? port
    : DEFAULT_LOCAL_API_PORT;
}

function escapeCmdDoubleQuoted(value: string): string {
  return value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/, "$1$1");
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
    `REM Dùng Idempotency-Key duy nhất cho mỗi nội dung tin nhắn khác nhau`,
    `curl.exe -X POST "http://127.0.0.1:${safePort}/v1/messages/send" ^`,
    `  -H "Authorization: Bearer ${escapeCmdDoubleQuoted(safeToken)}" ^`,
    `  -H "Content-Type: application/json; charset=utf-8" ^`,
    `  -H "Idempotency-Key: teza-job-001" ^`,
    `  -d "${escapeCmdDoubleQuoted(payload)}"`,
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
    `# Dùng Idempotency-Key duy nhất cho mỗi nội dung tin nhắn khác nhau`,
    `$headers = @{`,
    `  Authorization = 'Bearer ${escapePowerShellSingleQuoted(safeToken)}'`,
    `  'Idempotency-Key' = 'teza-job-001'`,
    `}`,
    `$body = @{`,
    `  thread_id = '${escapePowerShellSingleQuoted(safeThreadId)}'`,
    `  text = 'Xin chào từ Teza Local API!'`,
    `} | ConvertTo-Json -Compress`,
    `$bodyBytes = [Text.Encoding]::UTF8.GetBytes($body)`,
    `Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:${safePort}/v1/messages/send' -Headers $headers -ContentType 'application/json; charset=utf-8' -Body $bodyBytes`,
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
    `        "Authorization": "Bearer " + ${JSON.stringify(safeToken)},`,
    `        "Content-Type": "application/json",`,
    `        # Dùng Idempotency-Key duy nhất cho mỗi nội dung tin nhắn khác nhau`,
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
  if (
    norm === "idempotency_conflict" ||
    norm === "idempotency_in_flight" ||
    statusCode === 409
  ) {
    return "Trùng Idempotency-Key (khác nội dung hoặc đang xử lý)";
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
  if (norm === "not_authenticated" || statusCode === 503) {
    return "Chưa đăng nhập tài khoản";
  }
  if (statusCode === 408) {
    return "Hết thời gian chờ kết nối";
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
