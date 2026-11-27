import type { MessageDto } from "../types.ts";

export type BlockedStickerKind = "sticker" | "category";
export type BlockedStickerDisplayMode = "collapse" | "hide";

export interface BlockedStickerRule {
  id: string;
  kind: BlockedStickerKind;
  value: string;
  scopeThreadId: string | null;
  scopeThreadTitle?: string;
  previewUrl?: string;
  label?: string;
  createdAt: number;
}

export interface BlockedStickersConfig {
  mode: BlockedStickerDisplayMode;
  rules: BlockedStickerRule[];
}

export interface StickerIdentity {
  isSticker: boolean;
  stickerId: string | null;
  cateId: string | null;
  fallbackUrl: string | null;
}

export interface AddBlockedStickerRuleInput {
  kind: BlockedStickerKind;
  value: string;
  scopeThreadId: string | null;
  scopeThreadTitle?: string;
  previewUrl?: string;
  label?: string;
  createdAt?: number;
  id?: string;
}

export const BLOCKED_STICKERS_STORAGE_KEY = "teza.blockedStickers.v1";
export const MAX_BLOCKED_STICKER_RULES = 500;

export const DEFAULT_BLOCKED_STICKERS_CONFIG: BlockedStickersConfig = {
  mode: "collapse",
  rules: [],
};

const VALID_DISPLAY_MODES = new Set<BlockedStickerDisplayMode>(["collapse", "hide"]);
const VALID_RULE_KINDS = new Set<BlockedStickerKind>(["sticker", "category"]);
const STICKER_FILENAME_RE = /^sticker_(\d+)_(\d+)(?:\.[a-z0-9]+)?$/i;
const POSITIVE_INT_RE = /^[1-9]\d*$/;
const DIGITS_RE = /^\d+$/;

export function isStickerMsgType(msgType: string | null | undefined): boolean {
  const t = (msgType ?? "").trim().toLowerCase();
  return Boolean(t) && (t.includes("sticker") || t === "5" || t === "35" || t === "36");
}

function parsePositiveIntString(raw: string | null | undefined): string | null {
  const trimmed = (raw ?? "").trim();
  if (!trimmed || !DIGITS_RE.test(trimmed)) return null;
  const num = Number(trimmed);
  if (!Number.isSafeInteger(num) || num <= 0) return null;
  return String(num);
}

/**
 * Normalizes a sticker image URL for deterministic fallback matching and persistence:
 * - Accepts `http://` and `https://` URLs (ignoring ephemeral `blob:` and huge `data:` URIs).
 * - Lowercases scheme + host via `URL`, strips `#fragment`, and trims whitespace.
 */
export function normalizeStickerFallbackUrl(raw: string | null | undefined): string | null {
  const trimmed = (raw ?? "").trim();
  if (!trimmed || trimmed.length > 1024) return null;
  if (/^(?:blob|data):/i.test(trimmed)) return null;
  if (/^https?:\/\//i.test(trimmed)) {
    try {
      const parsed = new URL(trimmed);
      parsed.hash = "";
      return parsed.toString();
    } catch {
      return trimmed.split("#")[0]?.trim() || null;
    }
  }
  return null;
}

/**
 * Extracts stable sticker identity (`stickerId`, `cateId`, normalized `fallbackUrl`)
 * from a chat message's `msg_type`, `attachment_name`, and `attachment_url`.
 *
 * Mirrors the desktop parser conventions:
 * - `attachment_name`: `sticker_{cateId}_{stickerId}.{ext}` (`cateId = 0` means unknown/default pack -> `null`)
 * - `attachment_url`: `?eid={stickerId}` (plus optional `cateId`/`catId`), or `/thumb/{stickerId}.{ext}`
 */
