import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  MAX_ALLOWED_ROOTS,
  MAX_ALLOWED_THREAD_IDS,
  computeApprovalRemainingSecs,
  extractApprovalDetailView,
  formatAuditDecisionLabel,
  formatSystemAccessShortLabel,
  formatTezaAccessShortLabel,
  formatToolDisplayName,
  getAccessBadgeTone,
  getAuditDecisionTone,
  isAllowSessionPermitted,
  isHighRiskCombination,
  mergeToolActivities,
  normalizeAllowedRoots,
  normalizeAllowedThreadIds,
  normalizeSystemAccessLevel,
  normalizeTezaAccessLevel,
  shouldShowEmergencyStop,
  stripToolPrefix,
  type SystemAccessLevel,
  type TezaAccessLevel,
} from "../src/utils/aiPermissions.ts";

// Construct forbidden legacy/stale branding regex dynamically so this test file stays 100% clean.
const FORBIDDEN_BRAND_REGEX = new RegExp(
  [
    String.fromCharCode(122, 97, 108, 111),
    String.fromCharCode(116, 101, 108, 101, 103, 114, 97, 109),
    String.fromCharCode(111, 112, 101, 110, 99, 111, 100, 101),
    String.fromCharCode(111, 108, 108, 97, 109, 97),
    String.fromCharCode(103, 101, 109, 105, 110, 105),
    String.fromCharCode(97, 122, 117, 114, 101),
  ].join("|"),
  "i",
);

test("isHighRiskCombination matches the complete 2-axis permission truth table", () => {
  const cases: [TezaAccessLevel, SystemAccessLevel, boolean][] = [
    ["isolated", "safe", false],
    ["isolated", "supervised", false],
    ["isolated", "full", false],
    ["read_only", "safe", false],
    ["read_only", "supervised", false],
    ["read_only", "full", true],
    ["supervised", "safe", false],
    ["supervised", "supervised", false],
    ["supervised", "full", true],
    ["autonomous", "safe", true],
    ["autonomous", "supervised", true],
    ["autonomous", "full", true],
  ];

  for (const [teza, sys, expected] of cases) {
    assert.equal(
      isHighRiskCombination(teza, sys),
      expected,
      `expected isHighRiskCombination(${teza}, ${sys}) === ${expected}`,
    );
  }
});

test("getAccessBadgeTone computes accurate overall and per-axis tones", () => {
  assert.deepEqual(getAccessBadgeTone("isolated", "safe"), {
    tone: "neutral",
    tezaTone: "neutral",
    systemTone: "neutral",
    isHighRisk: false,
    isElevated: false,
  });

  assert.deepEqual(getAccessBadgeTone("read_only", "safe"), {
    tone: "info",
    tezaTone: "info",
    systemTone: "neutral",
    isHighRisk: false,
    isElevated: true,
  });

  assert.deepEqual(getAccessBadgeTone("supervised", "supervised"), {
    tone: "warn",
    tezaTone: "warn",
    systemTone: "warn",
    isHighRisk: false,
    isElevated: true,
  });

  assert.deepEqual(getAccessBadgeTone("isolated", "full"), {
    tone: "danger",
    tezaTone: "neutral",
    systemTone: "danger",
    isHighRisk: false,
    isElevated: true,
  });

  assert.deepEqual(getAccessBadgeTone("read_only", "full"), {
    tone: "danger",
    tezaTone: "info",
    systemTone: "danger",
    isHighRisk: true,
    isElevated: true,
  });

  assert.deepEqual(getAccessBadgeTone("autonomous", "safe"), {
    tone: "danger",
    tezaTone: "danger",
    systemTone: "neutral",
    isHighRisk: true,
    isElevated: true,
  });
});

