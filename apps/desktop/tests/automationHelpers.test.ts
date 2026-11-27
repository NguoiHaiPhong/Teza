import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  AI_CLOUD_PROVIDERS,
  AI_SLASH_COMMANDS,
  DEFAULT_AI_SYSTEM_PROMPT,
  DEFAULT_CLOUD_API_BASE_URL,
  DEFAULT_CLOUD_API_MODEL,
  detectAiCloudProvider,
  filterAiSlashCommands,
  formatAiHelpMarkdown,
  formatInstallSizeMb,
  formatShortAiModelLabel,
  getAiBackendBadgeLabel,
  getAiCloudProviderPreset,
  isPublicCloudModelsProvider,
  isSameCloudApiHost,
  isValidAiModelName,
  isValidHttpsApiBaseUrl,
  needsZeroClawApiKeyLink,
  parseAiSlashCommand,
  resolveQuickSwitchModelBackend,
  DEFAULT_LOCAL_API_AUDIT_LIMIT,
  DEFAULT_LOCAL_API_PORT,
  LOCAL_API_AUDIT_LIMIT_STEPS,
  MAX_LOCAL_API_AUDIT_LIMIT,
  MAX_LOCAL_API_INTERVAL_MS,
  MAX_LOCAL_API_RATE_LIMIT,
  MAX_SCHEDULE_HORIZON_MS,
  MIN_LOCAL_API_INTERVAL_MS,
  MIN_LOCAL_API_RATE_LIMIT,
  buildLocalApiCurlExample,
  buildLocalApiPowerShellExample,
  buildLocalApiPythonExample,
  clampSummaryMessageCount,
  extractAttachmentBasename,
  formatAuditOutcome,
  formatScheduleMisfirePolicy,
  formatScheduleRecurrence,
  formatScheduleStatus,
  formatThreadKindLabel,
  getAuditStatusTone,
  getScheduleStatusTone,
  isImageAttachmentPath,
  normalizeEpochMs,
  parseAllowedThreadIdsInput,
  parseInlineMarkdown,
  parseSimpleMarkdownBlocks,
  sanitizeLocalApiPort,
  toLocalDatetimeInputValue,
  validateLocalApiMinIntervalMs,
  validateLocalApiPort,
  validateLocalApiRateLimit,
  validateScheduleTimestamp,
} from "../src/utils/automationHelpers.ts";

// Construct forbidden legacy brand pattern dynamically so test source itself stays clean.
const FORBIDDEN_BRAND_REGEX = new RegExp(
  `${String.fromCharCode(122, 97, 108, 111)}|${String.fromCharCode(116, 101, 108, 101, 103, 114, 97, 109)}`,
  "i",
);

test("formatScheduleRecurrence maps all recurrence values and falls back cleanly", () => {
  assert.equal(formatScheduleRecurrence("once"), "Một lần");
  assert.equal(formatScheduleRecurrence("daily"), "Hằng ngày");
  assert.equal(formatScheduleRecurrence("weekly"), "Hằng tuần");
  assert.equal(formatScheduleRecurrence("monthly"), "Hằng tháng");
  assert.equal(formatScheduleRecurrence("unknown"), "Một lần");
});

test("formatScheduleMisfirePolicy and formatThreadKindLabel map all values", () => {
  assert.equal(formatScheduleMisfirePolicy("send_on_resume"), "Gửi ngay khi mở lại");
  assert.equal(formatScheduleMisfirePolicy("skip"), "Bỏ qua nếu trễ");
  assert.equal(formatScheduleMisfirePolicy("ask"), "Hỏi xác nhận");
  assert.equal(formatScheduleMisfirePolicy("other"), "Gửi ngay khi mở lại");

  assert.equal(formatThreadKindLabel("direct"), "Cá nhân");
  assert.equal(formatThreadKindLabel("group"), "Nhóm");
  assert.equal(formatThreadKindLabel("cloud"), "Cloud");
  assert.equal(formatThreadKindLabel("unknown"), "Cá nhân");
});

test("formatScheduleStatus and getScheduleStatusTone cover all schedule states", () => {
  assert.equal(formatScheduleStatus("pending"), "Chờ gửi");
  assert.equal(formatScheduleStatus("sending"), "Đang gửi");
  assert.equal(formatScheduleStatus("sent"), "Đã gửi");
  assert.equal(formatScheduleStatus("failed"), "Gửi lỗi");
  assert.equal(formatScheduleStatus("cancelled"), "Đã hủy");
  assert.equal(formatScheduleStatus("missed"), "Bỏ lỡ");
  assert.equal(formatScheduleStatus(""), "Không rõ");

  assert.equal(getScheduleStatusTone("pending"), "info");
  assert.equal(getScheduleStatusTone("sending"), "warn");
  assert.equal(getScheduleStatusTone("sent"), "success");
  assert.equal(getScheduleStatusTone("failed"), "danger");
  assert.equal(getScheduleStatusTone("missed"), "warn");
  assert.equal(getScheduleStatusTone("cancelled"), "neutral");
});

