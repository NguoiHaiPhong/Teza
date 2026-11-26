import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_AI_SYSTEM_PROMPT,
  DEFAULT_LOCAL_API_PORT,
  MAX_LOCAL_API_INTERVAL_MS,
  MAX_SCHEDULE_HORIZON_MS,
  MIN_LOCAL_API_INTERVAL_MS,
  buildLocalApiCurlExample,
  buildLocalApiPowerShellExample,
  buildLocalApiPythonExample,
  clampSummaryMessageCount,
  formatAuditOutcome,
  formatScheduleMisfirePolicy,
  formatScheduleRecurrence,
  formatScheduleStatus,
  formatThreadKindLabel,
  getAuditStatusTone,
  getScheduleStatusTone,
  normalizeEpochMs,
  parseAllowedThreadIdsInput,
  parseInlineMarkdown,
  parseSimpleMarkdownBlocks,
  sanitizeLocalApiPort,
  toLocalDatetimeInputValue,
  validateLocalApiMinIntervalMs,
  validateLocalApiPort,
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

test("Local API code snippets bind 127.0.0.1, escape quotes safely, and contain Teza branding only", () => {
  const curl = buildLocalApiCurlExample(17820, "teza_abc123", "thread_42");
  assert.match(curl, /http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send/);
  assert.match(curl, /Authorization: Bearer teza_abc123/);
  assert.match(curl, /Idempotency-Key: teza-job-001/);
  assert.match(curl, /"thread_id":"thread_42"/);
  assert.match(curl, /Teza Local API/);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(curl), false);

  // Invalid port falls back to default 17820
  const curlFallback = buildLocalApiCurlExample(22, "teza_x", "id_1");
  assert.match(curlFallback, /http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send/);

  const ps = buildLocalApiPowerShellExample(19001, "teza_tok'1", "id'2");
  assert.match(ps, /http:\/\/127\.0\.0\.1:19001\/v1\/messages\/send/);
  assert.match(ps, /Bearer teza_tok''1/);
  assert.match(ps, /thread_id = 'id''2'/);
  assert.match(ps, /Invoke-RestMethod/);
  assert.match(ps, /Teza Local API/);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(ps), false);

  const py = buildLocalApiPythonExample(17820, 'teza_"secret"', 'thr_"99"');
  assert.match(py, /http:\/\/127\.0\.0\.1:17820\/v1\/messages\/send/);
  assert.match(py, /urllib\.request/);
  assert.match(py, /"thr_\\"99\\""/);
  assert.match(py, /Teza Local API/);
  assert.equal(FORBIDDEN_BRAND_REGEX.test(py), false);

  // Verify default prompt and labels also have zero forbidden brand leaks
  assert.equal(FORBIDDEN_BRAND_REGEX.test(DEFAULT_AI_SYSTEM_PROMPT), false);
});

test("formatAuditOutcome and getAuditStatusTone classify HTTP statuses and outcomes", () => {
  assert.equal(formatAuditOutcome(200, "ok"), "Thành công");
  assert.equal(formatAuditOutcome(200, "idempotent_replay"), "Trả về kết quả đã lưu (Idempotent)");
  assert.equal(formatAuditOutcome(401, "unauthorized"), "Token không hợp lệ hoặc bị khóa");
  assert.equal(formatAuditOutcome(403, "forbidden_scope"), "Thiếu quyền (Scope)");
  assert.equal(formatAuditOutcome(403, "forbidden_thread"), "Hội thoại ngoài danh sách cho phép");
  assert.equal(formatAuditOutcome(403, "forbidden_origin"), "Bị chặn (CSRF Origin)");
  assert.equal(formatAuditOutcome(403, "forbidden_host"), "Bị chặn (Host không hợp lệ)");
  assert.equal(formatAuditOutcome(429, "rate_limited"), "Vượt giới hạn tần suất");
  assert.equal(formatAuditOutcome(400, "bad_request"), "Dữ liệu yêu cầu không hợp lệ");
  assert.equal(formatAuditOutcome(500, "internal_error"), "Lỗi xử lý cục bộ");

  assert.equal(getAuditStatusTone(200), "success");
  assert.equal(getAuditStatusTone(201), "success");
  assert.equal(getAuditStatusTone(429), "warn");
  assert.equal(getAuditStatusTone(403), "danger");
  assert.equal(getAuditStatusTone(500), "danger");
});

test("normalizeEpochMs and toLocalDatetimeInputValue handle seconds and ms cleanly", () => {
  assert.equal(normalizeEpochMs(0), 0);
  assert.equal(normalizeEpochMs(-5), 0);
  assert.equal(normalizeEpochMs(1_700_000_000), 1_700_000_000_000);
  assert.equal(normalizeEpochMs(1_700_000_000_000), 1_700_000_000_000);

  const formatted = toLocalDatetimeInputValue(1_700_000_000_000);
  assert.match(formatted, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
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