export function extractStickerIdentity(
  msg: Pick<MessageDto, "msg_type" | "attachment_name" | "attachment_url">,
): StickerIdentity {
  if (!isStickerMsgType(msg.msg_type)) {
    return {
      isSticker: false,
      stickerId: null,
      cateId: null,
      fallbackUrl: null,
    };
  }

  let stickerId: string | null = null;
  let cateId: string | null = null;

  const rawName = (msg.attachment_name ?? "").trim();
  if (rawName) {
    const m = STICKER_FILENAME_RE.exec(rawName);
    if (m) {
      const parsedCid = parsePositiveIntString(m[1]);
      const parsedSid = parsePositiveIntString(m[2]);
      if (parsedSid) {
        stickerId = parsedSid;
      }
      if (parsedCid) {
        cateId = parsedCid;
      }
    }
  }

  const rawUrl = (msg.attachment_url ?? "").trim();
  const fallbackUrl = normalizeStickerFallbackUrl(rawUrl);

  if (rawUrl) {
    const noHash = rawUrl.split("#")[0] ?? rawUrl;
    const qIdx = noHash.indexOf("?");
    if (qIdx >= 0) {
      const queryPart = noHash.slice(qIdx + 1);
      for (const pair of queryPart.split("&")) {
        const eqIdx = pair.indexOf("=");
        if (eqIdx < 0) continue;
        const key = pair.slice(0, eqIdx).trim().toLowerCase();
        const val = pair.slice(eqIdx + 1).trim();
        if (!stickerId && key === "eid") {
          const parsedSid = parsePositiveIntString(val);
          if (parsedSid) stickerId = parsedSid;
        } else if (!cateId && (key === "cateid" || key === "catid")) {
          const parsedCid = parsePositiveIntString(val);
          if (parsedCid) cateId = parsedCid;
        }
      }
    }

    if (!stickerId) {
      const pathOnly = qIdx >= 0 ? noHash.slice(0, qIdx) : noHash;
      const thumbMarker = "/thumb/";
      const thumbIdx = pathOnly.toLowerCase().lastIndexOf(thumbMarker);
      if (thumbIdx >= 0) {
        const afterThumb = pathOnly.slice(thumbIdx + thumbMarker.length);
        if (afterThumb && !afterThumb.includes("/")) {
          const stem = afterThumb.split(".")[0] ?? "";
          const parsedSid = parsePositiveIntString(stem);
          if (parsedSid) stickerId = parsedSid;
        }
      }
    }
  }

  return {
    isSticker: true,
    stickerId,
    cateId,
    fallbackUrl,
  };
}

function normalizeRuleValue(kind: BlockedStickerKind, rawValue: string): string | null {
  const trimmed = rawValue.trim();
  if (!trimmed) return null;
  if (kind === "category") {
    return parsePositiveIntString(trimmed);
  }
  const positiveId = parsePositiveIntString(trimmed);
  if (positiveId) return positiveId;
  return normalizeStickerFallbackUrl(trimmed);
}

function normalizeScopeThreadId(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  return trimmed ? trimmed : null;
}

export function getBlockedStickerRuleDedupeKey(
  rule: Pick<BlockedStickerRule, "scopeThreadId" | "kind" | "value">,
): string {
  return `${rule.scopeThreadId ?? "*"}::${rule.kind}::${rule.value}`;
}

/**
 * Matches a **received**, non-recalled sticker message against the user's local
 * blocked sticker rules (checking both global `scopeThreadId === null` and
 * thread-scoped `scopeThreadId === msg.thread_id` rules).
 */
export function matchBlockedStickerRule(
  msg: Pick<
    MessageDto,
    "thread_id" | "msg_type" | "attachment_name" | "attachment_url" | "is_outgoing" | "is_recalled"
  >,
  rules: readonly BlockedStickerRule[],
): BlockedStickerRule | null {
  if (msg.is_outgoing || msg.is_recalled) return null;
  if (!rules || rules.length === 0) return null;

  const identity = extractStickerIdentity(msg);
  if (!identity.isSticker) return null;
  if (!identity.stickerId && !identity.cateId && !identity.fallbackUrl) return null;

  for (const rule of rules) {
    if (rule.scopeThreadId !== null && rule.scopeThreadId !== msg.thread_id) {
      continue;
    }
    if (rule.kind === "category") {
      if (identity.cateId !== null && rule.value === identity.cateId) {
        return rule;
      }
      continue;
    }
    if (rule.kind === "sticker") {
      if (identity.stickerId !== null && rule.value === identity.stickerId) {
        return rule;
      }
      if (identity.fallbackUrl !== null) {
        if (
          rule.value === identity.fallbackUrl ||
          (!POSITIVE_INT_RE.test(rule.value) &&
            normalizeStickerFallbackUrl(rule.value) === identity.fallbackUrl)
        ) {
          return rule;
        }
      }
    }
  }
  return null;
}