test("validateScheduleTimestamp rejects invalid, past, and far-future timestamps", () => {
  const now = 1_760_000_000_000;

  assert.deepEqual(validateScheduleTimestamp(now + 60_000, now), {
    valid: true,
    error: null,
  });
  assert.deepEqual(validateScheduleTimestamp(now + 10 * 60_000, now), {
    valid: true,
    error: null,
  });

  const atNow = validateScheduleTimestamp(now, now);
  assert.equal(atNow.valid, false);
  assert.ok(atNow.error);

  const past = validateScheduleTimestamp(now - 5_000, now);
  assert.equal(past.valid, false);
  assert.ok(past.error);

  assert.equal(validateScheduleTimestamp(0, now).valid, false);
  assert.equal(validateScheduleTimestamp(-100, now).valid, false);
  assert.equal(validateScheduleTimestamp(Number.NaN, now).valid, false);
  assert.equal(validateScheduleTimestamp(Number.POSITIVE_INFINITY, now).valid, false);

  const tooFar = validateScheduleTimestamp(now + MAX_SCHEDULE_HORIZON_MS + 1, now);
  assert.equal(tooFar.valid, false);
  assert.ok(tooFar.error);
});

test("parseAllowedThreadIdsInput splits commas/newlines/whitespace and deduplicates in order", () => {
  assert.deepEqual(parseAllowedThreadIdsInput(""), []);
  assert.deepEqual(parseAllowedThreadIdsInput("   \n\t , ;  "), []);
  assert.deepEqual(
    parseAllowedThreadIdsInput("1001, 2002\n3003\r\n1001;  4004  2002, grp_99"),
    ["1001", "2002", "3003", "4004", "grp_99"],
  );
  assert.deepEqual(parseAllowedThreadIdsInput("abc-1,abc-1,abc-2"), ["abc-1", "abc-2"]);
});

test("Local API port validation and summary count clamping enforce strict bounds", () => {
  assert.deepEqual(validateLocalApiPort(17820), { valid: true, error: null });
  assert.deepEqual(validateLocalApiPort(1024), { valid: true, error: null });
  assert.deepEqual(validateLocalApiPort(65535), { valid: true, error: null });
  assert.equal(validateLocalApiPort(80).valid, false);
  assert.equal(validateLocalApiPort(1023).valid, false);
  assert.equal(validateLocalApiPort(65536).valid, false);
  assert.equal(validateLocalApiPort(17820.5).valid, false);
  assert.equal(validateLocalApiPort(Number.NaN).valid, false);

  assert.equal(sanitizeLocalApiPort(18000), 18000);
  assert.equal(sanitizeLocalApiPort(80), DEFAULT_LOCAL_API_PORT);
  assert.equal(sanitizeLocalApiPort(99999), DEFAULT_LOCAL_API_PORT);

  assert.deepEqual(validateLocalApiMinIntervalMs(MIN_LOCAL_API_INTERVAL_MS), {
    valid: true,
    error: null,
  });
  assert.deepEqual(validateLocalApiMinIntervalMs(1500), { valid: true, error: null });
  assert.deepEqual(validateLocalApiMinIntervalMs(MAX_LOCAL_API_INTERVAL_MS), {
    valid: true,
    error: null,
  });
  assert.equal(validateLocalApiMinIntervalMs(0).valid, false);
  assert.equal(validateLocalApiMinIntervalMs(199).valid, false);
  assert.equal(validateLocalApiMinIntervalMs(60_001).valid, false);
  assert.equal(validateLocalApiMinIntervalMs(Number.NaN).valid, false);

  assert.equal(clampSummaryMessageCount(2), 5);
  assert.equal(clampSummaryMessageCount(40), 40);
  assert.equal(clampSummaryMessageCount(500), 120);
  assert.equal(clampSummaryMessageCount(Number.NaN), 40);
});