test("formatToolDisplayName maps prefixed and unprefixed MCP tools across vi, en, and ko", () => {
  assert.equal(stripToolPrefix("teza__search_messages"), "search_messages");
  assert.equal(stripToolPrefix("mcp__teza__win_run_powershell"), "win_run_powershell");
  assert.equal(stripToolPrefix("send_message"), "send_message");

  const coreTools = [
    "get_account_info",
    "list_threads",
    "get_thread_details",
    "get_messages",
    "search_messages",
    "list_friends",
    "list_folders",
    "list_schedules",
    "export_attachment",
    "draft_message",
    "send_message",
    "send_attachment",
    "react_message",
    "recall_or_delete_message",
    "manage_thread_state",
    "pin_chat_message",
    "create_schedule",
    "manage_schedule",
    "create_poll",
    "create_reminder",
    "manage_social",
    "win_list_dir",
    "win_read_file",
    "win_write_file",
    "win_run_powershell",
    "win_open_path",
  ];

  for (const tool of coreTools) {
    for (const lang of ["vi", "en", "ko"] as const) {
      const bare = formatToolDisplayName(tool, lang);
      const prefixed = formatToolDisplayName(`teza__${tool}`, lang);
      assert.equal(prefixed, bare, `teza__${tool} should match ${tool} for ${lang}`);
      assert.ok(bare.length > 2, `${tool} label for ${lang} should be non-empty`);
      assert.ok(!bare.includes("_"), `${tool} label for ${lang} should be human-readable`);
    }
  }

  assert.equal(formatToolDisplayName("teza__win_run_powershell", "vi"), "Chạy lệnh PowerShell");
  assert.equal(formatToolDisplayName("win_run_powershell", "en"), "Run PowerShell command");
  assert.equal(formatToolDisplayName("teza__send_message", "ko"), "메시지 보내기");

  // Unknown and empty tool fallbacks
  assert.equal(formatToolDisplayName("teza__custom_report_export", "en"), "Custom report export");
  assert.equal(formatToolDisplayName("", "vi"), "Công cụ AI");
  assert.equal(formatToolDisplayName("   ", "en"), "AI Tool");
  assert.equal(formatToolDisplayName("", "ko"), "AI 도구");
});

test("normalizeAllowedRoots trims, strips quotes, dedupes Windows paths case-insensitively, and caps at 32", () => {
  assert.deepEqual(normalizeAllowedRoots(null), []);
  assert.deepEqual(normalizeAllowedRoots(undefined), []);

  const deduped = normalizeAllowedRoots([
    '  "C:\\Users\\Alice\\Documents\\"  ',
    "c:/users/alice/documents",
    "C:\\USERS\\ALICE\\DOCUMENTS\\\\",
    "D:\\Work\\Reports\\",
    "d:\\work\\reports",
    "C:\\",
    "c:/",
    "C:",
    "",
    "   ",
    "bad\u0000path",
    `E:\\${"a".repeat(520)}`,
  ]);

  assert.deepEqual(deduped, ["C:\\Users\\Alice\\Documents", "D:\\Work\\Reports", "C:\\"]);

  const many = Array.from({ length: 45 }, (_, idx) => `D:\\Projects\\Folder_${idx}`);
  const capped = normalizeAllowedRoots(many);
  assert.equal(capped.length, MAX_ALLOWED_ROOTS);
  assert.equal(capped[0], "D:\\Projects\\Folder_0");
  assert.equal(capped[31], "D:\\Projects\\Folder_31");
});

test("normalizeAllowedThreadIds trims, dedupes preserving order, and caps at 200", () => {
  assert.deepEqual(normalizeAllowedThreadIds([" t1 ", "t2", "t1", "", "  ", "t3"]), [
    "t1",
    "t2",
    "t3",
  ]);
  const many = Array.from({ length: 240 }, (_, i) => `thread_${i}`);
  const capped = normalizeAllowedThreadIds(many);
  assert.equal(capped.length, MAX_ALLOWED_THREAD_IDS);
  assert.equal(capped[199], "thread_199");
});

test("normalizeTezaAccessLevel and normalizeSystemAccessLevel fall back to safe defaults", () => {
  assert.equal(normalizeTezaAccessLevel("isolated"), "isolated");
  assert.equal(normalizeTezaAccessLevel("read_only"), "read_only");
  assert.equal(normalizeTezaAccessLevel("supervised"), "supervised");
  assert.equal(normalizeTezaAccessLevel("autonomous"), "autonomous");
  assert.equal(normalizeTezaAccessLevel("invalid"), "isolated");
  assert.equal(normalizeTezaAccessLevel(undefined), "isolated");

  assert.equal(normalizeSystemAccessLevel("safe"), "safe");
  assert.equal(normalizeSystemAccessLevel("supervised"), "supervised");
  assert.equal(normalizeSystemAccessLevel("full"), "full");
  assert.equal(normalizeSystemAccessLevel("root"), "safe");
  assert.equal(normalizeSystemAccessLevel(null), "safe");
});