/**
 * Builds an `AddBlockedStickerRuleInput` from a received sticker message for one-click
 * context menu blocking. Returns `null` if the message is not a blockable sticker
 * (or has no `cateId` when `opts.kind === "category"`).
 */
export function buildBlockedStickerRuleFromMessage(
  msg: Pick<MessageDto, "thread_id" | "msg_type" | "attachment_name" | "attachment_url">,
  opts: {
    kind: BlockedStickerKind;
    global: boolean;
    threadTitle?: string;
  },
): AddBlockedStickerRuleInput | null {
  const identity = extractStickerIdentity(msg);
  if (!identity.isSticker) return null;

  const value =
    opts.kind === "category"
      ? identity.cateId
      : (identity.stickerId ?? identity.fallbackUrl);
  if (!value) return null;

  const scopeThreadId = opts.global ? null : normalizeScopeThreadId(msg.thread_id);
  const scopeThreadTitle =
    !opts.global && opts.threadTitle?.trim() ? opts.threadTitle.trim() : undefined;

  return {
    kind: opts.kind,
    value,
    scopeThreadId,
    scopeThreadTitle,
    previewUrl: identity.fallbackUrl ?? undefined,
    label:
      opts.kind === "category"
        ? `#${value}`
        : identity.stickerId
          ? `#${identity.stickerId}`
          : undefined,
  };
}

function generateRuleId(createdAt: number): string {
  return `bsr-${createdAt}-${Math.random().toString(36).slice(2, 8)}`;
}

export function addBlockedStickerRule(
  config: BlockedStickersConfig,
  input: AddBlockedStickerRuleInput,
): BlockedStickersConfig {
  if (!VALID_RULE_KINDS.has(input.kind)) return config;
  const normalizedValue = normalizeRuleValue(input.kind, input.value);
  if (!normalizedValue) return config;

  const scopeThreadId = normalizeScopeThreadId(input.scopeThreadId);
  const dedupeKey = getBlockedStickerRuleDedupeKey({
    scopeThreadId,
    kind: input.kind,
    value: normalizedValue,
  });

  const cleanPreview = normalizeStickerFallbackUrl(input.previewUrl) ?? undefined;
  const cleanTitle = input.scopeThreadTitle?.trim() || undefined;
  const cleanLabel = input.label?.trim() || undefined;

  const existingIdx = config.rules.findIndex(
    (r) => getBlockedStickerRuleDedupeKey(r) === dedupeKey,
  );
  if (existingIdx >= 0) {
    const existing = config.rules[existingIdx];
    const merged: BlockedStickerRule = {
      ...existing,
      scopeThreadTitle: existing.scopeThreadTitle ?? cleanTitle,
      previewUrl: existing.previewUrl ?? cleanPreview,
      label: existing.label ?? cleanLabel,
    };
    if (
      merged.scopeThreadTitle === existing.scopeThreadTitle &&
      merged.previewUrl === existing.previewUrl &&
      merged.label === existing.label
    ) {
      return config;
    }
    const nextRules = [...config.rules];
    nextRules[existingIdx] = merged;
    return { ...config, rules: nextRules };
  }

  const createdAt =
    typeof input.createdAt === "number" && Number.isFinite(input.createdAt)
      ? Math.round(input.createdAt)
      : Date.now();
  const id = input.id?.trim() || generateRuleId(createdAt);

  const nextRule: BlockedStickerRule = {
    id,
    kind: input.kind,
    value: normalizedValue,
    scopeThreadId,
    ...(cleanTitle ? { scopeThreadTitle: cleanTitle } : {}),
    ...(cleanPreview ? { previewUrl: cleanPreview } : {}),
    ...(cleanLabel ? { label: cleanLabel } : {}),
    createdAt,
  };

  const nextRules = [nextRule, ...config.rules].slice(0, MAX_BLOCKED_STICKER_RULES);
  return { ...config, rules: nextRules };
}

export function removeBlockedStickerRule(
  config: BlockedStickersConfig,
  ruleId: string,
): BlockedStickersConfig {
  const cleanId = ruleId.trim();
  if (!cleanId) return config;
  const nextRules = config.rules.filter((r) => r.id !== cleanId);
  if (nextRules.length === config.rules.length) return config;
  return { ...config, rules: nextRules };
}