test("Local API code snippets bind 127.0.0.1, are Windows/Python-safe, and contain Teza branding only", () => {
  const curl = buildLocalApiCurlExample(17820, "teza_abc123", "thread_42");
  assert.match(curl, /^REM .*Idempotency-Key/m);
  assert.match(curl, /curl\.exe -X POST "http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send" \^/);
  assert.match(curl, /-H "Authorization: Bearer teza_abc123" \^/);
  assert.match(curl, /-H "Idempotency-Key: teza-job-001" \^/);
  assert.match(curl, /\\"thread_id\\":\\"thread_42\\"/);
  assert.match(curl, /Teza Local API/);
  assert.equal(curl.includes("'Authorization:"), false);
  assert.equal(curl.includes("\\\n"), false);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(curl), false);

  // Quotes/backslashes in token and thread_id are escaped safely for Windows argv parsing
  const curlEscaped = buildLocalApiCurlExample(17820, 'tok_"a"', 'thr_"99"');
  assert.match(curlEscaped, /Authorization: Bearer tok_\\"a\\"/);
  assert.match(curlEscaped, /\\"thread_id\\":\\"thr_\\\\\\"99\\\\\\"\\"/);

  // Invalid port falls back to default 17820
  const curlFallback = buildLocalApiCurlExample(22, "teza_x", "id_1");
  assert.match(curlFallback, /http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send/);

  const ps = buildLocalApiPowerShellExample(19001, "teza_tok'1", "id'2");
  assert.match(ps, /http:\/\/127\.0\.0\.1:19001\/v1\/messages\/send/);
  assert.match(ps, /Bearer teza_tok''1/);
  assert.match(ps, /thread_id = 'id''2'/);
  assert.match(ps, /\[Text\.Encoding\]::UTF8\.GetBytes\(\$body\)/);
  assert.match(ps, /Invoke-RestMethod.*-Body \$bodyBytes/);
  assert.match(ps, /Idempotency-Key/);
  assert.match(ps, /Teza Local API/);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(ps), false);

  const pyDefault = buildLocalApiPythonExample(17820);
  assert.match(pyDefault, /"Authorization": "Bearer " \+ "teza_YOUR_TOKEN",/);
  assert.equal(pyDefault.includes('f"Bearer {'), false);

  const py = buildLocalApiPythonExample(17820, 'teza_"secret"', 'thr_"99"');
  assert.match(py, /http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send/);
  assert.match(py, /urllib\.request/);
  assert.match(py, /"Authorization": "Bearer " \+ "teza_\\"secret\\"",/);
  assert.match(py, /"thr_\\"99\\""/);
  assert.match(py, /Idempotency-Key/);
  assert.match(py, /Teza Local API/);
  assert.equal(py.includes('f"Bearer {'), false);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(py), false);

  // Verify default prompt and labels also have zero forbidden brand leaks
  assert.equal(FORBIDDEN_BRAND_REGEX.test(DEFAULT_AI_SYSTEM_PROMPT), false);
});

test("Local API rate-limit validator and audit limit constants match backend bounds (1..=600, 100/250/500)", () => {
  assert.equal(MIN_LOCAL_API_RATE_LIMIT, 1);
  assert.equal(MAX_LOCAL_API_RATE_LIMIT, 600);
  assert.equal(DEFAULT_LOCAL_API_AUDIT_LIMIT, 100);
  assert.equal(MAX_LOCAL_API_AUDIT_LIMIT, 500);
  assert.deepEqual([...LOCAL_API_AUDIT_LIMIT_STEPS], [100, 250, 500]);

  assert.deepEqual(validateLocalApiRateLimit(1), { valid: true, error: null });
  assert.deepEqual(validateLocalApiRateLimit(20), { valid: true, error: null });
  assert.deepEqual(validateLocalApiRateLimit(120), { valid: true, error: null });
  assert.deepEqual(validateLocalApiRateLimit(600), { valid: true, error: null });
  assert.equal(validateLocalApiRateLimit(0).valid, false);
  assert.equal(validateLocalApiRateLimit(-5).valid, false);
  assert.equal(validateLocalApiRateLimit(601).valid, false);
  assert.equal(validateLocalApiRateLimit(12.5).valid, false);
  assert.equal(validateLocalApiRateLimit(Number.NaN).valid, false);
});