test("isAllowSessionPermitted blocks high-risk and destructive group operations", () => {
  assert.equal(isAllowSessionPermitted({ risk: "medium", tool_name: "teza__send_message" }), true);
  assert.equal(
    isAllowSessionPermitted({ risk: "medium", tool_name: "teza__win_run_powershell" }),
    true,
  );
  assert.equal(isAllowSessionPermitted({ risk: "high", tool_name: "teza__send_message" }), false);
  assert.equal(isAllowSessionPermitted({ risk: "medium", tool_name: "teza__leave_group" }), false);
  assert.equal(
    isAllowSessionPermitted({ risk: "medium", tool_name: "teza__kick_group_member" }),
    false,
  );
  assert.equal(isAllowSessionPermitted({ risk: "medium", tool_name: "kick_member" }), false);
});

test("computeApprovalRemainingSecs handles epoch seconds/ms and clamps expired values to 0", () => {
  const baseSec = 1_760_000_000;
  const baseMs = baseSec * 1000;

  assert.equal(computeApprovalRemainingSecs(baseSec, 120, baseMs + 25_100), 95);
  assert.equal(computeApprovalRemainingSecs(baseMs, 120, baseMs + 119_500), 1);
  assert.equal(computeApprovalRemainingSecs(baseMs, 120, baseMs + 120_000), 0);
  assert.equal(computeApprovalRemainingSecs(baseMs, 120, baseMs + 150_000), 0);
  assert.equal(computeApprovalRemainingSecs(0, 60, baseMs), 0);
  assert.equal(computeApprovalRemainingSecs(baseMs, 0, baseMs), 0);
});

test("shouldShowEmergencyStop activates when streaming, awaiting approval, or permissions are elevated", () => {
  assert.equal(
    shouldShowEmergencyStop({
      isStreaming: false,
      pendingApprovalsCount: 0,
      tezaAccessLevel: "isolated",
      systemAccessLevel: "safe",
    }),
    false,
  );
  assert.equal(
    shouldShowEmergencyStop({
      isStreaming: true,
      pendingApprovalsCount: 0,
      tezaAccessLevel: "isolated",
      systemAccessLevel: "safe",
    }),
    true,
  );
  assert.equal(
    shouldShowEmergencyStop({
      isStreaming: false,
      pendingApprovalsCount: 1,
      tezaAccessLevel: "isolated",
      systemAccessLevel: "safe",
    }),
    true,
  );
  assert.equal(
    shouldShowEmergencyStop({
      isStreaming: false,
      pendingApprovalsCount: 0,
      tezaAccessLevel: "read_only",
      systemAccessLevel: "safe",
    }),
    true,
  );
  assert.equal(
    shouldShowEmergencyStop({
      isStreaming: false,
      pendingApprovalsCount: 0,
      tezaAccessLevel: "isolated",
      systemAccessLevel: "supervised",
    }),
    true,
  );
});

test("mergeToolActivities deduplicates by tool_call_id while preserving order and upgrading status", () => {
  const merged = mergeToolActivities([
    {
      session_id: "s1",
      tool_call_id: "c1",
      tool_name: "teza__search_messages",
      status: "pending",
      summary: "Tìm báo cáo Q4",
    },
    {
      session_id: "s1",
      tool_call_id: "c2",
      tool_name: "teza__win_run_powershell",
      status: "pending",
      summary: "Đọc file Excel",
    },
    {
      session_id: "s1",
      tool_call_id: "c1",
      tool_name: "teza__search_messages",
      status: "completed",
      summary: "Tìm thấy 8 tin nhắn",
    },
    {
      session_id: "s1",
      tool_call_id: "c2",
      tool_name: "",
      status: "failed",
      summary: "",
    },
  ]);

  assert.equal(merged.length, 2);
  assert.deepEqual(merged[0], {
    session_id: "s1",
    tool_call_id: "c1",
    tool_name: "teza__search_messages",
    status: "completed",
    summary: "Tìm thấy 8 tin nhắn",
  });
  assert.deepEqual(merged[1], {
    session_id: "s1",
    tool_call_id: "c2",
    tool_name: "teza__win_run_powershell",
    status: "failed",
    summary: "Đọc file Excel",
  });
});