export function setBlockedStickerDisplayMode(
  config: BlockedStickersConfig,
  mode: BlockedStickerDisplayMode,
): BlockedStickersConfig {
  if (!VALID_DISPLAY_MODES.has(mode) || config.mode === mode) return config;
  return { ...config, mode };
}

export function normalizeBlockedStickersConfig(raw: unknown): BlockedStickersConfig {
  if (!raw || typeof raw !== "object") {
    return { mode: DEFAULT_BLOCKED_STICKERS_CONFIG.mode, rules: [] };
  }
  const obj = raw as Partial<BlockedStickersConfig>;
  const mode: BlockedStickerDisplayMode =
    obj.mode && VALID_DISPLAY_MODES.has(obj.mode)
      ? obj.mode
      : DEFAULT_BLOCKED_STICKERS_CONFIG.mode;

  const rules: BlockedStickerRule[] = [];
  const seenKeys = new Set<string>();
  const seenIds = new Set<string>();

  if (Array.isArray(obj.rules)) {
    for (let i = 0; i < obj.rules.length; i += 1) {
      if (rules.length >= MAX_BLOCKED_STICKER_RULES) break;
      const item = obj.rules[i];
      if (!item || typeof item !== "object") continue;
      const r = item as Partial<BlockedStickerRule>;
      if (!r.kind || !VALID_RULE_KINDS.has(r.kind)) continue;
      if (typeof r.value !== "string") continue;
      const value = normalizeRuleValue(r.kind, r.value);
      if (!value) continue;

      const scopeThreadId = normalizeScopeThreadId(r.scopeThreadId);
      const dedupeKey = getBlockedStickerRuleDedupeKey({
        scopeThreadId,
        kind: r.kind,
        value,
      });
      if (seenKeys.has(dedupeKey)) continue;
      seenKeys.add(dedupeKey);

      const createdAt =
        typeof r.createdAt === "number" && Number.isFinite(r.createdAt)
          ? Math.round(r.createdAt)
          : 0;
      let id = typeof r.id === "string" && r.id.trim() ? r.id.trim() : `bsr-restored-${i}`;
      while (seenIds.has(id)) {
        id = `${id}-${i}`;
      }
      seenIds.add(id);

      const scopeThreadTitle =
        typeof r.scopeThreadTitle === "string" && r.scopeThreadTitle.trim()
          ? r.scopeThreadTitle.trim()
          : undefined;
      const previewUrl =
        typeof r.previewUrl === "string"
          ? (normalizeStickerFallbackUrl(r.previewUrl) ?? undefined)
          : undefined;
      const label =
        typeof r.label === "string" && r.label.trim() ? r.label.trim() : undefined;

      rules.push({
        id,
        kind: r.kind,
        value,
        scopeThreadId,
        ...(scopeThreadTitle ? { scopeThreadTitle } : {}),
        ...(previewUrl ? { previewUrl } : {}),
        ...(label ? { label } : {}),
        createdAt,
      });
    }
  }

  return { mode, rules };
}

export function loadBlockedStickersConfig(
  storage?: Pick<Storage, "getItem"> | null,
): BlockedStickersConfig {
  const store =
    storage !== undefined
      ? storage
      : typeof localStorage !== "undefined"
        ? localStorage
        : null;
  if (!store) {
    return { mode: DEFAULT_BLOCKED_STICKERS_CONFIG.mode, rules: [] };
  }
  try {
    const raw = store.getItem(BLOCKED_STICKERS_STORAGE_KEY);
    if (!raw) {
      return { mode: DEFAULT_BLOCKED_STICKERS_CONFIG.mode, rules: [] };
    }
    return normalizeBlockedStickersConfig(JSON.parse(raw));
  } catch {
    return { mode: DEFAULT_BLOCKED_STICKERS_CONFIG.mode, rules: [] };
  }
}

export function saveBlockedStickersConfig(
  config: BlockedStickersConfig,
  storage?: Pick<Storage, "setItem"> | null,
): void {
  const store =
    storage !== undefined
      ? storage
      : typeof localStorage !== "undefined"
        ? localStorage
        : null;
  if (!store) return;
  try {
    const clean = normalizeBlockedStickersConfig(config);
    store.setItem(BLOCKED_STICKERS_STORAGE_KEY, JSON.stringify(clean));
  } catch {
    // Ignore storage quota or private-mode errors
  }
}