test("extractAttachmentBasename and isImageAttachmentPath handle Windows/POSIX paths safely", () => {
  assert.equal(extractAttachmentBasename("C:\\Users\\demo\\Pictures\\report.png"), "report.png");
  assert.equal(extractAttachmentBasename("/tmp/uploads/brief.pdf"), "brief.pdf");
  assert.equal(extractAttachmentBasename("C:\\folder\\file.docx\\"), "file.docx");
  assert.equal(extractAttachmentBasename("   "), "");
  assert.equal(extractAttachmentBasename(null), "");
  assert.equal(extractAttachmentBasename(undefined), "");

  assert.equal(isImageAttachmentPath("C:\\img\\photo.JPG"), true);
  assert.equal(isImageAttachmentPath("/var/a/sticker.webp"), true);
  assert.equal(isImageAttachmentPath("preview.gif"), true);
  assert.equal(isImageAttachmentPath("C:\\docs\\invoice.pdf"), false);
  assert.equal(isImageAttachmentPath(""), false);
});

test("formatAuditOutcome and getAuditStatusTone classify HTTP statuses and outcomes", () => {
  assert.equal(formatAuditOutcome(200, "ok"), "Thành công");
  assert.equal(formatAuditOutcome(200, "idempotent_replay"), "Trả về kết quả đã lưu (Idempotent)");
  assert.equal(
    formatAuditOutcome(409, "idempotency_conflict"),
    "Trùng Idempotency-Key (khác nội dung hoặc đang xử lý)",
  );
  assert.equal(
    formatAuditOutcome(409, "idempotency_in_flight"),
    "Trùng Idempotency-Key (khác nội dung hoặc đang xử lý)",
  );
  assert.equal(formatAuditOutcome(401, "unauthorized"), "Token không hợp lệ hoặc bị khóa");
  assert.equal(formatAuditOutcome(403, "forbidden_scope"), "Thiếu quyền (Scope)");
  assert.equal(formatAuditOutcome(403, "forbidden_thread"), "Hội thoại ngoài danh sách cho phép");
  assert.equal(formatAuditOutcome(403, "forbidden_origin"), "Bị chặn (CSRF Origin)");
  assert.equal(formatAuditOutcome(403, "forbidden_host"), "Bị chặn (Host không hợp lệ)");
  assert.equal(formatAuditOutcome(408, "timeout"), "Hết thời gian chờ kết nối");
  assert.equal(formatAuditOutcome(429, "rate_limited"), "Vượt giới hạn tần suất");
  assert.equal(formatAuditOutcome(400, "bad_request"), "Dữ liệu yêu cầu không hợp lệ");
  assert.equal(formatAuditOutcome(503, "not_authenticated"), "Chưa đăng nhập tài khoản");
  assert.equal(formatAuditOutcome(500, "internal_error"), "Lỗi xử lý cục bộ");

  assert.equal(getAuditStatusTone(200), "success");
  assert.equal(getAuditStatusTone(201), "success");
  assert.equal(getAuditStatusTone(429), "warn");
  assert.equal(getAuditStatusTone(403), "danger");
  assert.equal(getAuditStatusTone(500), "danger");
});

test("normalizeEpochMs and toLocalDatetimeInputValue handle seconds, ms, and <60s future minute floor", () => {
  assert.equal(normalizeEpochMs(0), 0);
  assert.equal(normalizeEpochMs(-5), 0);
  assert.equal(normalizeEpochMs(1_700_000_000), 1_700_000_000_000);
  assert.equal(normalizeEpochMs(1_700_000_000_000), 1_700_000_000_000);

  const formatted = toLocalDatetimeInputValue(1_700_000_000_000);
  assert.match(formatted, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);

  // Same-minute future timestamp (<59s ahead) must round up to next minute so datetime-local stays valid
  const nowSameMin = new Date(2026, 4, 10, 14, 30, 15, 0).getTime();
  const dueIn30s = new Date(2026, 4, 10, 14, 30, 45, 0).getTime();
  const roundedStr = toLocalDatetimeInputValue(dueIn30s, nowSameMin);
  assert.equal(roundedStr, "2026-05-10T14:31");
  assert.equal(validateScheduleTimestamp(new Date(roundedStr).getTime(), nowSameMin).valid, true);

  // Midnight rollover (23:59:10 -> 23:59:50 rounds up to next day 00:00)
  const nowMidnight = new Date(2026, 4, 10, 23, 59, 10, 0).getTime();
  const dueBeforeMidnight = new Date(2026, 4, 10, 23, 59, 50, 0).getTime();
  const rolloverStr = toLocalDatetimeInputValue(dueBeforeMidnight, nowMidnight);
  assert.equal(rolloverStr, "2026-05-11T00:00");
  assert.equal(validateScheduleTimestamp(new Date(rolloverStr).getTime(), nowMidnight).valid, true);

  // Already strictly future after truncation stays on exact minute
  const dueIn5m = new Date(2026, 4, 10, 14, 35, 45, 0).getTime();
  assert.equal(toLocalDatetimeInputValue(dueIn5m, nowSameMin), "2026-05-10T14:35");
});

