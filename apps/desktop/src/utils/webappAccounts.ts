import type { WebAppAccountDto, WebAppAccountInput } from "../types.ts";
import { normalizeWebUrl } from "./webApps.ts";

export function extractWebAppOrigin(rawUrl: string): string {
  const normalized = normalizeWebUrl(rawUrl);
  if (!normalized) return "";
  try {
    const parsed = new URL(normalized);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return "";
    return parsed.origin;
  } catch {
    return "";
  }
}

export function resolveWebAppAccountLabel(
  label: string,
  username: string,
  appName: string,
): string {
  const cleanLabel = label.trim();
  if (cleanLabel) return cleanLabel;
  const cleanUser = username.trim();
  if (cleanUser) return cleanUser;
  const cleanApp = appName.trim();
  return cleanApp || "Account";
}

export interface BuildAccountInputParams {
  editingAccount: WebAppAccountDto | null;
  appId: string;
  appUrl: string;
  appName: string;
  label: string;
  username: string;
  password: string;
  clearPassword?: boolean;
  autoFill: boolean;
}

export function buildWebAppAccountInput(params: BuildAccountInputParams): WebAppAccountInput {
  const normalizedUrl = normalizeWebUrl(params.appUrl) || params.appUrl.trim();
  const clearPassword = Boolean(params.clearPassword);
  const password = clearPassword
    ? null
    : params.password.length > 0
      ? params.password
      : null;

  return {
    id: params.editingAccount ? params.editingAccount.id : null,
    app_id: params.appId.trim(),
    url: normalizedUrl,
    label: resolveWebAppAccountLabel(params.label, params.username, params.appName),
    username: params.username.trim(),
    password,
    clear_password: clearPassword,
    auto_fill: params.autoFill,
  };
}

export function isWebAppOverlaySuspended(
  externalSuspended: boolean,
  accountsOpen: boolean,
): boolean {
  return externalSuspended || accountsOpen;
}

export function sortWebAppAccountsForDisplay(
  accounts: readonly WebAppAccountDto[],
): WebAppAccountDto[] {
  return [...accounts].sort(
    (a, b) =>
      Number(b.auto_fill) - Number(a.auto_fill) ||
      b.updated_at_ms - a.updated_at_ms ||
      a.label.localeCompare(b.label),
  );
}

export function filterDemoWebAppAccounts(
  accounts: readonly WebAppAccountDto[],
  appId?: string,
  origin?: string,
): WebAppAccountDto[] {
  const cleanAppId = appId?.trim() || "";
  const cleanOrigin = origin?.trim() ? extractWebAppOrigin(origin) || origin.trim().toLowerCase() : "";

  return [...accounts]
    .filter((acc) => {
      if (cleanAppId && acc.app_id !== cleanAppId) return false;
      if (cleanOrigin && acc.origin !== cleanOrigin) return false;
      return true;
    })
    .sort((a, b) => b.updated_at_ms - a.updated_at_ms || a.id.localeCompare(b.id));
}

const MAX_APP_ID_LEN = 256;
const MAX_LABEL_LEN = 256;
const MAX_USERNAME_LEN = 512;
const MAX_PASSWORD_LEN = 2048;
const MAX_WEBAPP_ACCOUNTS = 200;

function charCount(value: string): number {
  return Array.from(value).length;
}

export function upsertDemoWebAppAccount(
  existing: readonly WebAppAccountDto[],
  input: WebAppAccountInput,
  nowMs = Date.now(),
): { next: WebAppAccountDto[]; saved: WebAppAccountDto } {
  const appId = input.app_id.trim();
  if (!appId) {
    throw new Error("Web App ID is required");
  }
  if (charCount(appId) > MAX_APP_ID_LEN) {
    throw new Error("Web App ID exceeds maximum length");
  }

  const origin = extractWebAppOrigin(input.url);
  if (!origin) {
    throw new Error("Invalid Web App URL");
  }

  const username = input.username.trim();
  if (!username) {
    throw new Error("Username is required");
  }
  if (charCount(username) > MAX_USERNAME_LEN) {
    throw new Error("Username exceeds maximum length");
  }

  const label = input.label.trim() || username;
  if (charCount(label) > MAX_LABEL_LEN) {
    throw new Error("Label exceeds maximum length");
  }

  if (typeof input.password === "string" && charCount(input.password) > MAX_PASSWORD_LEN) {
    throw new Error("Password exceeds maximum length");
  }

  const requestedId = input.id?.trim() || "";
  let targetId: string;
  let prev: WebAppAccountDto | null = null;
  if (requestedId) {
    prev = existing.find((a) => a.id === requestedId) ?? null;
    if (!prev) {
      throw new Error("Account not found");
    }
    targetId = requestedId;
  } else {
    if (existing.length >= MAX_WEBAPP_ACCOUNTS) {
      throw new Error("Maximum number of saved Web App accounts reached");
    }
    let candidate = `wa-acc-${nowMs}-${Math.random().toString(36).slice(2, 7)}`;
    let suffix = 0;
    while (existing.some((a) => a.id === candidate)) {
      suffix += 1;
      candidate = `wa-acc-${nowMs}-${suffix}`;
    }
    targetId = candidate;
  }

  const hasNewPassword = typeof input.password === "string" && input.password.length > 0;
  if (
    prev &&
    prev.origin !== origin &&
    prev.has_password &&
    !input.clear_password &&
    !hasNewPassword
  ) {
    throw new Error("Changing account origin requires entering a new password or clearing the saved password");
  }

  let hasPassword = prev?.has_password ?? false;
  if (input.clear_password) {
    hasPassword = false;
  } else if (hasNewPassword) {
    hasPassword = true;
  }

  const saved: WebAppAccountDto = {
    id: targetId,
    app_id: appId,
    origin,
    label,
    username,
    has_password: hasPassword,
    auto_fill: Boolean(input.auto_fill),
    updated_at_ms: nowMs,
  };

  const withoutTarget = existing
    .filter((a) => a.id !== targetId)
    .map((a) =>
      saved.auto_fill && a.app_id === saved.app_id && a.origin === saved.origin
        ? { ...a, auto_fill: false }
        : { ...a },
    );

  const next = [saved, ...withoutTarget].sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  return { next, saved };
}