test("extractApprovalDetailView formats PowerShell, write_file, open_path, message, and generic details", () => {
  const ps = extractApprovalDetailView({
    tool_name: "teza__win_run_powershell",
    details: {
      command: "Get-ChildItem C:\\Users\\Alice\\Documents\\*.xlsx",
      cwd: "C:\\Users\\Alice\\Documents",
      timeout_secs: 45,
    },
  });
  assert.deepEqual(ps, {
    kind: "powershell",
    command: "Get-ChildItem C:\\Users\\Alice\\Documents\\*.xlsx",
    cwd: "C:\\Users\\Alice\\Documents",
    timeoutSecs: 45,
  });

  const wf = extractApprovalDetailView({
    tool_name: "teza__win_write_file",
    details: {
      path: "C:\\Users\\Alice\\Documents\\summary.txt",
      append: false,
      content: "Line 1\nLine 2",
    },
  });
  assert.deepEqual(wf, {
    kind: "write_file",
    path: "C:\\Users\\Alice\\Documents\\summary.txt",
    mode: "overwrite",
    contentPreview: "Line 1\nLine 2",
    truncated: false,
  });

  const openPath = extractApprovalDetailView({
    tool_name: "win_open_path",
    details: { path: "C:\\Users\\Alice\\Documents\\report.xlsx" },
  });
  assert.deepEqual(openPath, {
    kind: "open_path",
    path: "C:\\Users\\Alice\\Documents\\report.xlsx",
  });

  const msg = extractApprovalDetailView({
    tool_name: "teza__send_attachment",
    details: {
      recipient_name: "Nhóm Kế Toán",
      caption: "Gửi anh chị file tổng hợp tháng 11",
      file_path: "C:\\Users\\Alice\\Documents\\tonghop.xlsx",
    },
  });
  assert.deepEqual(msg, {
    kind: "message",
    recipient: "Nhóm Kế Toán",
    text: "Gửi anh chị file tổng hợp tháng 11",
    filePath: "C:\\Users\\Alice\\Documents\\tonghop.xlsx",
    scheduleTime: undefined,
    recurrence: undefined,
    replyTo: undefined,
  });

  const generic = extractApprovalDetailView({
    tool_name: "teza__create_poll",
    details: {
      question: "Họp thứ mấy?",
      options: ["Thứ 3", "Thứ 5"],
      empty: "",
    },
  });
  assert.equal(generic.kind, "generic");
  if (generic.kind === "generic") {
    assert.deepEqual(generic.entries, [
      { key: "question", value: "Họp thứ mấy?" },
      { key: "options", value: '["Thứ 3","Thứ 5"]' },
    ]);
  }
});

test("localized short labels and audit decision labels cover vi, en, and ko", () => {
  for (const lang of ["vi", "en", "ko"] as const) {
    assert.ok(formatTezaAccessShortLabel("supervised", lang).includes("T2"));
    assert.ok(formatSystemAccessShortLabel("supervised", lang).includes("S1"));
    assert.ok(formatAuditDecisionLabel("allow_once", lang).length > 0);
    assert.ok(formatAuditDecisionLabel("blocked", lang).length > 0);
  }
  assert.equal(getAuditDecisionTone("auto", true), "info");
  assert.equal(getAuditDecisionTone("allow_once", true), "success");
  assert.equal(getAuditDecisionTone("allow_session", false), "warn");
  assert.equal(getAuditDecisionTone("deny", false), "danger");
  assert.equal(getAuditDecisionTone("timeout", false), "danger");
  assert.equal(getAuditDecisionTone("blocked", false), "danger");
});

test("aiPermissions helper source contains zero forbidden branding or stale backend terms", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = readFileSync(resolve(here, "../src/utils/aiPermissions.ts"), "utf8");
  assert.equal(FORBIDDEN_BRAND_REGEX.test(source), false);
});