test("parseSimpleMarkdownBlocks and parseInlineMarkdown safely parse markdown without HTML injection", () => {
  const sample = [
    "## Kế hoạch tuần",
    "- **Mục 1**: kiểm tra `config.json`",
    "- *Ghi chú* kèm <script>alert(1)</script>",
    "",
    "1. Bước một",
    "2. Bước hai",
    "",
    "```ts",
    "const x = 1 < 2;",
    "```",
  ].join("\n");

  const blocks = parseSimpleMarkdownBlocks(sample);
  assert.equal(blocks.length, 4);
  assert.deepEqual(blocks[0], { kind: "heading", level: 2, text: "Kế hoạch tuần" });
  assert.deepEqual(blocks[1], {
    kind: "ul",
    items: [
      "**Mục 1**: kiểm tra `config.json`",
      "*Ghi chú* kèm <script>alert(1)</script>",
    ],
  });
  assert.deepEqual(blocks[2], { kind: "ol", items: ["Bước một", "Bước hai"] });
  assert.deepEqual(blocks[3], { kind: "code", lang: "ts", code: "const x = 1 < 2;" });

  const inline = parseInlineMarkdown("**Đậm** và `mã` và *nghiêng* <img src=x>");
  assert.deepEqual(inline, [
    { kind: "bold", value: "Đậm" },
    { kind: "text", value: " và " },
    { kind: "code", value: "mã" },
    { kind: "text", value: " và " },
    { kind: "italic", value: "nghiêng" },
    { kind: "text", value: " <img src=x>" },
  ]);
});

test("ZeroClaw model validator and install size formatter work cleanly", () => {
  assert.equal(isValidAiModelName("default"), true);
  assert.equal(isValidAiModelName("qwen2.5:7b"), true);
  assert.equal(isValidAiModelName("meta-llama/llama-3.3-70b-instruct"), true);
  assert.equal(isValidAiModelName("../secret"), false);
  assert.equal(isValidAiModelName("bad model"), false);

  assert.equal(formatInstallSizeMb(0), "0 MB");
  assert.equal(formatInstallSizeMb(34_296_153), "32.7 MB");
});

test("AI Cloud API presets cover Groq, OpenRouter, and xAI Grok with curated chips and strict HTTPS validation", () => {
  assert.equal(DEFAULT_CLOUD_API_BASE_URL, "https://api.groq.com/openai/v1");
  assert.equal(DEFAULT_CLOUD_API_MODEL, "llama-3.3-70b-versatile");

  const ids = AI_CLOUD_PROVIDERS.map((p) => p.id);
  assert.deepEqual(ids, ["groq", "openrouter", "xai"]);

  const groq = getAiCloudProviderPreset("groq");
  assert.ok(groq);
  assert.equal(groq.baseUrl, "https://api.groq.com/openai/v1");
  assert.equal(groq.defaultModel, "llama-3.3-70b-versatile");
  assert.ok(groq.recommendedModels.length >= 3);

  const openrouter = getAiCloudProviderPreset("openrouter");
  assert.ok(openrouter);
  assert.equal(openrouter.baseUrl, "https://openrouter.ai/api/v1");
  assert.equal(openrouter.defaultModel, "meta-llama/llama-3.3-70b-instruct:free");
  assert.ok(openrouter.recommendedModels.every((m) => m.isFree));

  const xai = getAiCloudProviderPreset("xai");
  assert.ok(xai);
  assert.equal(xai.baseUrl, "https://api.x.ai/v1");
  assert.equal(xai.defaultModel, "grok-3-mini");
  assert.ok(xai.recommendedModels.length >= 2);

  assert.equal(getAiCloudProviderPreset("custom"), null);

  for (const p of AI_CLOUD_PROVIDERS) {
    assert.equal(isValidHttpsApiBaseUrl(p.baseUrl), true);
    assert.equal(isValidAiModelName(p.defaultModel), true);
    assert.equal(FORBIDDEN_BRAND_REGEX.test(p.hint), false);
    for (const m of p.recommendedModels) {
      assert.equal(isValidAiModelName(m.id), true);
    }
  }

  assert.equal(detectAiCloudProvider("https://api.groq.com/openai/v1/"), "groq");
  assert.equal(detectAiCloudProvider("https://openrouter.ai/api/v1"), "openrouter");
  assert.equal(detectAiCloudProvider("https://api.x.ai/v1"), "xai");
  assert.equal(detectAiCloudProvider("https://ai.example.com/v1"), "custom");

  assert.equal(
    getAiBackendBadgeLabel({ backend: "cloud_api", api_base_url: "https://api.groq.com/openai/v1" }),
    "Groq",
  );
  assert.equal(
    getAiBackendBadgeLabel({ backend: "cloud_api", api_base_url: "https://openrouter.ai/api/v1" }),
    "OpenRouter",
  );
  assert.equal(
    getAiBackendBadgeLabel({ backend: "cloud_api", api_base_url: "https://api.x.ai/v1" }),
    "xAI Grok",
  );
  assert.equal(
    getAiBackendBadgeLabel({ backend: "cloud_api", api_base_url: "https://custom.example.com/v1" }),
    "Khóa API",
  );
  assert.equal(
    getAiBackendBadgeLabel({ backend: "zeroclaw", api_base_url: "https://api.groq.com/openai/v1" }),
    "ZeroClaw",
  );
  assert.equal(getAiBackendBadgeLabel(null), "ZeroClaw");

  // Strict HTTPS validation checks
  assert.equal(isValidHttpsApiBaseUrl("https://api.groq.com/openai/v1"), true);
  assert.equal(isValidHttpsApiBaseUrl("http://api.groq.com/openai/v1"), false);
  assert.equal(isValidHttpsApiBaseUrl("http://127.0.0.1:8080/v1"), false);
  assert.equal(isValidHttpsApiBaseUrl("https://user:pass@api.groq.com/openai/v1"), false);
  assert.equal(isValidHttpsApiBaseUrl("https://api.groq.com/openai/v1?foo=bar"), false);
  assert.equal(isValidHttpsApiBaseUrl("https://api.groq.com/openai/v1#frag"), false);
  assert.equal(isValidHttpsApiBaseUrl(""), false);

  // Public keyless catalog detection & same-host guard
  assert.equal(isPublicCloudModelsProvider("https://openrouter.ai/api/v1"), true);
  assert.equal(isPublicCloudModelsProvider("https://api.groq.com/openai/v1"), false);
  assert.equal(isPublicCloudModelsProvider("https://api.x.ai/v1"), false);
  assert.equal(
    isSameCloudApiHost("https://api.groq.com/openai/v1", "https://API.GROQ.COM/openai/v1"),
    true,
  );
  assert.equal(
    isSameCloudApiHost("https://api.groq.com/openai/v1", "https://openrouter.ai/api/v1"),
    false,
  );
});

test("ZeroClaw short model label, quick-switch backend preservation, and API key banner helpers work cleanly", () => {
  assert.equal(formatShortAiModelLabel("default"), "");
  assert.equal(formatShortAiModelLabel(""), "");
  assert.equal(formatShortAiModelLabel(null), "");
  assert.equal(formatShortAiModelLabel("llama-3.3-70b-versatile"), "llama-3.3-70b-versatile");
  assert.equal(
    formatShortAiModelLabel("meta-llama/llama-3.3-70b-instruct:free"),
    "llama-3.3-70b-instruct:free",
  );
  assert.equal(
    formatShortAiModelLabel("org/very-long-model-identifier-exceeding-limit"),
    "very-long-model-identifier-…",
  );

  // Picking a model keeps current backend (ZeroClaw stays ZeroClaw unless blocked by corporate relay).
  assert.equal(resolveQuickSwitchModelBackend("zeroclaw", false), "zeroclaw");
  assert.equal(resolveQuickSwitchModelBackend("zeroclaw", true), "cloud_api");
  assert.equal(resolveQuickSwitchModelBackend("cloud_api", false), "cloud_api");
  assert.equal(resolveQuickSwitchModelBackend(undefined, false), "cloud_api");

  // Banner appears only when enabled + ZeroClaw + no API key + no local gateway + no relay block.
  assert.equal(
    needsZeroClawApiKeyLink(
      { enabled: true, backend: "zeroclaw", has_api_key: false, zeroclaw_gateway_url: "" },
      false,
    ),
    true,
  );
  assert.equal(
    needsZeroClawApiKeyLink(
      { enabled: true, backend: "zeroclaw", has_api_key: true, zeroclaw_gateway_url: "" },
      false,
    ),
    false,
  );
  assert.equal(
    needsZeroClawApiKeyLink(
      {
        enabled: true,
        backend: "zeroclaw",
        has_api_key: false,
        zeroclaw_gateway_url: "http://127.0.0.1:42617",
      },
      false,
    ),
    false,
  );
  assert.equal(
    needsZeroClawApiKeyLink(
      { enabled: true, backend: "zeroclaw", has_api_key: false, zeroclaw_gateway_url: "" },
      true,
    ),
    false,
  );
  assert.equal(
    needsZeroClawApiKeyLink(
      { enabled: false, backend: "zeroclaw", has_api_key: false, zeroclaw_gateway_url: "" },
      false,
    ),
    false,
  );
  assert.equal(
    needsZeroClawApiKeyLink(
      { enabled: true, backend: "cloud_api", has_api_key: false, zeroclaw_gateway_url: "" },
      false,
    ),
    false,
  );
  assert.equal(needsZeroClawApiKeyLink(null, false), false);
});

test("AI_SLASH_COMMANDS defines all expected commands with clean Vietnamese copy", () => {
  assert.deepEqual(
    AI_SLASH_COMMANDS.map((c) => c.cmd),
    ["/new", "/clear", "/model", "/memory", "/memory clear", "/skills", "/status", "/help"],
  );

  const zcOnlyCmds = AI_SLASH_COMMANDS.filter((c) => c.zeroclawOnly).map((c) => c.cmd);
  assert.deepEqual(zcOnlyCmds, ["/memory", "/memory clear", "/skills", "/status"]);

  for (const item of AI_SLASH_COMMANDS) {
    assert.ok(item.cmd.startsWith("/"));
    assert.ok(item.label.trim().length > 0);
    assert.ok(item.description.trim().length > 0);
    assert.equal(FORBIDDEN_BRAND_REGEX.test(item.label), false);
    assert.equal(FORBIDDEN_BRAND_REGEX.test(item.description), false);
  }
});

test("parseAiSlashCommand matches exact commands, aliases, and rejects normal/unknown text", () => {
  assert.deepEqual(parseAiSlashCommand("/new"), { kind: "new" });
  assert.deepEqual(parseAiSlashCommand("  /NEW  "), { kind: "new" });
  assert.deepEqual(parseAiSlashCommand("/clear"), { kind: "clear" });
  assert.deepEqual(parseAiSlashCommand("/reset"), { kind: "clear" });
  assert.deepEqual(parseAiSlashCommand(" /ReSeT "), { kind: "clear" });
  assert.deepEqual(parseAiSlashCommand("/model"), { kind: "model" });
  assert.deepEqual(parseAiSlashCommand("/help"), { kind: "help" });

  assert.deepEqual(parseAiSlashCommand("/memory"), { kind: "zeroclaw", command: "memory" });
  assert.deepEqual(parseAiSlashCommand("/memory stats"), { kind: "zeroclaw", command: "memory" });
  assert.deepEqual(parseAiSlashCommand("/memory list"), { kind: "zeroclaw", command: "memory" });
  assert.deepEqual(parseAiSlashCommand("  /MEMORY   STATS  "), {
    kind: "zeroclaw",
    command: "memory",
  });
  assert.deepEqual(parseAiSlashCommand("/memory\tlist"), { kind: "zeroclaw", command: "memory" });
  assert.deepEqual(parseAiSlashCommand("/memory clear"), {
    kind: "zeroclaw",
    command: "memory_clear",
  });
  assert.deepEqual(parseAiSlashCommand(" /Memory   Clear "), {
    kind: "zeroclaw",
    command: "memory_clear",
  });
  assert.deepEqual(parseAiSlashCommand("/skills"), { kind: "zeroclaw", command: "skills" });
  assert.deepEqual(parseAiSlashCommand("/status"), { kind: "zeroclaw", command: "status" });

  // Non-commands, unknown slash commands, extra arguments, and multiline input return null
  assert.equal(parseAiSlashCommand(""), null);
  assert.equal(parseAiSlashCommand("   "), null);
  assert.equal(parseAiSlashCommand("/"), null);
  assert.equal(parseAiSlashCommand("Xin chào /new"), null);
  assert.equal(parseAiSlashCommand("Hãy tóm tắt tin nhắn"), null);
  assert.equal(parseAiSlashCommand("/unknown"), null);
  assert.equal(parseAiSlashCommand("/new hello"), null);
  assert.equal(parseAiSlashCommand("/clear all"), null);
  assert.equal(parseAiSlashCommand("/memory purge"), null);
  assert.equal(parseAiSlashCommand("/skills extra"), null);
  assert.equal(parseAiSlashCommand("/new\nthêm dòng"), null);
  assert.equal(parseAiSlashCommand("/memory\nclear"), null);
});

test("filterAiSlashCommands filters composer autocomplete suggestions accurately", () => {
  assert.equal(filterAiSlashCommands("/").length, AI_SLASH_COMMANDS.length);
  assert.deepEqual(
    filterAiSlashCommands("/n").map((c) => c.cmd),
    ["/new"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/m").map((c) => c.cmd),
    ["/model", "/memory", "/memory clear"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/MEM").map((c) => c.cmd),
    ["/memory", "/memory clear"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/memory ").map((c) => c.cmd),
    ["/memory clear"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/memory   c").map((c) => c.cmd),
    ["/memory clear"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/s").map((c) => c.cmd),
    ["/skills", "/status"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/st").map((c) => c.cmd),
    ["/status"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/h").map((c) => c.cmd),
    ["/help"],
  );

  // Optional isZeroClaw=false filters out ZeroClaw-only commands
  assert.deepEqual(
    filterAiSlashCommands("/", false).map((c) => c.cmd),
    ["/new", "/clear", "/model", "/help"],
  );
  assert.deepEqual(
    filterAiSlashCommands("/m", false).map((c) => c.cmd),
    ["/model"],
  );

  // Non-slash, multiline, leading whitespace, or unmatched drafts return []
  assert.deepEqual(filterAiSlashCommands(""), []);
  assert.deepEqual(filterAiSlashCommands("hello"), []);
  assert.deepEqual(filterAiSlashCommands(" /new"), []);
  assert.deepEqual(filterAiSlashCommands("/new\nhello"), []);
  assert.deepEqual(filterAiSlashCommands("/m\r\nx"), []);
  assert.deepEqual(filterAiSlashCommands("/new extra"), []);
  assert.deepEqual(filterAiSlashCommands("/doesnotexist"), []);
});

test("formatAiHelpMarkdown renders clean Vietnamese markdown for ZeroClaw and Cloud modes", () => {
  const withZc = formatAiHelpMarkdown(true);
  assert.match(withZc, /\/new/);
  assert.match(withZc, /\/clear/);
  assert.match(withZc, /\/model/);
  assert.match(withZc, /\/help/);
  assert.match(withZc, /\/memory`/);
  assert.match(withZc, /\/memory clear/);
  assert.match(withZc, /\/skills/);
  assert.match(withZc, /\/status/);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(withZc), false);

  const zcBlocks = parseSimpleMarkdownBlocks(withZc);
  assert.ok(zcBlocks.some((b) => b.kind === "heading"));
  assert.ok(zcBlocks.some((b) => b.kind === "ul"));

  const withoutZc = formatAiHelpMarkdown(false);
  assert.match(withoutZc, /\/new/);
  assert.match(withoutZc, /\/clear/);
  assert.match(withoutZc, /\/model/);
  assert.match(withoutZc, /\/help/);
  assert.equal(withoutZc.includes("/memory"), false);
  assert.equal(withoutZc.includes("/skills"), false);
  assert.equal(withoutZc.includes("/status"), false);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(withoutZc), false);

  const cloudBlocks = parseSimpleMarkdownBlocks(withoutZc);
  assert.ok(cloudBlocks.some((b) => b.kind === "heading"));
  assert.ok(cloudBlocks.some((b) => b.kind === "ul"));
});

test("styles.css defines --bg-input, --text-muted, and --bg-elevated across all 4 themes with select option contrast", () => {
  const testDir = dirname(fileURLToPath(import.meta.url));
  const css = readFileSync(resolve(testDir, "../src/styles.css"), "utf8");
  const themes = ["triforce", "night-blue", "dark", "light"] as const;
  for (const theme of themes) {
    const startIdx = css.indexOf(`[data-theme="${theme}"]`);
    assert.ok(startIdx >= 0, `missing theme block for ${theme}`);
    const openBrace = css.indexOf("{", startIdx);
    const closeBrace = css.indexOf("}", openBrace);
    assert.ok(openBrace > startIdx && closeBrace > openBrace, `malformed theme block for ${theme}`);
    const block = css.slice(openBrace, closeBrace);
    assert.match(block, /--bg-input\s*:/, `missing --bg-input in ${theme}`);
    assert.match(block, /--text-muted\s*:/, `missing --text-muted in ${theme}`);
    assert.match(block, /--bg-elevated\s*:/, `missing --bg-elevated in ${theme}`);
    assert.match(block, /color-scheme\s*:/, `missing color-scheme in ${theme}`);
  }
  assert.match(css, /select\s+option\s*\{[^}]*background-color\s*:\s*var\(--bg-input\)/);
});
